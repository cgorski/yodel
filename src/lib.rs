//! Bell 202 AFSK software modem.
//!
//! [Bell 202] audio frequency-shift keying (AFSK) encodes binary data as a
//! pair of audio tones: a **mark** tone (1200 Hz, logical one) and a
//! **space** tone (2200 Hz, logical zero), keyed at 1200 baud. It is the
//! classic physical layer of packet radio (AX.25/APRS) and legacy telephone
//! modems, and remains popular because the tones survive ordinary voice
//! channels.
//!
//! `warble` is a `#![no_std]`, zero-dependency, allocation-free
//! implementation. The modulator produces *continuous-phase* FSK: a single
//! phase accumulator runs across bit boundaries, so switching tones never
//! introduces a click (a discontinuity) in the waveform.
//!
//! # Streaming API
//!
//! The core types are push/pull state machines that own no buffers:
//!
//! * feed one bit into a [`Modulator`] with [`Modulator::feed`], then pull
//!   PCM samples out with [`Modulator::next_i16`] or
//!   [`Modulator::next_f32`] until the bit is exhausted;
//! * or wrap any `Iterator<Item = Bit>` with [`Modulator::i16_samples`] /
//!   [`Modulator::f32_samples`] and pull samples from the returned iterator.
//!
//! Both an integer-only `i16` PCM path and an `f32` PCM path are provided.
//!
//! # Features
//!
//! * `mod` (default): the modulator.
//! * `demod` (default): the demodulator.
//! * `nrzi`: NRZI differential line coding (the layer between raw AFSK
//!   bits and HDLC framing).
//! * `ax25`: AX.25 UI frames — addresses, FCS, HDLC bit framing; implies
//!   `nrzi`. Full frame↔samples wiring appears when combined with `mod` /
//!   `demod`.
//! * `aprs`: APRS position/status/message payloads over AX.25 UI frames;
//!   implies `ax25`.
//! * `micE`: Mic-E compressed position reports (APRS 1.01 chapter 10);
//!   implies `aprs`.
//! * `digipeat`: WIDEn-N digipeater primitives — served aliases, the
//!   pure relay-decision core, duplicate suppression; implies `ax25`.
//! * `kiss`: KISS TNC framing (byte-level escaping); standalone.
//! * `fx25`: the FX.25 FEC layer — Reed-Solomon `RS(255,k)` codec over
//!   `GF(256)` plus the correlation-tag framing wrapper (the tag-hunting
//!   receiver additionally needs `ax25`); standalone.
//! * `il2p`: the IL2P frame codec — sync word, 13-byte header codec,
//!   `x^9 + x^4 + 1` scrambler, per-block Reed-Solomon FEC; implies
//!   `ax25` (header translation). Off by default, enabled by nothing
//!   else.
//! * `g3ruh`: G3RUH 9600-baud support: the multiplicative LFSR
//!   scrambler/descrambler (x^17 + x^12 + 1, standalone) plus — combined
//!   with `mod` / `demod` — the direct-baseband modulator and demodulator
//!   front end.
//! * `ft8`: FT8 ([`ft8`]) — the documented message subset
//!   (standard `i3 = 1` + free text) through CRC-14, LDPC(174,91),
//!   Gray/Costas mapping (79 channel symbols) to GFSK-shaped
//!   continuous-phase 8-FSK audio, plus the no_std receive math
//!   (Gray-demap LLRs, hard-capped LDPC min-sum decoder, CRC-14
//!   verify, message unpack). Combined with `std`, also the buffered
//!   `Ft8Decoder` receive engine. Standalone, off by default, enabled
//!   by `cli`.
//! * `m17`: M17 packet-mode data ([`m17`]) — base-40 callsign
//!   addressing, Link Setup Frame + packet superframes (CRC-16
//!   0x5935), K=5 convolutional FEC with P1/P3 puncturing, QPP
//!   interleaver, randomizer, Golay(24,12) building block, and the
//!   4-level RRC baseband modem (TX + RX) at 4800 symbols/s. Fully
//!   no_std and alloc-free; voice (Codec2) absent (see
//!   docs/ARCHITECTURE.md). Standalone, off by default, enabled by
//!   nothing else.
//! * `wspr`: the WSPR beacon ([`wspr`]) — type-1 message encoding
//!   through the 162-symbol channel coding to continuous-phase 4-FSK
//!   audio, plus the no_std receive math (deinterleave, hard-capped
//!   Fano sequential decoder, message unpack). Combined with `std`,
//!   also the buffered `WsprDecoder` receive engine. Standalone, off
//!   by default, enabled by `cli`.
//! * `tnc`: high-level TNC pipeline (PCM samples ↔ APRS packets);
//!   implies `aprs`, `mod` and `demod`.
//! * `alloc`: heap-backed conveniences — `TncTransmitter::to_vec_i16`
//!   / `_f32`, `kiss::encode_to_vec`, `AprsPacket::to_vec` and
//!   `UiFrame::to_vec`. Each is gated on `alloc` plus the feature that
//!   owns the type, and pulls in no dependency.
//! * `std`: std conveniences; implies `alloc`. Pulls in no dependency.
//! * `wav`: WAV I/O via `hound`; implies `std`.
//! * `async`: tokio adapters ([`asynk`]) — decoded frames as `Stream`s,
//!   a one-call async KISS server, a concurrent many-feeds decoder;
//!   implies `std`, `tnc` and `kiss`. The only feature that pulls an
//!   async runtime; off by default.
//! * `embassy`: no_std embassy adapters ([`embassy`]) — an async
//!   chunk-drain decode loop over the sync core plus an embassy-time
//!   TX ticker; implies `tnc`, pulls only `embassy-time`. Off by
//!   default, enabled by nothing else.
//! * `ptt`: serial PTT for `warble ptt` — assert RTS or DTR on a
//!   USB-serial adapter to key a transmitter, hold it, and drop it
//!   again. CLI only, and the one feature in this crate that can put a
//!   signal on the air by itself, so its failure mode is deassert.
//!   Pulls `serialport` with default features off.
//! * `capture`: live sound-card capture (`cpal`) for the `live_capture`
//!   example only. Off by default and enabled by nothing else.
//! * `cli`: aggregate (`wav` + `tnc` + `micE` + `kiss` + `fx25` +
//!   `il2p` + `wspr` + `ft8` + `m17` + `ptt`) enabling the `warble`
//!   command-line binary.
//!
//! # Units and geography
//!
//! [`geo`] carries [`Coordinates`], [`Latitude`], [`Longitude`],
//! [`Ambiguity`] and [`MaidenheadGrid`] — the position primitives —
//! together with integer-only distance and bearing. It sits at the
//! crate root rather than under `aprs` because grid squares belong to
//! WSPR and FT8 too, and because `units` would otherwise have to point
//! into `aprs` to express a distance.
//!
//! [`units`] carries the crate's physical quantities — [`Distance`],
//! [`Speed`], [`Bearing`], [`Temperature`], [`Pressure`], [`Rainfall`],
//! [`Power`], [`Humidity`] — each storing one canonical integer unit and
//! naming its unit in every constructor and accessor. It is **not**
//! feature-gated: it is integer-only, tiny, and needed by both the
//! transmit and receive halves of the APRS layer, so gating it would
//! only fragment the build matrix for no saving.
//!
//! [Bell 202]: https://en.wikipedia.org/wiki/Bell_202_modem
#![cfg_attr(not(feature = "std"), no_std)]
// docs.rs sets `docsrs` (see [package.metadata.docs.rs]); this renders
// the feature badge on every gated item. Nightly-only, and never set by
// an ordinary build.
#![cfg_attr(docsrs, feature(doc_auto_cfg))]
// Without the DSP features, the shared sine-table machinery in `types` is
// unused; it is not worth cfg-gating each item for feature-solo builds.
#![cfg_attr(not(any(feature = "mod", feature = "demod")), allow(dead_code))]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(feature = "alloc")]
extern crate alloc;

/// Compiles and runs the README examples as doctests.
#[cfg(all(doctest, feature = "mod", feature = "demod"))]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;

/// Compiles and runs the embedded guide's examples as doctests.
#[cfg(all(doctest, feature = "mod", feature = "demod"))]
#[doc = include_str!("../docs/EMBEDDED.md")]
pub struct EmbeddedDoctests;

pub mod error;
pub mod geo;
mod types;
pub mod units;

#[cfg(feature = "aprs")]
pub mod aprs;
#[cfg(feature = "async")]
pub mod asynk;
#[cfg(feature = "ax25")]
pub mod ax25;
#[cfg(feature = "g3ruh")]
pub mod baseband;
#[cfg(feature = "demod")]
pub mod demodulator;
#[cfg(feature = "digipeat")]
pub mod digipeat;
#[cfg(feature = "demod")]
pub mod discriminator;
#[cfg(feature = "embassy")]
pub mod embassy;
#[cfg(feature = "ft8")]
pub mod ft8;
#[cfg(feature = "fx25")]
pub mod fx25;
#[cfg(feature = "il2p")]
pub mod il2p;
#[cfg(feature = "kiss")]
pub mod kiss;
#[cfg(feature = "m17")]
pub mod m17;
#[cfg(feature = "mod")]
pub mod modulator;
#[cfg(feature = "nrzi")]
pub mod nrzi;
pub mod ring;
#[cfg(any(feature = "fx25", feature = "il2p"))]
pub mod rs;
#[cfg(feature = "g3ruh")]
pub mod scrambler;
#[cfg(feature = "demod")]
pub mod slicer;
#[cfg(feature = "tnc")]
pub mod tnc;
#[cfg(feature = "wav")]
pub mod wav;
#[cfg(feature = "wspr")]
pub mod wspr;

pub use error::ConfigError;
pub use geo::{
    Ambiguity, Coordinates, DegreesMinutes, GeoError, GridPrecision, Latitude, LatitudeHemisphere,
    Longitude, LongitudeHemisphere, MaidenheadGrid,
};
pub use ring::SampleRing;
pub use types::{
    BAUD_MAX, BAUD_MIN, BaudRate, Bit, DevicePreset, ModemProfile, ModulationScheme,
    SAMPLE_RATE_MAX, SAMPLE_RATE_MIN, SampleRate, TonePair,
};
pub use units::{
    Bearing, CompassPoint, Distance, Humidity, Power, Pressure, Rainfall, Speed, Temperature,
    UnitError,
};

#[cfg(feature = "mod")]
pub use modulator::{Modulator, ModulatorConfig};

#[cfg(feature = "demod")]
pub use demodulator::{AfskDemodulator, Demodulator, DemodulatorConfig};
#[cfg(feature = "demod")]
pub use discriminator::{Discriminator, QuadratureCorrelator};
#[cfg(feature = "demod")]
pub use slicer::Slicer;

#[cfg(feature = "nrzi")]
pub use nrzi::{NrziDecoder, NrziEncoder};

#[cfg(feature = "g3ruh")]
pub use scrambler::{Descrambler, Scrambler};

#[cfg(all(feature = "g3ruh", feature = "mod"))]
pub use baseband::BasebandModulator;

#[cfg(all(feature = "g3ruh", feature = "demod"))]
pub use baseband::BasebandDemodulator;
