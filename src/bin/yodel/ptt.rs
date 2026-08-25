//! `yodel ptt`: key a transmitter over a serial control line.
//!
//! # Why this is a wrapper and not a transmit mode
//!
//! The rest of this CLI does protocol and DSP and leaves audio to the
//! operating system: `gen` and `serve` write raw PCM to a pipe, and
//! `decode` reads it. Keeping PTT to the same shape is not tidiness,
//! it is the only arrangement that keys correctly.
//!
//! Push-to-talk has to be asserted **before** the first sample reaches
//! the air and released **after** the last one. A process that writes
//! PCM into a pipe cannot know either moment: the player downstream
//! buffers, so the writer finishes early and would unkey mid-packet.
//! The process that owns playback is the only one that knows when the
//! audio is really gone, so this subcommand *runs* that process and
//! holds the line for its whole lifetime.
//!
//! ```text
//! yodel ptt --port /dev/ttyUSB0 -- sox packet.wav -t alsa default
//! ```
//!
//! # Failure mode
//!
//! This is the one place in the crate that can put a signal on the air
//! by itself, so every path out of it drops the line: normal exit, an
//! error, a panic, a child that fails to start, and a child that never
//! finishes. [`Keyed`] does it in `Drop`, and `--max` bounds the worst
//! case even if the child hangs, because a stuck transmitter jams a
//! shared channel for everyone in range and can cook the radio's
//! output stage.
//!
//! One hazard is worth stating because it is invisible and surprising:
//! **some USB-serial drivers assert RTS the instant the port is
//! opened.** MEASURED on a CP2102N under macOS, the modem-status
//! register reads `0x0026` immediately after `open`, which is CTS plus
//! RTS plus DTR. On a wired-up interface that keys the transmitter
//! before a single line of user code runs, and it keeps keying it for
//! as long as any program holds the port. So [`open_port`] drops both
//! lines before doing anything else, and any tool that opens the same
//! port without doing so will key the radio by accident.

use std::process::Command;
use std::time::{Duration, Instant};

use clap::{Args, ValueEnum};

/// Which modem control line keys the radio.
#[derive(Copy, Clone, PartialEq, Eq, Debug, ValueEnum)]
pub enum Signal {
    /// Request To Send. The usual choice, and what a Digirig-style
    /// interface ties its open-collector PTT switch to.
    Rts,
    /// Data Terminal Ready. Some home-built interfaces use this line
    /// instead, and a few use it for a second function such as CW.
    Dtr,
}

impl Signal {
    /// The name to print, matching the value the user typed.
    const fn name(self) -> &'static str {
        match self {
            Signal::Rts => "RTS",
            Signal::Dtr => "DTR",
        }
    }

    /// How the keyed state reads for a log line, `--invert` included.
    ///
    /// Which way the line is driven is exactly the thing an operator
    /// gets wrong when a radio will not key, so the message says what
    /// was actually done rather than only which line was chosen.
    pub const fn describe(self, invert: bool) -> &'static str {
        match (self, invert) {
            (Signal::Rts, false) => "RTS high",
            (Signal::Rts, true) => "RTS low",
            (Signal::Dtr, false) => "DTR high",
            (Signal::Dtr, true) => "DTR low",
        }
    }
}

/// Arguments of `yodel ptt`.
#[derive(Args)]
pub struct PttArgs {
    /// Serial port that keys the radio, e.g. `/dev/ttyUSB0`,
    /// `/dev/cu.usbserial-1110` or `COM3`. Prefer a stable path such
    /// as `/dev/serial/by-id/...` where the platform offers one: a
    /// bare `ttyUSB0` moves when other adapters are plugged in, and
    /// keying the wrong device transmits from the wrong radio.
    #[arg(long, value_name = "DEVICE", required_unless_present = "list")]
    port: Option<String>,

    /// List the serial ports this machine can see, and exit.
    #[arg(long)]
    list: bool,

    /// Control line to assert [default: rts]
    #[arg(long, value_enum, default_value_t = Signal::Rts)]
    signal: Signal,

    /// Key on the line being LOW rather than high, for interfaces that
    /// invert it.
    #[arg(long)]
    invert: bool,

    /// Milliseconds to hold the line before starting the player, so
    /// the transmitter's output stage settles and the receiving end's
    /// squelch opens before any data arrives.
    ///
    /// This is not the same thing as TXDelay. A packet built by this
    /// crate already opens with 32 HDLC flags, 213 ms at 1200 baud,
    /// which is the on-air lead-in; this is the electrical one.
    #[arg(long, value_name = "MS", default_value_t = 300)]
    lead: u64,

    /// Milliseconds to keep holding after the player exits, so a
    /// buffered tail is not cut off.
    #[arg(long, value_name = "MS", default_value_t = 150)]
    tail: u64,

    /// Hard limit on total key-down time. Exceeding it drops the line,
    /// kills the player and exits non-zero.
    ///
    /// A safety net, not a schedule. A hung player would otherwise hold
    /// a transmitter up indefinitely, which jams the channel for
    /// everyone in range and can destroy the radio's output stage.
    #[arg(long, value_name = "MS", default_value_t = 60_000)]
    max: u64,

    /// Key for this many milliseconds and release, running nothing.
    /// For checking that the interface keys at all before trusting it
    /// with a transmission. Cannot be combined with a command.
    #[arg(long, value_name = "MS", conflicts_with = "command")]
    hold: Option<u64>,

    /// The player to run while the line is held, after `--`. It owns
    /// the audio, so the line is released when it exits.
    #[arg(last = true, value_name = "COMMAND")]
    command: Vec<String>,
}

/// A held PTT line that releases itself.
///
/// The release lives in `Drop` rather than at the end of the happy
/// path so that an error return, a `?`, or a panic in the player
/// plumbing cannot leave a transmitter keyed.
pub struct Keyed {
    port: Box<dyn serialport::SerialPort>,
    signal: Signal,
    invert: bool,
}

impl Keyed {
    /// Asserts the line and returns the guard that will release it.
    pub fn assert(mut port: Box<dyn serialport::SerialPort>, signal: Signal, invert: bool) -> Self {
        let _ = set_line(&mut port, signal, !invert);
        Self {
            port,
            signal,
            invert,
        }
    }
}

impl Drop for Keyed {
    fn drop(&mut self) {
        // Nothing useful to do with a failure here, and returning early
        // would skip the other line. Try, and let the OS dropping the
        // modem lines at close be the backstop.
        let _ = set_line(&mut self.port, self.signal, self.invert);
    }
}

/// Drives one modem control line to `high`.
fn set_line(
    port: &mut Box<dyn serialport::SerialPort>,
    signal: Signal,
    high: bool,
) -> Result<(), serialport::Error> {
    match signal {
        Signal::Rts => port.write_request_to_send(high),
        Signal::Dtr => port.write_data_terminal_ready(high),
    }
}

/// Opens the port with **both** control lines already released.
///
/// See the module docs: some drivers assert RTS at open, which keys the
/// radio before any of this code runs. Dropping both lines first is
/// what makes opening the port safe.
pub fn open_port(path: &str, invert: bool) -> Result<Box<dyn serialport::SerialPort>, String> {
    let mut port = serialport::new(path, 9600)
        .timeout(Duration::from_millis(50))
        .open()
        .map_err(|e| format!("cannot open serial port {path}: {e}"))?;
    let _ = set_line(&mut port, Signal::Rts, invert);
    let _ = set_line(&mut port, Signal::Dtr, invert);
    Ok(port)
}

/// Runs `yodel ptt`.
///
/// # Errors
///
/// A port that cannot be opened, a player that cannot be started, a
/// player that exits non-zero, or a transmission that runs past
/// `--max`.
pub fn ptt(args: &PttArgs) -> Result<(), String> {
    if args.list {
        return list_ports();
    }
    let path = args
        .port
        .as_deref()
        .ok_or_else(|| "--port is required unless --list is given".to_string())?;

    if args.hold.is_none() && args.command.is_empty() {
        return Err(
            "nothing to do: give --hold <MS> to key for a fixed time, or \
             `-- <command>` to key while a player runs"
                .to_string(),
        );
    }
    if args.lead + args.tail >= args.max {
        return Err(format!(
            "--lead {} plus --tail {} already reaches --max {}, so the \
             transmission could never run",
            args.lead, args.tail, args.max
        ));
    }

    let port = open_port(path, args.invert)?;
    let started = Instant::now();
    let keyed = Keyed::assert(port, args.signal, args.invert);
    eprintln!(
        "PTT on  ({} {} on {path})",
        args.signal.name(),
        if args.invert { "low" } else { "high" }
    );

    let outcome = transmit(args, started);

    drop(keyed);
    eprintln!("PTT off ({} ms keyed)", started.elapsed().as_millis());
    outcome
}

/// The keyed section: settle, run the player, let the tail out.
///
/// Split out so that every `?` in it unwinds through the caller's
/// `drop(keyed)` rather than returning past it.
fn transmit(args: &PttArgs, started: Instant) -> Result<(), String> {
    let budget = Duration::from_millis(args.max);
    std::thread::sleep(Duration::from_millis(args.lead));

    if let Some(ms) = args.hold {
        std::thread::sleep(Duration::from_millis(
            ms.min(args.max.saturating_sub(args.lead)),
        ));
        return Ok(());
    }

    let (program, rest) = args
        .command
        .split_first()
        .ok_or_else(|| "no player command given after `--`".to_string())?;
    let mut child = Command::new(program)
        .args(rest)
        .spawn()
        .map_err(|e| format!("cannot start player {program}: {e}"))?;

    // Poll rather than wait, so `--max` can still fire on a hung
    // player. A transmitter held up by a wedged process is the failure
    // this bound exists for.
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("waiting on player: {e}"))?
        {
            break status;
        }
        if started.elapsed() >= budget {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "key-down exceeded --max {} ms: player killed and PTT released",
                args.max
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    std::thread::sleep(Duration::from_millis(args.tail));
    if !status.success() {
        return Err(format!("player {program} exited with {status}"));
    }
    Ok(())
}

/// Prints the serial ports this machine can see.
fn list_ports() -> Result<(), String> {
    let ports =
        serialport::available_ports().map_err(|e| format!("cannot enumerate serial ports: {e}"))?;
    let mut out = crate::shared::Output::new();
    if ports.is_empty() {
        out.line(format_args!("no serial ports found"))?;
        return out.finish();
    }
    for p in ports {
        match p.port_type {
            serialport::SerialPortType::UsbPort(info) => out.line(format_args!(
                "{}  USB {:04x}:{:04x} {}",
                p.port_name,
                info.vid,
                info.pid,
                info.product.unwrap_or_default()
            ))?,
            other => out.line(format_args!("{}  {other:?}", p.port_name))?,
        }
    }
    out.finish()
}
