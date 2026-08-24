//! Proof of the two hostile-input guards in the `warble aprsis` client.
//!
//! `aprsis` is the only subcommand that reads from a network peer this
//! crate does not control, and it runs for days at a time. Two of its
//! failure modes are only reachable from a misbehaving or hostile
//! server, so they get in-process tests rather than a live connection:
//!
//! * `read_bounded_line` must refuse to grow without limit when a peer
//!   streams bytes and never sends a terminator;
//! * `downstream_closed` must tell "our own reader went away" apart
//!   from "the connection failed", because only the second one is
//!   worth reconnecting for — and the reconnect target is a volunteer
//!   Tier 2 server.
//!
//! The module is included with `#[path]`, the technique `tests/serve.rs`
//! uses, so the test drives the same code the binary runs.
#![cfg(all(feature = "cli", feature = "std"))]

#[path = "../src/bin/warble/aprsis.rs"]
#[allow(dead_code, unused_imports)]
mod warble_bin;

use std::io::{BufReader, ErrorKind};

use warble_bin::{LineOutcome, downstream_closed, read_bounded_line};

use warble::aprs::monitor::LINE_MAX;

/// Reads every line a slice contains, returning the outcomes.
fn read_all(input: &[u8], max: usize) -> Vec<(LineOutcome, Vec<u8>)> {
    let mut reader = BufReader::new(input);
    let mut out = Vec::new();
    loop {
        let mut buf = Vec::new();
        let outcome = read_bounded_line(&mut reader, &mut buf, max).expect("in-memory read");
        out.push((outcome, buf));
        if matches!(outcome, LineOutcome::Eof) {
            return out;
        }
    }
}

#[test]
fn ordinary_lines_come_back_whole() {
    let outcomes = read_all(b"first\r\nsecond\r\n", LINE_MAX);
    assert_eq!(outcomes[0].0, LineOutcome::Line);
    assert_eq!(outcomes[0].1, b"first\r\n");
    assert_eq!(outcomes[1].0, LineOutcome::Line);
    assert_eq!(outcomes[1].1, b"second\r\n");
    assert_eq!(outcomes[2].0, LineOutcome::Eof);
}

/// A line at exactly the cap is still a line: the bound is inclusive,
/// so a maximum-length APRS-IS packet is never dropped.
#[test]
fn a_line_at_exactly_the_cap_is_accepted() {
    let mut input = vec![b'x'; LINE_MAX - 1];
    input.push(b'\n');
    let outcomes = read_all(&input, LINE_MAX);
    assert_eq!(outcomes[0].0, LineOutcome::Line);
    assert_eq!(outcomes[0].1.len(), LINE_MAX);
}

/// The bug this guards: `BufRead::read_until` has no upper bound, so a
/// peer that never sends a terminator grew the buffer until the process
/// died. The read must stop at the cap instead.
#[test]
fn an_endless_line_is_bounded_not_buffered() {
    // Far more than the cap, with no terminator anywhere.
    let input = vec![b'x'; LINE_MAX * 8];
    let mut reader = BufReader::new(&input[..]);
    let mut buf = Vec::new();
    let outcome = read_bounded_line(&mut reader, &mut buf, LINE_MAX).expect("in-memory read");

    // The peer ran out before sending a terminator, so this reads as a
    // closed connection -- and, crucially, the buffer never grew past
    // the cap on the way there.
    assert_eq!(outcome, LineOutcome::Eof);
    assert!(
        buf.len() <= LINE_MAX,
        "buffer grew to {} bytes, past the {LINE_MAX}-byte cap",
        buf.len()
    );
}

/// An overlong line is dropped whole, and the reader resynchronises on
/// the next one rather than emitting the tail as a fake packet.
#[test]
fn an_oversized_line_is_dropped_and_the_next_one_survives() {
    let mut input = vec![b'x'; LINE_MAX * 3];
    input.extend_from_slice(b"\r\n");
    input.extend_from_slice(b"N0CALL>APRS:>real packet\r\n");

    let outcomes = read_all(&input, LINE_MAX);
    assert_eq!(outcomes[0].0, LineOutcome::Oversized);
    assert!(
        outcomes[0].1.is_empty(),
        "an oversized line must yield nothing, got {:?}",
        String::from_utf8_lossy(&outcomes[0].1)
    );
    assert_eq!(
        outcomes[1].0,
        LineOutcome::Line,
        "the reader must resynchronise on the following line"
    );
    assert_eq!(outcomes[1].1, b"N0CALL>APRS:>real packet\r\n");
}

/// A peer that streams forever without a terminator is abandoned rather
/// than spun on until the read timeout.
#[test]
fn a_peer_that_never_terminates_is_abandoned() {
    /// Yields `b'x'` forever and never a newline.
    struct Endless;
    impl std::io::Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            buf.fill(b'x');
            Ok(buf.len())
        }
    }

    let mut reader = BufReader::new(Endless);
    let mut buf = Vec::new();
    let err = read_bounded_line(&mut reader, &mut buf, LINE_MAX)
        .expect_err("an endless stream with no terminator must error, not spin");
    assert!(
        err.to_string().contains("no line terminator"),
        "unexpected error: {err}"
    );
}

/// Only a closed downstream ends the run; every other error is worth a
/// reconnect. Getting this backwards either hammers a volunteer server
/// for output nobody reads, or gives up on a recoverable blip.
#[test]
fn only_a_broken_pipe_counts_as_a_closed_downstream() {
    assert!(downstream_closed(&std::io::Error::new(
        ErrorKind::BrokenPipe,
        "head exited"
    )));

    for kind in [
        ErrorKind::ConnectionReset,
        ErrorKind::ConnectionRefused,
        ErrorKind::ConnectionAborted,
        ErrorKind::TimedOut,
        ErrorKind::UnexpectedEof,
        ErrorKind::WouldBlock,
        ErrorKind::NotConnected,
    ] {
        assert!(
            !downstream_closed(&std::io::Error::new(kind, "server side")),
            "{kind:?} is a server-side failure and must still reconnect"
        );
    }
}
