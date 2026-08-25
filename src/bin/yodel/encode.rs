//! `yodel encode`: build an APRS packet and modulate it into a
//! 16-bit mono WAV file.

use clap::{Args, Subcommand};

use yodel::SampleRate;
use yodel::aprs::{
    Addressee, AprsPacket, Latitude, Longitude, Message, MessageContent, Position, Symbol,
};
use yodel::ax25::Address;
use yodel::tnc::{TncConfig, TncTransmitter};

use crate::shared::{
    DEFAULT_TXTAIL_MS, MAX_TXDELAY_MS, MAX_TXTAIL_MS, ModemArgs, flags_for_ms, fx25_samples,
    parse_address,
};

#[derive(Args)]
pub struct EncodeArgs {
    /// Output WAV file (16-bit mono integer PCM)
    #[arg(long, value_name = "OUTPUT.wav")]
    out: String,

    /// Source callsign, `CALL` or `CALL-SSID` (SSID 0..=15)
    #[arg(long, value_name = "CALL[-SSID]")]
    from: String,

    /// Destination callsign, `CALL` or `CALL-SSID`
    #[arg(long, value_name = "CALL[-SSID]")]
    to: String,

    /// Comma-separated digipeater path, e.g. `WIDE1-1,WIDE2-1`
    #[arg(long, value_name = "DIGI[-SSID],...")]
    path: Option<String>,

    /// Output sample rate in Hz [range: 8000..=48000]
    #[arg(
        long = "sample-rate",
        visible_alias = "rate",
        value_name = "HZ",
        default_value_t = 44_100
    )]
    sample_rate: u32,

    /// TXDelay: milliseconds of HDLC flags sent ahead of the frame, so a
    /// receiving station's squelch, AGC and clock recovery have all
    /// settled before the data starts [range: 0..=2000] [default: ~213,
    /// the library's 32 flags at 1200 baud]
    ///
    /// This is the ON-AIR lead-in and it travels inside the WAV. It is
    /// not the electrical one: `yodel ptt --lead` holds the control line
    /// up before the player starts, which is a different delay solving a
    /// different problem. A slow transmitter, a relay with a sequencer,
    /// or a distant receiver whose squelch has to open all want both.
    ///
    /// Given in milliseconds rather than in flags because that is how
    /// every TNC on the air is configured, and because the flag count a
    /// delay works out to depends on the baud rate.
    #[arg(long, value_name = "MS")]
    txdelay: Option<u32>,

    /// TXTail: milliseconds of HDLC flags sent AFTER the frame, so the
    /// checksum is clear of the transmit path before the modulator stops
    /// [range: 0..=500]
    ///
    /// Every transmit path has latency between handing the last sample
    /// to an audio device and that sample leaving the radio, and some
    /// players simply discard whatever has not been converted when they
    /// exit. Whatever is lost comes off the END of the transmission,
    /// which is where the FCS lives -- so too short a tail produces
    /// frames that look perfect at the transmitter and fail their CRC at
    /// every receiver.
    ///
    /// Flags, not silence: a tail keeps the modulator running, which is
    /// what Dire Wolf and hardware TNCs do, and what a receiver's clock
    /// recovery expects. Trailing silence would hold the carrier up
    /// saying nothing.
    ///
    /// 0 falls back to the library's two-flag framing minimum, which
    /// closes the frame correctly but leaves no margin for the path.
    #[arg(long, value_name = "MS", default_value_t = DEFAULT_TXTAIL_MS)]
    txtail: u32,

    #[command(flatten)]
    modem: ModemArgs,

    #[command(subcommand)]
    packet: Packet,
}

#[derive(Subcommand)]
enum Packet {
    /// An uncompressed position report.
    Position {
        /// Latitude in decimal degrees [range: -90..=90; negative = S]
        #[arg(long, value_name = "DEG", allow_hyphen_values = true)]
        lat: f64,

        /// Longitude in decimal degrees [range: -180..=180; negative = W]
        #[arg(long, value_name = "DEG", allow_hyphen_values = true)]
        lon: f64,

        /// APRS symbol: exactly two characters, table then code
        #[arg(long, value_name = "TABLE+CODE", default_value = "/-")]
        symbol: String,

        /// Free-text comment appended to the report
        #[arg(long, value_name = "TEXT", default_value = "")]
        comment: String,
    },
    /// A text message to another station.
    Message {
        /// Addressee callsign (up to 9 characters)
        #[arg(long = "to-call", value_name = "CALL")]
        to_call: String,

        /// The message text
        #[arg(long, value_name = "TEXT")]
        text: String,

        /// Optional message id (up to 5 characters), e.g. `42`
        #[arg(long, value_name = "ID")]
        id: Option<String>,
    },
}

/// Checks a signed decimal-degree coordinate against its range.
fn check_degrees(value: f64, flag: &str, max: f64) -> Result<f64, String> {
    if !value.is_finite() || value < -max || value > max {
        return Err(format!(
            "bad {flag} '{value}': the range -{max}..={max} degrees is required"
        ));
    }
    Ok(value)
}

/// Applies `--txdelay` and `--txtail` to `config`'s flag counts.
///
/// An absent `--txdelay` leaves the library's documented 32-flag
/// preamble exactly as it was. `--txtail` always applies, because its
/// default is a deliberate CLI choice rather than the library's: see
/// [`DEFAULT_TXTAIL_MS`] for why a framing minimum is not a transmit
/// tail. `--txtail 0` asks for the library minimum back.
fn apply_flag_timing(
    config: TncConfig,
    txdelay: Option<u32>,
    txtail: u32,
) -> Result<TncConfig, String> {
    let baud = config.baud().bps();

    let preamble = match txdelay {
        None => config.preamble_flags(),
        // Zero is refused rather than rounded up to the one flag that
        // merely delimits the frame: that is not a very short lead-in,
        // it is a transmission no receiver can lock onto, and silently
        // producing one would be the sort of no-op this CLI refuses
        // elsewhere.
        Some(0) => {
            return Err(format!(
                "bad --txdelay '0': a lead-in of at least 1 ms is required (a frame with no \
                 preamble gives a receiver's squelch, AGC and clock recovery nothing to \
                 settle on, so it will not decode). The range is 1..={MAX_TXDELAY_MS} ms."
            ));
        }
        Some(ms) if ms > MAX_TXDELAY_MS => {
            return Err(format!(
                "bad --txdelay '{ms}': the range 1..={MAX_TXDELAY_MS} ms is required (flags \
                 are transmitted, so a longer lead-in is key-down time on a shared channel)"
            ));
        }
        Some(ms) => flags_for_ms(ms, baud),
    };

    if txtail > MAX_TXTAIL_MS {
        return Err(format!(
            "bad --txtail '{txtail}': the range 0..={MAX_TXTAIL_MS} ms is required (a tail \
             only has to outlast the transmit path; past that it is airtime spent saying \
             nothing)"
        ));
    }
    let tail = if txtail == 0 {
        config.tail_flags()
    } else {
        flags_for_ms(txtail, baud)
    };

    Ok(config.with_flags(preamble, tail))
}

/// Runs `yodel encode`: builds the packet and writes the WAV.
pub fn encode(args: &EncodeArgs) -> Result<(), String> {
    let src = parse_address(&args.from)?;
    let dest = parse_address(&args.to)?;
    let path = match args.path.as_deref() {
        Some(list) => list
            .split(',')
            .map(parse_address)
            .collect::<Result<Vec<_>, _>>()?,
        None => Vec::new(),
    };
    let rate = SampleRate::new(args.sample_rate)
        .map_err(|e| format!("bad sample rate '{}': {e}", args.sample_rate))?;
    args.modem.reject_il2p("encode")?;
    let config = apply_flag_timing(args.modem.config(rate)?, args.txdelay, args.txtail)?;

    let packet = match args.packet {
        Packet::Position {
            lat,
            lon,
            ref symbol,
            ref comment,
        } => {
            let lat = check_degrees(lat, "--lat", 90.0)?;
            let lon = check_degrees(lon, "--lon", 180.0)?;
            let sym = symbol.as_bytes();
            if sym.len() != 2 {
                return Err(format!(
                    "bad symbol '{symbol}': exactly two characters (table then code) are required"
                ));
            }
            let latitude = Latitude::from_degrees(lat).map_err(|e| format!("bad --lat: {e}"))?;
            let longitude = Longitude::from_degrees(lon).map_err(|e| format!("bad --lon: {e}"))?;
            AprsPacket::Position(Position {
                latitude,
                longitude,
                symbol: Symbol::from_wire(sym[0], sym[1]),
                // The CLI takes decimal degrees, so the caller is
                // stating a position, not blanking one.
                ambiguity: yodel::geo::Ambiguity::EXACT,
                messaging: false,
                compressed: false,
                extension: None,
                comment: comment.as_bytes(),
            })
        }
        Packet::Message {
            ref to_call,
            ref text,
            ref id,
        } => {
            let addressee = Addressee::new(to_call.as_bytes())
                .map_err(|e| format!("bad --to-call '{to_call}': {e}"))?;
            AprsPacket::Message(Message {
                addressee,
                content: MessageContent::Text {
                    text: text.as_bytes(),
                    id: id.as_deref().map(str::as_bytes),
                },
            })
        }
    };

    write_wav(
        &args.out,
        rate,
        config,
        args.modem.fx25,
        &packet,
        dest,
        src,
        &path,
    )
}

/// Modulates `packet` and writes a 16-bit mono WAV to `out`.
#[allow(clippy::too_many_arguments)]
fn write_wav(
    out: &str,
    rate: SampleRate,
    config: TncConfig,
    fx25: bool,
    packet: &AprsPacket<'_>,
    dest: Address,
    src: Address,
    path: &[Address],
) -> Result<(), String> {
    let tx = TncTransmitter::new(config);
    let samples = if fx25 {
        fx25_samples(&tx, config, packet, dest, src, path)?
    } else {
        tx.transmit_to_vec_i16(packet, dest, src, path)
            .map_err(|e| format!("building the packet: {e}"))?
    };
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate.hz(),
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer =
        hound::WavWriter::create(out, spec).map_err(|e| format!("creating '{out}': {e}"))?;
    for s in samples {
        writer
            .write_sample(s)
            .map_err(|e| format!("writing '{out}': {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("finalizing '{out}': {e}"))?;
    Ok(())
}
