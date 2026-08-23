//! Shared plumbing of the `warble` subcommands: the modem presets and
//! per-knob overrides (`--preset`/`--baud`/`--mark`/`--space`/`--fx25`/
//! `--il2p`), address parsing/formatting, WAV-header validation,
//! raw-PCM sample iteration, and the FX.25/IL2P transmit wrappers used
//! by both `encode` and `gen`.

use clap::{Args, ValueEnum};

use warble::ax25::Address;
use warble::fx25::{WRAP_MAX, byte_bits, stuff_frame, wrap};
use warble::il2p::{self, ENCODED_MAX, Il2pParity};
use warble::modulator::{Modulator, ModulatorConfig};
use warble::nrzi;
use warble::tnc::{MAX_FRAME_BYTES, TncConfig, TncTransmitter};
use warble::wav::{SniffedPcm, WavError, sniff_pcm};
use warble::{BaudRate, ModemProfile, SampleRate, TonePair};

/// The named modem presets (baud rate + mark/space tone pair).
///
/// These are *mode* presets — they name a dialect on the air and map
/// 1:1 onto the library's [`ModemProfile`] constants. They are distinct
/// from the library's [`warble::DevicePreset`], which names a target
/// *chip* (ESP32-C3/C6/H2/P4) and resolves to a full `TncConfig` sized
/// to that chip's CPU budget; a device preset is not meaningful as a
/// CLI flag on a desktop host, so the two are not merged.
#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum Preset {
    /// Bell 202, the VHF APRS standard: 1200 Bd, 1200/2200 Hz.
    #[value(name = "bell202")]
    Bell202,
    /// HF APRS convention: 300 Bd, 1600/1800 Hz.
    #[value(name = "hf300")]
    Hf300,
    /// Bell 103 originate side: 300 Bd, 1270/1070 Hz.
    #[value(name = "bell103")]
    Bell103,
    /// Bell 103 answer side: 300 Bd, 2225/2025 Hz.
    #[value(name = "bell103-answer")]
    Bell103Answer,
    /// G3RUH 9600-baud packet: scrambled direct-baseband FSK (no
    /// audio tones; --mark/--space/--baud do not apply).
    #[cfg(feature = "g3ruh")]
    #[value(name = "g3ruh", alias = "g3ruh-9600")]
    G3ruh,
}

impl Preset {
    /// The library profile constant behind the preset name.
    fn profile(self) -> ModemProfile {
        match self {
            Preset::Bell202 => ModemProfile::BELL_202,
            Preset::Hf300 => ModemProfile::HF_APRS_300,
            Preset::Bell103 => ModemProfile::BELL_103,
            Preset::Bell103Answer => ModemProfile::BELL_103_ANSWER,
            #[cfg(feature = "g3ruh")]
            Preset::G3ruh => ModemProfile::G3RUH_9600,
        }
    }
}

/// Modem knobs shared by `decode` and `encode`.
///
/// Precedence: `--preset` picks the base profile, then `--baud`,
/// `--mark` and `--space` each override that single field of it.
#[derive(Args, Clone)]
pub struct ModemArgs {
    /// Modem preset: the baud rate and mark/space tone pair
    #[arg(long, value_enum, default_value_t = Preset::Bell202)]
    pub preset: Preset,

    /// Baud rate override in bits per second [range: 1..=9600]
    /// [default: the preset's rate]
    #[arg(long, value_name = "BPS")]
    pub baud: Option<u32>,

    /// Mark (logical one) tone override in Hz [range: 1..Nyquist]
    /// [default: the preset's mark tone]
    #[arg(long, value_name = "HZ")]
    pub mark: Option<u32>,

    /// Space (logical zero) tone override in Hz [range: 1..Nyquist]
    /// [default: the preset's space tone]
    #[arg(long, value_name = "HZ")]
    pub space: Option<u32>,

    /// FX.25 forward error correction: on encode, wrap each frame in a
    /// correlation tag + Reed-Solomon codeblock (legacy receivers still
    /// decode the embedded AX.25 frame); on decode, use the FX.25-aware
    /// receive path (which also still decodes plain AX.25 frames).
    /// Tone-AFSK presets only.
    #[arg(long, conflicts_with = "il2p")]
    pub fx25: bool,

    /// IL2P framing (Improved Layer 2 Protocol): on encode, replace the
    /// HDLC framing wholesale — sync word, translated header and
    /// Reed-Solomon-protected payload blocks (16 parity symbols per
    /// block, the published baseline); on decode, use the IL2P sync-word
    /// receive path. NOT AX.25-compatible on the air: both ends must
    /// speak IL2P. Tone-AFSK presets only. Implemented by `gen` and
    /// `decode`; `encode`, `bench` and `serve` reject it rather than
    /// quietly producing plain AX.25.
    #[arg(long)]
    pub il2p: bool,
}

impl ModemArgs {
    /// Rejects `--il2p` on a subcommand that does not implement it.
    ///
    /// `--il2p` is shared plumbing, so every subcommand parses it, but
    /// only `gen` and `decode` act on it. Accepting and ignoring it
    /// meant `warble encode --il2p …` wrote a plain AX.25 WAV with no
    /// warning — the operator gets a file they believe is IL2P and
    /// discovers otherwise on the air. Refusing is the correct
    /// behaviour: an unimplemented flag is an error, not a no-op.
    pub fn reject_il2p(&self, subcommand: &str) -> Result<(), String> {
        if self.il2p {
            return Err(format!(
                "--il2p is not implemented for `{subcommand}` (only `gen` and `decode` \
                 honor it). Refusing rather than silently producing plain AX.25."
            ));
        }
        Ok(())
    }

    /// Composes the preset and any per-knob overrides into a validated
    /// TNC configuration at `rate`.
    pub fn config(&self, rate: SampleRate) -> Result<TncConfig, String> {
        let base = self.preset.profile();
        #[cfg(feature = "g3ruh")]
        if matches!(self.preset, Preset::G3ruh) {
            if self.fx25 {
                // The FX.25 receive seam sits on the post-NRZI tone-AFSK
                // bit stream; the scrambled-baseband pipeline is not
                // wired through it.
                return Err("--fx25 does not apply to the g3ruh preset (tone-AFSK \
                     presets only)"
                    .to_owned());
            }
            if self.il2p {
                // Same seam: the CLI's IL2P paths ride the tone-AFSK
                // demodulator (the library supports IL2P over the
                // baseband machinery; the CLI wiring does not yet).
                return Err("--il2p does not apply to the g3ruh preset (tone-AFSK \
                     presets only)"
                    .to_owned());
            }
            // Scrambled-baseband profile: no audio tones exist and the
            // baud rate is part of the standard, so the per-knob
            // overrides do not compose with it.
            if self.baud.is_some() || self.mark.is_some() || self.space.is_some() {
                return Err(
                    "--baud/--mark/--space do not apply to the g3ruh preset (scrambled \
                     baseband has no audio tones and a fixed 9600 Bd rate)"
                        .to_owned(),
                );
            }
            return TncConfig::from_profile(rate, base)
                .map_err(|e| format!("sample rate {} Hz: {e}", rate.hz()));
        }
        let baud = match self.baud {
            Some(bps) => BaudRate::new(bps).map_err(|e| format!("bad --baud '{bps}': {e}"))?,
            None => base.baud(),
        };
        let mark = self.mark.unwrap_or(base.tones().mark_hz());
        let space = self.space.unwrap_or(base.tones().space_hz());
        let tones = TonePair::new(mark, space, rate).map_err(|e| {
            format!(
                "bad tones (mark {mark} Hz, space {space} Hz) at {} Hz: {e}",
                rate.hz()
            )
        })?;
        TncConfig::new(rate, baud, tones).map_err(|e| format!("sample rate {} Hz: {e}", rate.hz()))
    }
}

/// Raw-PCM sample encodings accepted on stdin. Only one exists today;
/// the enum keeps `--format` open for more (e.g. f32le) later.
#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum InputFormat {
    /// Signed 16-bit little-endian mono PCM.
    #[value(name = "s16le")]
    S16le,
}

/// Parses `CALL` or `CALL-SSID` into an AX.25 address.
pub fn parse_address(text: &str) -> Result<Address, String> {
    let (call, ssid) = match text.split_once('-') {
        Some((call, ssid)) => {
            let ssid: u8 = ssid.parse().map_err(|_| {
                format!("bad SSID '{ssid}' in '{text}': a number 0..=15 is required")
            })?;
            (call, ssid)
        }
        None => (text, 0),
    };
    Address::new(call.as_bytes(), ssid).map_err(|e| format!("bad callsign '{text}': {e}"))
}

/// Formats an address as `CALL` or `CALL-SSID`.
pub fn format_address(addr: &Address) -> String {
    let call = String::from_utf8_lossy(addr.callsign.as_bytes()).into_owned();
    match addr.ssid.value() {
        0 => call,
        n => format!("{call}-{n}"),
    }
}

/// The sample rates the crate accepts, for the unsupported-rate
/// error message.
pub const SUPPORTED_RATES: &str = "8000..=48000 Hz";

/// Validates a WAV header (16-bit mono integer PCM at a supported
/// rate); `source` names the input in error messages. Thin wrapper over
/// the library's [`warble::wav::check_spec`], mapping its typed error
/// into the CLI's source-labelled message.
pub fn check_wav_spec(spec: &hound::WavSpec, source: &str) -> Result<SampleRate, String> {
    warble::wav::check_spec(spec).map_err(|e| match e {
        warble::wav::WavError::UnsupportedFormat { .. } => format!(
            "unsupported WAV format in '{source}': got {} channel(s), {} bits, {:?} samples; \
             16-bit mono integer PCM is required",
            spec.channels, spec.bits_per_sample, spec.sample_format
        ),
        _ => format!(
            "unsupported WAV sample rate in '{source}': got {} Hz, supported: {SUPPORTED_RATES}",
            spec.sample_rate
        ),
    })
}

/// A WAV reader's samples as the error-mapped iterator the decode core
/// consumes.
pub fn wav_samples<'r, R: std::io::Read>(
    reader: &'r mut hound::WavReader<R>,
    source: &'r str,
) -> impl Iterator<Item = Result<i16, String>> + 'r {
    reader
        .samples::<i16>()
        .map(move |s| s.map_err(|e| format!("reading '{source}': {e}")))
}

/// Stdin-style audio (WAV or raw s16le PCM) resolved by the sniff: the
/// validated sample rate plus a `Send + 'static` sample iterator.
pub type SniffedSamples = (
    SampleRate,
    Box<dyn Iterator<Item = Result<i16, String>> + Send>,
);

/// Sniffs a byte stream the way `decode -` and `serve --input -` share:
/// a RIFF header means WAV (rate from the header, checked against any
/// `--sample-rate` the user also passed), anything else is raw s16le
/// PCM at the required `--sample-rate`. Thin CLI wrapper over the
/// library's [`warble::wav::sniff_pcm`], mapping its typed errors into
/// flag-level messages.
pub fn sniff_stdin_samples<R>(reader: R, sample_rate: Option<u32>) -> Result<SniffedSamples, String>
where
    R: std::io::Read + Send + 'static,
{
    let hint = match sample_rate {
        Some(hz) => {
            Some(SampleRate::new(hz).map_err(|e| format!("bad --sample-rate '{hz}': {e}"))?)
        }
        None => None,
    };
    match sniff_pcm(reader, hint) {
        Ok(SniffedPcm::Wav { rate, reader }) => {
            let samples = reader
                .into_samples::<i16>()
                .map(|s| s.map_err(|e| format!("reading stdin: {e}")));
            Ok((rate, Box::new(samples)))
        }
        Ok(SniffedPcm::Raw { rate, reader }) => Ok((
            rate,
            Box::new(S16leSamples {
                reader: std::io::BufReader::new(reader),
            }),
        )),
        Err(WavError::RateRequired) => Err(
            "raw PCM on stdin has no sample-rate header: pass --sample-rate <HZ> (it \
             must match the rate your capture tool records at)"
                .to_owned(),
        ),
        Err(WavError::RateContradiction {
            header_hz,
            given_hz,
        }) => Err(format!(
            "--sample-rate {given_hz} contradicts the WAV header on stdin \
             ({header_hz} Hz); drop the flag for WAV input"
        )),
        Err(e @ WavError::UnsupportedFormat { .. }) => {
            Err(format!("unsupported WAV format on stdin: {e}"))
        }
        Err(WavError::UnsupportedRate { hz }) => Err(format!(
            "unsupported WAV sample rate on stdin: got {hz} Hz, supported: {SUPPORTED_RATES}"
        )),
        Err(e) => Err(format!("reading WAV from stdin: {e}")),
    }
}

/// Streams signed 16-bit little-endian samples out of a byte reader
/// until EOF; a trailing odd byte is an error, not a silent drop.
pub struct S16leSamples<R: std::io::Read> {
    /// The underlying byte reader.
    pub reader: R,
}

impl<R: std::io::Read> Iterator for S16leSamples<R> {
    type Item = Result<i16, String>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut bytes = [0u8; 2];
        let mut filled = 0usize;
        while filled < bytes.len() {
            match self.reader.read(&mut bytes[filled..]) {
                Ok(0) if filled == 0 => return None,
                Ok(0) => {
                    return Some(Err(
                        "reading stdin: truncated sample (odd byte count) at EOF".to_owned(),
                    ));
                }
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Some(Err(format!("reading stdin: {e}"))),
            }
        }
        Some(Ok(i16::from_le_bytes(bytes)))
    }
}

/// Builds `packet` into a UI frame, wraps it in an FX.25 transmission
/// (correlation tag + Reed-Solomon codeblock around the stuffed HDLC
/// frame) and modulates it, with the configured preamble/tail flag
/// octets around the wrapped block so clock recovery can lock.
pub fn fx25_samples(
    tx: &TncTransmitter,
    config: TncConfig,
    packet: &warble::aprs::AprsPacket<'_>,
    dest: Address,
    src: Address,
    path: &[Address],
) -> Result<Vec<i16>, String> {
    let mut info_buf = [0u8; MAX_FRAME_BYTES];
    let mut frame_buf = [0u8; MAX_FRAME_BYTES];
    let len = tx
        .build_frame(packet, dest, src, path, &mut info_buf, &mut frame_buf)
        .map_err(|e| format!("building the packet: {e}"))?;
    let mut stuffed = [0u8; 2 * MAX_FRAME_BYTES];
    let stuffed_len =
        stuff_frame(&frame_buf[..len], &mut stuffed).map_err(|e| format!("FX.25 framing: {e}"))?;
    let mut wrapped = [0u8; WRAP_MAX];
    let frame =
        wrap(&stuffed[..stuffed_len], &mut wrapped).map_err(|e| format!("FX.25 framing: {e}"))?;
    let mut bytes = vec![0x7Eu8; config.preamble_flags()];
    bytes.extend_from_slice(&wrapped[..frame.len()]);
    bytes.extend(std::iter::repeat_n(0x7Eu8, config.tail_flags().max(2)));
    let modulator_config =
        ModulatorConfig::new(config.sample_rate(), config.baud(), config.tones())
            .map_err(|e| format!("transmitter setup: {e}"))?;
    Ok(Modulator::new(modulator_config)
        .i16_samples(nrzi::encode_iter(byte_bits(&bytes)))
        .collect())
}

/// The payload-parity operating point the CLI's IL2P paths use: the
/// published 16-symbols-per-block baseline (IL2P does not signal the
/// point in the header, so both ends must agree).
pub const IL2P_PARITY: Il2pParity = Il2pParity::Sixteen;

/// Builds `packet` into a UI frame, encodes it as an IL2P transmission
/// (sync word, translated/transparent header, scrambled RS-protected
/// payload blocks) and modulates it, with the configured preamble/tail
/// counts as 0x55 bytes around the frame so clock recovery can lock.
pub fn il2p_samples(
    tx: &TncTransmitter,
    config: TncConfig,
    packet: &warble::aprs::AprsPacket<'_>,
    dest: Address,
    src: Address,
    path: &[Address],
) -> Result<Vec<i16>, String> {
    let mut info_buf = [0u8; MAX_FRAME_BYTES];
    let mut frame_buf = [0u8; MAX_FRAME_BYTES];
    let len = tx
        .build_frame(packet, dest, src, path, &mut info_buf, &mut frame_buf)
        .map_err(|e| format!("building the packet: {e}"))?;
    let ui = warble::ax25::UiFrame::parse(&frame_buf[..len])
        .map_err(|e| format!("building the packet: {e}"))?;
    let mut encoded = [0u8; ENCODED_MAX];
    let len = il2p::encode_ui_frame(&ui, IL2P_PARITY, &mut encoded)
        .map_err(|e| format!("IL2P framing: {e}"))?;
    let modulator_config =
        ModulatorConfig::new(config.sample_rate(), config.baud(), config.tones())
            .map_err(|e| format!("transmitter setup: {e}"))?;
    // IL2P is NOT differentially encoded -- see `il2p::tx_bits`. The
    // bits go straight to the modulator, unlike the AX.25/FX.25 path
    // just above, which does pass through NRZI.
    Ok(Modulator::new(modulator_config)
        .i16_samples(il2p::tx_bits(
            &encoded[..len],
            config.preamble_flags(),
            config.tail_flags().max(2),
        ))
        .collect())
}
