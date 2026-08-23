//! Encode and decode APRS without a radio, a sound card, or a network.
//!
//! Most of this crate is a modem, but the APRS layer stands on its own.
//! If you have packets as text, from an APRS-IS capture, a log file, or
//! another program, you can decode them here. If you want to produce
//! packets for something else to transmit, you can build them here.
//! Neither direction touches audio.
//!
//! ```sh
//! cargo run --example aprs_offline --features std,aprs,micE
//! ```
//!
//! The four parts below are:
//!
//! 1. building a typed packet and serializing it to the wire bytes,
//! 2. decoding a line of TNC2 monitor text, which is the format
//!    APRS-IS, most TNCs and most log files use,
//! 3. reading a whole capture and summarizing it,
//! 4. round-tripping, and where byte-exactness holds.
//!
//! # Where the buffer sizes come from
//!
//! Builders here write into a buffer you supply, which raises the
//! question of how big it has to be. There is no `INFO_MAX` constant to
//! reach for, and that is on purpose: a position comment, a status text
//! and an object payload are all caller-supplied slices that the packet
//! type does not bound, so any constant would be a guess presented as a
//! guarantee.
//!
//! Two things make the size checkable instead:
//!
//! * An undersized buffer is never silently truncated. `build` returns
//!   [`AprsError::BufferTooSmall`], which carries `needed`, so you can
//!   size from the failure and retry.
//! * A real bound exists one layer down. `tnc::MAX_FRAME_BYTES` is 330,
//!   covering the longest AX.25 address field, control, PID, a 256-byte
//!   information field and the FCS. An information field that fits a
//!   standard AX.25 frame therefore fits in 256 bytes.
//!
//! The 256-byte buffers below are that bound, not an arbitrary number.
//! If you have a heap, [`aprs_offline_alloc.rs`](aprs_offline_alloc.rs)
//! shows the same work with the sizing done for you.

use warble::aprs::monitor::MonitorLine;
use warble::aprs::{
    Addressee, AprsPacket, Decoded, DecodedKind, Latitude, Longitude, Message, MessageContent,
    Position, Status, Symbol,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_packets()?;
    decode_one_line()?;
    summarize_a_capture();
    round_trip()?;
    Ok(())
}

/// 1. Build typed packets and serialize them.
///
/// Every builder writes into a caller-provided buffer and returns the
/// length used, so none of this allocates.
fn build_packets() -> Result<(), Box<dyn std::error::Error>> {
    println!("── building ──");
    // 256 is the information field a standard AX.25 frame can carry.
    // See the sizing note in the module docs.
    let mut buf = [0u8; 256];

    // A position report. The types make an invalid report hard to
    // construct: coordinates are validated, and the symbol is a typed
    // pair rather than two loose bytes.
    let position = Position::new(
        Latitude::from_degrees(49.0583)?,
        Longitude::from_degrees(-72.0292)?,
        Symbol::CAR,
    )
    .with_comment(b"warble offline");
    let n = position.build(&mut buf)?;
    println!("position   {}", show(&buf[..n]));

    // A status report.
    let status = Status {
        text: b"bench testing",
    };
    let n = status.build(&mut buf)?;
    println!("status     {}", show(&buf[..n]));

    // A directed message with an ID, which asks the recipient to ack.
    let message = Message {
        addressee: Addressee::new(b"N1CALL")?,
        content: MessageContent::Text {
            text: b"see you at the hamfest",
            id: Some(b"42"),
        },
    };
    let n = message.build(&mut buf)?;
    println!("message    {}", show(&buf[..n]));

    // Those are information fields. Wrapping one in a full TNC2 line is
    // just text, and this is the form APRS-IS and most logs use.
    let n = position.build(&mut buf)?;
    println!(
        "\nas a TNC2 line:\n  N0CALL-7>APRS,WIDE1-1:{}",
        show(&buf[..n])
    );
    println!();
    Ok(())
}

/// 2. Decode one line of TNC2 monitor text.
fn decode_one_line() -> Result<(), Box<dyn std::error::Error>> {
    println!("── decoding one line ──");
    let raw = b"KD8XYZ-9>APRS,WIDE1-1,qAR,W8ABC-10:!4237.14N/08325.55W>073/019/A=000712 mobile";

    let line = MonitorLine::parse(raw)?;
    println!("source     {}", show(line.source));
    println!("dest       {}", show(line.dest));
    println!("path       {}", show(line.path));
    println!(
        "heard on   {}",
        if line.is_from_rf() {
            "RF"
        } else {
            "the internet"
        }
    );
    if let Some(gate) = line.igate() {
        println!("gated by   {}", show(gate));
    }
    for hop in line.hops() {
        println!(
            "  hop      {:<10} {}",
            show(hop.call),
            if hop.repeated { "(used)" } else { "" }
        );
    }

    // `decoded` is total: it always returns something, and an
    // unparseable field comes back labelled rather than dropped.
    match line.decoded().kind {
        DecodedKind::Packet(AprsPacket::Position(p)) => {
            println!(
                "position   {:.4}, {:.4}  comment {:?}",
                p.latitude.to_degrees(),
                p.longitude.to_degrees(),
                String::from_utf8_lossy(p.comment)
            );
        }
        other => println!("decoded to {other:?}"),
    }
    println!();
    Ok(())
}

/// 3. Read a capture and summarize it.
///
/// These lines are the shape APRS-IS delivers, including a Mic-E frame
/// whose position lives half in the destination, a message whose
/// information field opens with its own colon, and one line that does
/// not parse.
fn summarize_a_capture() {
    println!("── summarizing a capture ──");
    let capture: &[&[u8]] = &[
        b"KT4ROY-10>APRS,TCPIP*,qAC,SA7AUX:!3145.81N/08556.54W>on the air",
        b"K3RTA>APWW10,WIDE1-1,qAR,W3ISR-10:;DSP_Trp_2*111111z3936.34N/07543.72W!unit 4",
        b"KQ4ZAX-5>APFII0,TCPIP*,qAC,APRSFI::OTA      :CQ{D447B",
        b"KB9ZI>APRS,TCPIP*,qAC,T2LANE:@210354z4441.41N/09324.33W_192/000g000t073",
        b"W8JES>APU25N,TCPIP*,qAC,T2MCI:>Findlay's IGate",
        b"NOTAPACKET",
    ];

    let (mut rf, mut net, mut bad) = (0, 0, 0);
    for raw in capture {
        let Ok(line) = MonitorLine::parse(raw) else {
            bad += 1;
            continue;
        };
        if line.is_from_rf() {
            rf += 1;
        } else {
            net += 1;
        }
        let decoded = line.decoded();
        println!(
            "  {:<10} {:<14} {}",
            show(line.source),
            kind_name(&decoded),
            show(&decoded.info[..decoded.info.len().min(38)])
        );
    }
    println!("\n  {rf} from RF, {net} from the internet, {bad} unparseable\n");
}

/// 4. Round-tripping, and where byte-exactness holds.
fn round_trip() -> Result<(), Box<dyn std::error::Error>> {
    println!("── round trip ──");
    let mut buf = [0u8; 256];

    let original = Position::new(
        Latitude::from_degrees(49.0583)?,
        Longitude::from_degrees(-72.0292)?,
        Symbol::CAR,
    )
    .with_comment(b"round trip");
    let n = original.build(&mut buf)?;
    let wire = &buf[..n];

    // Parse the bytes back and rebuild them.
    let mut again = [0u8; 256];
    let reparsed = match Decoded::decode(wire).kind {
        DecodedKind::Packet(AprsPacket::Position(p)) => p,
        other => return Err(format!("expected a position, got {other:?}").into()),
    };
    let m = reparsed.build(&mut again)?;

    println!("built    {}", show(wire));
    println!("rebuilt  {}", show(&again[..m]));
    println!(
        "byte-exact: {}",
        if wire == &again[..m] { "yes" } else { "no" }
    );

    // Not every payload rebuilds byte-for-byte. A weather report whose
    // sender wrote the optional fields in a different order comes back
    // in this crate's canonical order, which is legal on the wire but
    // not identical. docs/APRS_CONFORMANCE.md records the measurement.
    // If you are forwarding traffic rather than interpreting it, send
    // `Decoded::info`, which is the bytes exactly as received.
    let received = b"@210354z4441.41N/09324.33W_192/000g000t073r000p000h50b09900";
    let decoded = Decoded::decode(received);
    println!(
        "\nforwarding a weather report unchanged: {}",
        decoded.info == received
    );
    Ok(())
}

fn kind_name(decoded: &Decoded<'_>) -> &'static str {
    match &decoded.kind {
        DecodedKind::Packet(p) => match p {
            AprsPacket::Position(_)
            | AprsPacket::PositionCs(_)
            | AprsPacket::PositionTimestamped(_) => "position",
            AprsPacket::PositionWeather(_) | AprsPacket::Weather(_) => "weather",
            AprsPacket::Status(_) => "status",
            AprsPacket::Message(_) => "message",
            AprsPacket::Object(_) => "object",
            AprsPacket::Item(_) => "item",
            AprsPacket::Telemetry(_) => "telemetry",
            AprsPacket::Capabilities(_) => "capabilities",
            _ => "other",
        },
        #[cfg(feature = "micE")]
        DecodedKind::MicE(_) => "mic-e",
        DecodedKind::Nmea(_) => "nmea",
        DecodedKind::Ultimeter(_) => "ultimeter",
        DecodedKind::ThirdParty(_) => "third-party",
        DecodedKind::NeedsDestination { .. } => "needs-dest",
        DecodedKind::Unsupported { .. } => "unsupported",
        DecodedKind::Malformed { .. } => "malformed",
        _ => "other",
    }
}

/// Information fields are arbitrary bytes. Render them lossily, and
/// only for display.
fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
