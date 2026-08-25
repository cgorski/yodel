//! RECEIVE → DECIDE → RESPOND: hear a message for MYCALL, answer it.
//!
//! * **Scenario** — an automated station that answers: hear an APRS
//!   message addressed to MYCALL, acknowledge it and send a canned
//!   reply. The building block for a beacon that responds to queries.
//! * **Hardware** — any host; an always-on Raspberry Pi attached to a
//!   radio is the usual home for this.
//! * **Features** — `tnc,wav`.
//!
//! # What this file does, start to finish
//!
//! This is the smallest complete "APRS application": a station that
//! listens, recognizes traffic addressed to itself, and transmits a
//! spec-correct response.
//!
//! 1. Decodes frames from input audio (a WAV path, same receive chain
//!    as `examples/decode_wav.rs` and `examples/decode_to_log.rs`).
//! 2. Feeds every decoded frame to the PURE decision function
//!    [`decide`]. Its rules follow APRS 1.01 chapter 14, as implemented
//!    (and unit-tested) in `src/aprs/message.rs`:
//!
//!    * only APRS **messages addressed to [`MYCALL`]** trigger anything
//!      — positions, statuses, and messages for other stations are
//!      ignored;
//!    * if the message carries a message-id `{n}`, we owe the sender an
//!      **ack**: a message back to them whose body is exactly `ack`
//!      followed by the SAME id (`Testing{003` → `ack003`);
//!    * a message WITHOUT an id gets NO ack (there is nothing to
//!      acknowledge by), only the canned reply;
//!    * an **ack (or rej) is never itself acked or replied to** —
//!      answering acks with acks would ping-pong forever;
//!    * every triggering message also gets one canned text reply
//!      ([`REPLY_TEXT`]) so a human at the other end sees a response.
//!
//! 3. "Transmits" the responses by rendering their Bell 202 AFSK
//!    samples into an output WAV (`reply.wav`). A real station would
//!    instead key PTT, play exactly these samples into the radio's mic
//!    input, and unkey — see the **Hardware guide** in
//!    `examples/esp32-riscv/README.md` for the PTT wiring, TXDelay, and
//!    audio-level details. Writing a WAV keeps this example runnable
//!    with no radio at all, and lets the host tests decode the output
//!    back to prove the responses are correct on the air, not just in
//!    memory.
//!
//! Because [`decide`] is pure (frame in → response plan out, no I/O),
//! `tests/app_examples.rs` exercises every rule above directly AND
//! proves the full loop: synthesized "message to MYCALL with id" audio
//! in, decoded ack + reply audio out.
//!
//! # Try it
//!
//! Generate a triggering message WAV first (the `yodel` CLI `encode`
//! command can build one, or adapt `examples/encode_wav.rs` to send
//! `AprsPacket::Message`), then run:
//!
//! ```sh
//! cargo run --example trigger_reply --features tnc,wav -- input.wav
//! # responses (if any) land in reply.wav; decode them back with:
//! cargo run --example decode_to_log --features tnc,wav -- reply.wav
//! ```

use yodel::SampleRate;
use yodel::aprs::{Addressee, AprsPacket, Message, MessageContent};
use yodel::ax25::Address;
use yodel::tnc::{DefaultTncReceiver, MAX_FRAME_BYTES, TncConfig, TncReceiver, TncTransmitter};

// ---------------------------------------------------------------------
// Station configuration — edit these.
// ---------------------------------------------------------------------

/// Our station callsign (also the message addressee we answer to).
/// N0CALL is the conventional placeholder — put YOUR callsign here.
pub const MYCALL: &[u8] = b"N0CALL";
/// Our SSID on the air (0 = no SSID suffix). Keep it consistent with
/// the addressee text you answer to: a station transmitting as
/// `N0CALL-10` should set [`MYCALL`] to `b"N0CALL-10"` too.
pub const MYCALL_SSID: u8 = 0;
/// The canned reply text sent to whoever messaged us.
pub const REPLY_TEXT: &[u8] = b"QSL - automated yodel station";
/// The APRS destination "tocall" for everything we transmit.
pub const TOCALL: &[u8] = b"APRS";
/// Sample rate for the generated reply audio.
pub const SAMPLE_RATE_HZ: u32 = 48_000;

// ---------------------------------------------------------------------
// The pure decision core.
// ---------------------------------------------------------------------

/// What we decided to send back for one received frame.
///
/// Owned bytes (not borrows) so the plan outlives the receiver's
/// internal frame buffer, which is reused on the next push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsePlan {
    /// Who we are answering (the source of the triggering frame).
    pub to: Vec<u8>,
    /// The message-id to ack, when the message carried a `{n}` id.
    pub ack_id: Option<Vec<u8>>,
    /// The canned reply text (always sent for a triggering message).
    pub reply_text: Vec<u8>,
}

/// Decides whether — and how — to respond to one received frame.
///
/// PURE: no I/O, no clock, no radio. Takes the pieces of a parsed
/// frame, returns `Some(plan)` only when a response is owed:
///
/// * `None` for anything that is not an APRS message (positions,
///   statuses, telemetry, unparseable payloads);
/// * `None` for messages addressed to anyone but `mycall` (the
///   addressee comparison is against the exact callsign text,
///   e.g. `N0CALL-10`);
/// * `None` for acks and rejects — even ones addressed to us —
///   because acking an ack would loop forever;
/// * `Some` with `ack_id: Some(id)` for a text message carrying `{id}`
///   (spec: the recipient MUST ack messages that have a message-id);
/// * `Some` with `ack_id: None` for a text message without an id (no
///   id means nothing to ack, but we still send the canned reply).
#[must_use]
pub fn decide(mycall: &[u8], src: &Address, info: &[u8]) -> Option<ResponsePlan> {
    // Only APRS messages trigger; everything else is ignored.
    let AprsPacket::Message(msg) = AprsPacket::parse(info).ok()? else {
        return None;
    };
    // Only messages addressed to US trigger.
    if msg.addressee.as_bytes() != mycall {
        return None;
    }
    // Never respond to an ack or a rej (loop prevention).
    let (ack_id, _text) = match msg.content {
        MessageContent::Text { text, id } => (id.map(<[u8]>::to_vec), text),
        MessageContent::Ack { .. } | MessageContent::Reject { .. } => return None,
    };
    Some(ResponsePlan {
        to: addr_text(src),
        ack_id,
        reply_text: REPLY_TEXT.to_vec(),
    })
}

/// Builds the response messages (ack first, then the canned reply) as
/// APRS `Message` payload byte strings — also pure. Each entry is one
/// information field ready to wrap in a UI frame and modulate.
///
/// The ack follows `src/aprs/message.rs` semantics exactly: body
/// `ack` + the original id, e.g. `:N1CALL   :ack003`.
///
/// # Errors
///
/// Propagates `AprsError` when the addressee or id is invalid (cannot
/// happen for ids that parsed off the air, but the types require the
/// check anyway).
pub fn build_responses(plan: &ResponsePlan) -> Result<Vec<Vec<u8>>, yodel::aprs::AprsError> {
    let addressee = Addressee::new(&plan.to)?;
    let mut out = Vec::new();
    let mut buf = [0u8; 128];
    if let Some(id) = &plan.ack_id {
        let ack = Message {
            addressee,
            content: MessageContent::Ack { id },
        };
        let len = ack.build(&mut buf)?;
        out.push(buf[..len].to_vec());
    }
    let reply = Message {
        addressee,
        content: MessageContent::Text {
            text: &plan.reply_text,
            id: None, // an unnumbered reply: we do not ask for an ack back
        },
    };
    let len = reply.build(&mut buf)?;
    out.push(buf[..len].to_vec());
    Ok(out)
}

/// The textual form of an address (`CALL` or `CALL-SSID`) as message
/// addressee bytes.
fn addr_text(addr: &Address) -> Vec<u8> {
    let mut out = addr.callsign.as_bytes().to_vec();
    let ssid = addr.ssid.value();
    if ssid != 0 {
        out.push(b'-');
        out.extend_from_slice(ssid.to_string().as_bytes());
    }
    out
}

// ---------------------------------------------------------------------
// The I/O shell: WAV in, decisions, WAV out.
// ---------------------------------------------------------------------

/// Where to get an input, printed whenever one is missing or unusable.
/// This example wants a *message* addressed to MYCALL, so the generic
/// beacon generators are not enough on their own — hence the third
/// recipe, which is the one that triggers a reply.
const INPUT_HELP: &str = "\
input: a 16-bit mono integer PCM WAV, 8000-48000 Hz, containing an APRS
message addressed to MYCALL (see the constant in this file).

make one that actually triggers a reply:
  cargo run --features cli -- encode --out msg.wav \\
      --from N0CALL-7 --to APRS \\
      message --to-call MYCALL --text 'ping' --id 42

or just something to decode (no reply expected):
  cargo run --example encode_wav --features tnc,wav
  cargo run --features cli -- gen --out test.wav --count 10 --snr 10";

fn main() {
    // Display, not Debug: returning `Result` from `main` would escape
    // the newlines in the help text onto one unreadable line.
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| format!("usage: trigger_reply <input.wav>\n\n{INPUT_HELP}"))?;

    // Receive chain, exactly as in decode_wav / decode_to_log.
    let mut reader = hound::WavReader::open(&path).map_err(|e| match e {
        hound::Error::IoError(io) if io.kind() == std::io::ErrorKind::NotFound => {
            format!("cannot open {path}: no such file\n\n{INPUT_HELP}")
        }
        hound::Error::FormatError(_) => format!("{path} is not a WAV file ({e})\n\n{INPUT_HELP}"),
        other => format!("cannot open {path}: {other}"),
    })?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err(format!(
            "{path} is {}-channel {}-bit at {} Hz; need 1-channel 16-bit integer\n\n{INPUT_HELP}",
            spec.channels, spec.bits_per_sample, spec.sample_rate
        )
        .into());
    }
    let rate = SampleRate::new(spec.sample_rate)?;
    let mut rx: DefaultTncReceiver = TncReceiver::new(TncConfig::bell_202(rate)?)?;

    // Collect the response plans first (the borrowed frame dies on the
    // next push; the plan owns its bytes).
    let mut plans = Vec::new();
    for sample in reader.samples::<i16>() {
        if let Some(frame) = rx.push_i16(sample?)
            && let Some(plan) = decide(MYCALL, &frame.src(), frame.info())
        {
            println!(
                "triggered by {}: ack_id {:?}",
                String::from_utf8_lossy(&plan.to),
                plan.ack_id.as_deref().map(String::from_utf8_lossy)
            );
            plans.push(plan);
        }
    }
    if plans.is_empty() {
        println!(
            "no messages addressed to {} heard",
            String::from_utf8_lossy(MYCALL)
        );
        return Ok(());
    }

    // "Transmit": render every response frame's Bell 202 samples into
    // reply.wav. A real station keys PTT, plays these samples, unkeys
    // (see the ESP32 hardware guide for the wiring).
    let out_rate = SampleRate::new(SAMPLE_RATE_HZ)?;
    let tx = TncTransmitter::new(TncConfig::bell_202(out_rate)?);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE_HZ,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create("reply.wav", spec)?;
    let src = Address::new(MYCALL, MYCALL_SSID)?;
    let dest = Address::new(TOCALL, 0)?;
    let mut frames = 0usize;
    for plan in &plans {
        for info in build_responses(plan)? {
            // Wrap the message payload in a UI frame and modulate it.
            // The AX.25 worst case, so any reply we can build fits.
            let mut frame_buf = [0u8; MAX_FRAME_BYTES];
            let len = tx.build_frame_raw(dest, src, &[], &info, &mut frame_buf)?;
            for sample in tx.frame_samples_i16(&frame_buf[..len]) {
                writer.write_sample(sample)?;
            }
            frames += 1;
        }
    }
    writer.finalize()?;
    println!("wrote reply.wav: {frames} response frame(s) at {SAMPLE_RATE_HZ} Hz");
    Ok(())
}
