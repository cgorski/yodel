//! Encode and decode APRS the ergonomic way, on a machine with a heap.
//!
//! [`aprs_offline.rs`](aprs_offline.rs) shows the allocation-free API:
//! every builder writes into a buffer you supply and returns the length
//! it used. That is the right shape on a microcontroller, where the
//! whole point is that nothing allocates behind your back.
//!
//! On a desktop or a server it is needless ceremony. With the `alloc`
//! feature the same builders offer `to_vec`, which sizes the buffer for
//! you, and `format_line` assembles a whole TNC2 line.
//!
//! ```sh
//! cargo run --example aprs_offline_alloc --features std,alloc,aprs,micE
//! ```
//!
//! # On buffer sizes
//!
//! The fixed-buffer API raises the question of how big a buffer has to
//! be, and this crate declines to answer it with a constant. There is
//! no defensible `INFO_MAX`, because a position comment, a status text
//! and an object payload are all caller-supplied slices that the
//! packet type does not bound. A constant would be a guess presented
//! as a guarantee.
//!
//! What you get instead:
//!
//! * **Overflow is always reported.** `build` returns
//!   `AprsError::BufferTooSmall`, and that error carries `needed`, so a
//!   caller who cannot pick a size up front can size from the failure
//!   and retry. Nothing is ever silently truncated.
//! * **A real bound exists one layer down.** `tnc::MAX_FRAME_BYTES` is
//!   330: the longest AX.25 address field, control, PID, a 256-byte
//!   information field, and the FCS. So an information field that fits
//!   a standard AX.25 frame fits in 256 bytes, and that is what `to_vec`
//!   sizes against.
//!
//! The last section below demonstrates both of those.

use yodel::aprs::monitor::{MonitorLine, format_line};
use yodel::aprs::{
    Addressee, AprsError, AprsPacket, Latitude, Longitude, Message, MessageContent, Position,
    Status, Symbol,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    the_ceremony_you_can_skip()?;
    build_a_whole_line()?;
    sizing_without_a_constant()?;
    Ok(())
}

/// The same packet, built both ways.
fn the_ceremony_you_can_skip() -> Result<(), Box<dyn std::error::Error>> {
    println!("── two ways to build the same packet ──");

    let packet = AprsPacket::Position(
        Position::new(
            Latitude::from_degrees(49.0583)?,
            Longitude::from_degrees(-72.0292)?,
            Symbol::CAR,
        )
        .with_comment(b"yodel"),
    );

    // Allocation-free: you own the buffer and the length.
    let mut buf = [0u8; 256];
    let n = packet.build(&mut buf)?;
    let no_alloc = &buf[..n];

    // With `alloc`: the buffer is sized for you.
    let with_alloc = packet.to_vec()?;

    println!("fixed buffer  {}", text(no_alloc));
    println!("to_vec        {}", text(&with_alloc));
    assert_eq!(no_alloc, &with_alloc[..], "same bytes either way");
    println!("identical: yes\n");
    Ok(())
}

/// Assemble a complete TNC2 line, the form APRS-IS and log files use.
fn build_a_whole_line() -> Result<(), Box<dyn std::error::Error>> {
    println!("── building whole lines ──");

    let packets: Vec<(&[u8], AprsPacket)> = vec![
        (
            b"WIDE1-1",
            AprsPacket::Position(
                Position::new(
                    Latitude::from_degrees(51.5074)?,
                    Longitude::from_degrees(-0.1278)?,
                    Symbol::CAR,
                )
                .with_comment(b"mobile"),
            ),
        ),
        (
            b"",
            AprsPacket::Status(Status {
                text: b"on the air",
            }),
        ),
        (
            b"WIDE2-1",
            AprsPacket::Message(Message {
                addressee: Addressee::new(b"N1CALL")?,
                content: MessageContent::Text {
                    text: b"meet at 14:00",
                    id: Some(b"7"),
                },
            }),
        ),
    ];

    let mut capture: Vec<Vec<u8>> = Vec::new();
    for (path, packet) in &packets {
        let line = format_line(b"N0CALL-7", b"APRS", path, &packet.to_vec()?);
        println!("  {}", text(&line));
        capture.push(line);
    }

    // Read the capture straight back. This is the loop an offline tool
    // runs over a log file.
    println!("\n  parsed back:");
    for line in &capture {
        let parsed = MonitorLine::parse(line)?;
        println!(
            "    {:<10} {:<8} {}",
            text(parsed.source),
            if parsed.path.is_empty() {
                "direct".into()
            } else {
                text(parsed.path)
            },
            text(parsed.info)
        );
    }
    println!();
    Ok(())
}

/// How to size a buffer without a constant that cannot exist.
fn sizing_without_a_constant() -> Result<(), Box<dyn std::error::Error>> {
    println!("── sizing ──");

    // A comment long enough to overflow the 32-byte buffer below.
    let long = "the quick brown fox ".repeat(6);
    let packet = AprsPacket::Position(
        Position::new(
            Latitude::from_degrees(0.0)?,
            Longitude::from_degrees(0.0)?,
            Symbol::CAR,
        )
        .with_comment(long.as_bytes()),
    );

    // 1. Ask for too little. The error says how much was needed, so you
    //    never have to guess twice.
    let mut small = [0u8; 32];
    match packet.build(&mut small) {
        Err(AprsError::BufferTooSmall { needed, .. }) => {
            println!("32 bytes was too small; the error reports needed = {needed}");
            let mut right = vec![0u8; needed];
            let n = packet.build(&mut right)?;
            println!("retried with {needed}, wrote {n}");
        }
        Err(e) => return Err(e.into()),
        Ok(n) => println!("unexpectedly fit in 32 bytes ({n})"),
    }

    // 2. Or size against the frame layer, which does have a bound.
    // `tnc::MAX_FRAME_BYTES` is 330: the longest address field, control,
    // PID, a 256-byte information field and the FCS. This example does
    // not enable the `tnc` feature, so the bound is quoted rather than
    // imported.
    println!(
        "\nA 330-byte AX.25 frame leaves 256 bytes of information field,\n\
         and that is what to_vec allocates against:"
    );
    println!("  to_vec produced {} bytes", packet.to_vec()?.len());

    // 3. And nothing is ever truncated. A field too long for a frame is
    //    an error, not a short packet.
    let absurd = "x".repeat(400);
    let too_big = AprsPacket::Status(Status {
        text: absurd.as_bytes(),
    });
    match too_big.to_vec() {
        Err(AprsError::BufferTooSmall { needed, .. }) => {
            println!("  a 400-byte status reports needed = {needed}, rather than truncating");
        }
        Err(e) => println!("  rejected: {e}"),
        Ok(v) => println!("  built {} bytes", v.len()),
    }
    Ok(())
}

/// Information fields are arbitrary bytes. Render lossily, for display
/// only.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
