# yodel architecture

The contributor front door: how the crate is layered, what every file
does, where the seams are, which of them creak, and which modes fit
them. Pairs with [`../README.md`](../README.md) (user-facing feature
table and examples) and [`../CONTRIBUTING.md`](../CONTRIBUTING.md)
(build gates, design invariants, the binding scope rules, and the
rules around `reference/`).

## Layer diagram

Two physical-layer (PHY) front ends feed one shared protocol stack:

```text
                 PCM samples (i16 / f32)
                          │
        ┌─────────────────┴──────────────────┐
        │ PHY front ends                     │
        │                                    │
        │  tone AFSK                G3RUH    │
        │  discriminator.rs         baseband │
        │  (Discriminator trait,    .rs      │
        │   QuadratureCorrelator)   (9600 Bd │
        │  + slicer.rs (PLL)        pulse    │
        │  modulator.rs (CPFSK TX)  wave)    │
        └────────┬──────────────────┬────────┘
                 │   bit sources    │
                 │                  │
                 │           scrambler.rs (G3RUH
                 │           LFSR x^17+x^12+1)
                 └────────┬─────────┘
                          │ raw line bits
                    nrzi.rs (line coding)
                          │
                 ax25/hdlc.rs (flags, bit stuffing)
                          │
       ax25/{addr,fcs,frame}.rs (addresses, CRC, UI frames)
                          │
        aprs.rs + aprs/* (position, status, message, …)

side stacks (standalone, attach at the frame/byte level):
    kiss.rs                serial framing for host <-> TNC links
    rs.rs + fx25.rs        RS(255,k) FEC + FX.25 correlation-tag framing
    rs.rs + il2p.rs        IL2P framing: replaces HDLC + NRZI whole,
                           same RS codec, same modulator underneath

orchestration:
    tnc.rs                 TncTransmitter / TncReceiver — composes the
                           whole column above into two types
profiles / presets:
    types.rs               SampleRate, BaudRate, TonePair, ModemProfile
CLI:
    bin/yodel/            the `yodel` binary: main.rs (clap tree +
                           dispatch), one module per subcommand
                           (decode, encode, gen, bench, serve, aprsis,
                           level, ptt, wspr, ft8, m17), shared.rs
                           (modem-flag plumbing and the stdout writer)
```

Every layer is independently usable and independently feature-gated;
the arrows are plain function/type composition, not trait objects.

## Module map

| file | one line |
|---|---|
| `src/lib.rs` | Crate root: feature-gated module wiring, re-exports, README doctests. |
| `src/error.rs` | `ConfigError`: validated-constructor failures for the config types. |
| `src/geo.rs` | Integer-only geography, never feature-gated: `Latitude`/`Longitude` as `i64` counts of 1/342 833 400 000 000 of a degree, the unit chosen so every APRS position format's denominator divides it exactly (`units()` reads the count); `Coordinates` pairing them with an `Ambiguity`; `Ambiguity` with `step()`/`mask()`, so a declared ambiguity can be **applied** to a coordinate and not only reported; `MaidenheadGrid`; equirectangular `distance_to`/`bearing_to`. Shared by APRS, WSPR and FT8. |
| `src/units.rs` | Physical quantities with the unit in the type, never feature-gated: `Distance`, `Speed`, `Temperature`, `Pressure`, `Rainfall`, `Power`, `Bearing`, `Humidity`. Each carries one canonical integer unit, named constructors and accessors. |
| `src/types.rs` | Validated config primitives (`SampleRate`, `BaudRate`, `TonePair`), `ModemProfile` presets, `DevicePreset`, `ModulationScheme`, `Bit`, shared sine table. |
| `src/modulator.rs` | Continuous-phase FSK modulator: phase-accumulator tone synthesis, fractional samples-per-bit, `i16`/`f32` sample iterators. |
| `src/demodulator.rs` | Tone-AFSK demodulator: composes a `Discriminator` with the PLL `Slicer`; sample in, bit out. |
| `src/discriminator.rs` | The `Discriminator` trait (PCM sample → signed mark/space metric) and `QuadratureCorrelator`, its default implementation. |
| `src/slicer.rs` | PLL bit slicer: recovers the bit clock from metric zero crossings, one decision per bit cell. |
| `src/baseband.rs` | G3RUH 9600-baud direct-baseband PHY: band-limited pulse-shaping modulator and matched demodulator (replaces the tone discriminator; the crate's PLL `Slicer` is reused inside it). |
| `src/scrambler.rs` | G3RUH multiplicative LFSR scrambler/descrambler pair (x¹⁷ + x¹² + 1). |
| `src/nrzi.rs` | NRZI differential line coding: streaming encoder/decoder. |
| `src/ax25.rs` | AX.25 module root: re-exports, frame↔samples glue (`tx_i16`, `FrameReceiver`). |
| `src/ax25/addr.rs` | AX.25 address fields: callsign + SSID encoding/parsing. |
| `src/ax25/fcs.rs` | CRC-16/X.25 frame check sequence. |
| `src/ax25/frame.rs` | UI-frame build/parse (addresses, control 0x03, PID 0xF0). |
| `src/ax25/hdlc.rs` | HDLC framing: 0x7E flags, zero-bit stuffing, streaming deframer. |
| `src/aprs.rs` | APRS module root: `AprsPacket` dispatch, data-type identifiers, UI-frame glue. |
| `src/aprs/position.rs` | Position reports: uncompressed, base-91 compressed (all csT variants), timestamped forms. |
| `src/aprs/extension.rs` | The 7-byte data extension after the symbol: course/speed, wind, `PHG`/`PHGR`, `RNG`, `DFS`, plus `/A=` altitude. |
| `src/aprs/status.rs` | Status reports. |
| `src/aprs/message.rs` | Directed messages, acks/rejects, addressees. |
| `src/aprs/weather.rs` | Weather reports. |
| `src/aprs/telemetry.rs` | Telemetry reports. |
| `src/aprs/object.rs` | Object and item reports; `Timestamp`. |
| `src/aprs/symbol.rs` | APRS symbol table/code pairs, overlays, `describe()` lookup. |
| `src/aprs/mic_e.rs` | Mic-E compressed positions (split across destination + info fields). |
| `src/aprs/nmea.rs` | Raw NMEA 0183 sentences (`$`). **Receive-only.** |
| `src/aprs/ultimeter.rs` | Peet Bros Ultimeter weather records (`$ULTW`, `!!`, `*`, `#`). **Receive-only.** |
| `src/aprs/thirdparty.rs` | Gateway-encapsulated traffic (`}`). **Receive-only**; nested encapsulation is out of scope. |
| `src/aprs/capabilities.rs` | Station capability reports (`<`). |
| `src/aprs/monitor.rs` | TNC2 monitor lines (`SRC>DEST,PATH:information`), the text form APRS travels in off the air: as APRS-IS streams it, and as it appears inside a third-party frame. Addresses are raw slices rather than `Address`, because APRS-IS permits callsigns and `q` constructs that AX.25 does not; the information field stays bytes, never text. |
| `src/digipeat.rs` | WIDEn-N digipeater primitives: served aliases, the pure `relay_decision` core, `DupeRing` duplicate suppression. Attaches at the header only; see "The relay seam". |
| `src/kiss.rs` | KISS TNC serial framing: escaping encoder, streaming deframer, command bytes. |
| `src/rs.rs` | Reed-Solomon RS(255,k) codec over GF(256). Shared by `fx25` and `il2p`. |
| `src/fx25.rs` | FX.25 FEC layer: correlation tags + RS codeblocks around AX.25 frames; tag-hunting receiver. |
| `src/il2p.rs` | IL2P frame codec: sync word, 13-byte header codec, x⁹ + x⁴ + 1 scrambler, per-block RS FEC, bit-level `Il2pReceiver`. Replaces HDLC framing wholesale (not AX.25-compatible on the air). |
| `src/ring.rs` | `SampleRing<N>`: fixed-capacity SPSC-shaped sample FIFO for ISR/main-loop intake. Drops newest on overrun and counts it. |
| `src/wav.rs` | `wav`-gated WAV header validation and whole-file sync decode via `hound`. |
| `src/wspr.rs` | WSPR TX: type-1 message packing, K=32 r=1/2 convolutional code, interleaver, sync merge, 4-FSK audio; plus the no_std RX math (deinterleave, capped Fano decoder, unpack). |
| `src/wspr/rx.rs` | `std`-gated WSPR receive engine: mixer → two FIR decimation stages → FFT candidate search → sync correlation → soft demod → Fano. Buffers a whole ~114 s capture (≈ 14.9 MiB peak). |
| `src/ft8.rs` | FT8 TX: 77-bit payload packing (standard `i3=1` + free text), CRC-14, LDPC(174,91), Gray/Costas symbol map, GFSK 8-FSK audio; plus the no_std RX math (LLRs, min-sum decoder, CRC verify, unpack). |
| `src/ft8/rx.rs` | `std`-gated FT8 receive engine: same shape as the WSPR engine over a 12.64 s capture (≈ 2.1 MiB peak). |
| `src/m17.rs` | M17 packet mode, entirely no_std: base-40 addressing, LSF + packet frames, the packet superframe codec, transmitter and receiver. |
| `src/m17/fec.rs` | M17 channel coding as pure bit manipulation: K=5 convolutional FEC with P1/P3 puncturing, Viterbi, the QPP interleaver, the randomizer, Golay(24,12). |
| `src/m17/modem.rs` | M17 physical layer: 4-level symbol mapping and the RRC-shaped baseband modulator. |
| `src/tnc.rs` | The orchestrator's receive half: `TncReceiver` and the multi-chain diversity receiver (see below). |
| `src/tnc/config.rs` | `TncConfig` and the policy types both directions read: `SpaceGainSweep`, `InputBandPass`, `ChainVoting`, `TncError`. |
| `src/tnc/tx.rs` | `TncTransmitter` and its lazy sample iterators. |
| `src/asynk.rs` | `async`-gated tokio adapters: frame `Stream`s, the one-call KISS server, the many-feeds decoder. |
| `src/embassy.rs` | `embassy`-gated no_std adapters: the `SampleSource` seam, an async chunk-drain decode loop, and a periodic-TX ticker. |
| `src/bin/yodel/` | The `cli`-gated binary: `main.rs` holds the clap command tree and dispatch; `decode.rs`/`encode.rs`/`gen.rs`/`bench.rs`/`serve.rs`/`aprsis.rs`/`level.rs`/`wspr.rs`/`ft8.rs`/`m17.rs`/`ptt.rs` hold one subcommand each (`serve.rs` includes the transport-agnostic bridge core as its nested `serve` module); `json.rs` holds the JSON Lines projection of the library types; `shared.rs` holds the modem presets/overrides and WAV/PCM input plumbing. **`ptt.rs` is the only part of this crate that can put a signal on the air by itself**, so it is built the other way round from everything else here: the failure mode of every path through it, including a panic or a hung child, is to release the line. |

## The PHY seam

There are **two** mechanisms by which a physical layer plugs into the
receive chain, and they disagree:

1. **The advertised seam** is the `Discriminator` trait
   (`src/discriminator.rs`). It abstracts exactly one stage: PCM
   sample in, signed mark/space soft metric out. The tone-AFSK path
   uses it (`QuadratureCorrelator` is the default implementation),
   and `Demodulator<D>` composes any `Discriminator` with the PLL
   `Slicer` (`AfskDemodulator` is the `QuadratureCorrelator` alias).
2. **The in-use seam for G3RUH** is a set of cfg'd branches inside
   `tnc.rs`. The 9600-baud scrambled-baseband path does *not*
   implement `Discriminator`: `TncConfig` carries an
   `Option<BasebandModulator>` template, `TncReceiver` a `BasebandRx`
   branch, and the TX iterators grow `TxI16Inner::Baseband` /
   `TxF32Inner::Baseband` arms, all selected by
   `ModulationScheme::ScrambledBaseband`.

**Why they disagree.** Baseband G3RUH replaces the discriminator with
a metric of a different kind. Its front half (`BasebandFilter`: FIR
low-pass plus decision-feedback baseline removal, crate-private) emits
a centered baseband amplitude rather than a mark/space difference, and
the path inserts a scrambler stage that tone AFSK does not have. The
PLL `Slicer` is *not* replaced; `BasebandDemodulator` and
`TncReceiver`'s `BasebandRx` each drive one. The shape fits the trait;
the *plumbing* does not. Making the metric half public, giving
`TncReceiver` a generic front end per chain, and growing a matching
TX-side abstraction are all behavior-affecting refactors of tuned
code. Entering at the bit source boundary inside `tnc.rs` kept the
tone paths byte-identical; the `baseband: None` case is documented in
`tnc.rs` as leaving them untouched.

**What was weighed when G3RUH landed.** A *const-generic or parameter
tweak* was rejected: G3RUH shares no meaningful parameters with the
AFSK path, so there is nothing to parameterize. A *closed enum of
front ends* is workable, but forces every front end's state into one
type, makes every downstream user pay for all variants, and closes the
set against external ones. A *trait* was chosen in principle. What
shipped is the narrower version: the crate-internal integration
selects the front end per configuration at the bit-source boundary
(`TncTransmitter::frame_samples_*` / `TncReceiver::push_sample` branch
on `TncConfig::scheme()`), while the public `Discriminator` seam stays
open for external front ends.

**What unification would look like.** A wider `BitSource`-style PHY
trait: PCM sample in, `Option<Bit>` out, implemented by
(discriminator + slicer) for tone AFSK and by
(baseband demodulator + descrambler) for G3RUH, with a matching
sample-iterator trait on the TX side. `TncReceiver` would then hold
one generic front end per chain instead of the `Option`/enum
branches.

**Why it was NOT done now.** The multi-chain diversity receiver in
`tnc.rs` is tuned machinery: its per-chain gains, filter mixes, and
voting produce the pinned benchmark rows in
[`BENCHMARKS.md`](BENCHMARKS.md) (including the ≥ 74 synthetic-noise
row asserted by `tests/benchmark.rs`). Threading a new trait through
the chain bank risks perturbing exactly those numbers for zero
user-visible gain while there are only two schemes. The refactor
becomes worth its risk when a third PHY arrives; the candidates are
surveyed under "Scope: which modes fit these seams" and "Roadmap"
below. Until then the split stays **documented and deferred**.

**M17 is not the third PHY.** Its 4-level RRC-shaped baseband PAM
could have become a third `ModulationScheme` variant and did not: the
existing seam is binary-symbol and HDLC-shaped, M17 shares neither its
symbol alphabet nor its framing, and extending the enum would have
complicated shipped modes for zero reuse.
`M17Modulator`/`M17Receiver` ship as a standalone pair in the G3RUH
baseband *family* instead: same fixed-point FIR and feed/pull
structure, no shared enum. Like G3RUH it stops at the baseband
boundary; the ±0.8/±2.4 kHz 4FSK RF modulation lives in the radio,
outside an audio modem's scope.

## The bit-source boundary

`HdlcDeframer::push(bit)` / `Modulator::feed(bit)`, plus the NRZI
stage on either side, form a clean bit-level seam: anything that
produces or consumes a demodulated bit stream can slot in front of the
framing layer. That is where the G3RUH front end enters, and it is why
HDLC, FCS, AX.25, KISS and APRS are reused verbatim by it. Below the
seam nothing carried over, which is why G3RUH cost a whole slice:
pulse shaping, matched filtering, clock recovery at 9600 baud and DC
handling are a new front end, not a parameterization of the old one.

Above the seam the mod/demod path is baud- and tone-parametric, and no
Bell-202 residue is left in it. `BaudRate`, `TonePair` and
`SampleRate` are validated newtypes (`types.rs`), and every phase
increment, correlator window length and `Slicer` step size is
*derived* from them. `discriminator::MAX_WINDOW` is 240, sized for the
orthogonal 1.5-symbol window the 300-baud profiles need, wider than a
single symbol (see the tone-orthogonality section of
`src/discriminator.rs`). `BandPass::new` takes its corners from the
`TonePair`: 3/4 of the lowest tone, one baud above the highest.
`TncConfig::new` gives every non-Bell-202 configuration
`SpaceGainSweep::UNITY` rather than the emphasis-tuned bank, whose
sweep values compensate Bell-202-specific pre-emphasis. Presets such
as `bell_202()` are conveniences over the checked constructors, not
modes, so no path silently assumes 1200/2200 Hz.

**Where the scrambler sits (binding).** A scrambler is a
stateless-per-bit LFSR adapter shaped like
`NrziEncoder`/`NrziDecoder`, and it goes between NRZI and the PHY on
both sides. The G3RUH stage order was settled against the G3RUH design
description and confirmed by the differential leg:

```
TX: stuffed HDLC bits -> NRZI encode -> Scrambler -> baseband waveform synthesis
RX: samples -> low-pass/AGC/slicer/PLL -> raw bits -> Descrambler -> NRZI decode -> HDLC deframe
```

The scrambler operates on the NRZI-domain bit stream, descrambling on
raw sliced bits *before* NRZI decode. An earlier draft modeled it on
the other side of the NRZI stage, which is **not** interoperable and
is superseded.

## The frame-wrapper seam: FX.25 and IL2P

FX.25 wraps a complete, unmodified AX.25 frame, HDLC flags and
bit-stuffing included, in a Reed-Solomon codeblock behind a 64-bit
correlation tag, and is additive by construction: a legacy receiver
decodes the embedded frame and ignores the rest. That puts `wrap`
between `TncTransmitter::build_frame` and the NRZI/AFSK bit stages on
transmit, and makes `Fx25Receiver` a parallel tag-hunting path beside
`HdlcDeframer` on receive. The default TNC paths are untouched and
remain byte-identical, as the design note in `src/fx25.rs` sets out:
FX.25 is not a `TncConfig` setting, and callers opt in by composing
the stages themselves.

The RS(255,k) codec over GF(256) is shared machinery (`src/rs.rs`,
with an `fcr` seam where `fcr = 0` is byte-identical to FX.25). It
shipped as its own slice ahead of the framing layer, because a
correct, tested, allocation-free `no_std` RS decoder (syndromes,
Berlekamp–Massey, Chien search, Forney) is a real project in its own
right.

IL2P sits at the same seam and reuses that codec, but replaces more of
the stack. `Il2pReceiver` is a parallel bit consumer beside
`HdlcDeframer`, on the `Fx25Receiver` pattern; no trait seam was
needed, since `Chain` hardcodes `HdlcDeframer`, and IL2P frames never
look like HDLC. IL2P does **not** use NRZI: the specification states
that differential encoding is not used, so `tx_bits` feeds the
modulator directly. It needs no new `ModemProfile` either, being a
framing layer over the existing tone-AFSK presets rather than a new
waveform.

**Remaining levers on this seam**, none of them blockers:
`serve`/`bench`/`encode` IL2P wiring, and IL2P over the g3ruh
baseband, both straightforward over the same seams. Fusing the FX.25
tag hunter into the multi-chain `TncReceiver` is **superseded as the
first move**: smoothing the bare discriminator's decision statistic
took the noisy row 60 → 92 (ref 82) on its own, and diversity then
measured to add *nothing* on a flat channel, where the union over 11
chains equals the best single chain. Fusion remains worth doing only
for worst-case robustness on tilted channels (see
[`BENCHMARKS.md`](BENCHMARKS.md)).

## The relay seam: a digipeater only touches the header

`digipeat` attaches beside the frame layer rather than inside it, and
the property behind that placement is worth stating before someone
wires it in differently.

Split a received frame into its header and its information field,
`w = (h, i)`. Two operations get called "forwarding" and they are not
the same map:

```text
canonicalise   k(h, i) = (h, build(parse(i)))    reads the payload
digipeat       D(h, i) = (b_h(t(p_h(h))), i)     does not
```

A digipeater's authority is the AX.25 header alone: find the first
unused hop, decide whether it is addressed here, set the H bit,
decrement `WIDEn-N`, re-transmit. The information field is carried by
**identity**.

Three consequences follow, and all three matter:

* **Byte fidelity on a relayed payload is free.** Not aspirational, not
  the product of a preservation mechanism. It holds because nothing on
  the path reads those bytes.
* **It is total.** `k` is *partial*, and it is undefined on exactly the
  frames a relay is obliged to forward: the ones no parser here accepts.
  A forwarding path built out of parse-then-build is therefore not a
  slower version of the right thing, it is a filter.
* **The argument for "preserve the received bytes in `build`" collapses.**
  That argument is always some form of "a digipeater that parses and
  re-transmits puts bytes on the air nobody sent". True, and it is a
  reason not to apply `k` on a relay path, not a reason to make `k` the
  identity. See `../CONTRIBUTING.md`, "Design invariants that are
  load-bearing", for why making `k` the identity would cost the crate
  its main diagnostic.

The seam is enforced by the signatures rather than by convention.
`relay_decision` takes the path and nothing else, so the payload cannot
influence the decision, and `UiFrame::with_hops` borrows the information
field rather than rebuilding it, so the decision cannot influence the
payload. `tests/digipeat_laws.rs` sweeps the three laws that follow:

| law | statement | what it bounds |
|---|---|---|
| payload transparency | `info(D(w)) = info(w)` | a relay is not a filter |
| termination | every relay spends exactly one hop of the budget | a flood is finite |
| local loop freedom | a station offered its own output declines | two stations cannot trade a frame forever |

Termination deserves the emphasis. Define the residual budget of a path
as the sum over unrepeated hops of `N` for `WIDEn-N` and 1 for anything
else. Every relay decrements it by exactly one, so the flood depth is
bounded by what the originating station requested, by induction on a
well-founded order. A branch that consumed no budget would leave
termination resting on the duplicate-suppression ring, which is a time
window and not a bound, and the resulting failure would be network-wide
rather than local: no single station's tests would show it.

Anything that gives the relay path a reason to read the payload is a
change to this seam, and the argument in
[`APRS_CONFORMANCE.md`](APRS_CONFORMANCE.md) section 4 has to be
answered first.

## Feature flags

| feature | gates | dependency chain | rationale |
|---|---|---|---|
| `mod` (default) | `modulator.rs` | — | TX-only devices (beacons) need no demodulator. |
| `demod` (default) | `demodulator.rs`, `discriminator.rs`, `slicer.rs` | — | RX-only devices need no modulator. |
| `alloc` | heap conveniences (e.g. `TncTransmitter::transmit_to_vec_i16`) | — | The core is allocation-free; `Vec` adapters are opt-in. |
| `std` | std conveniences (links `std`; drops the crate's `no_std` attribute) | `alloc` | Pulls in **no** dependency, since WAV moved out to `wav`. |
| `wav` | WAV I/O via the `hound` codec: the library `wav` module (header validation + whole-file sync decode) and the CLI's WAV edges | `std`, `dep:hound` | Split from `std` so a plain-`std` consumer does not pay for a WAV codec. |
| `nrzi` | `nrzi.rs` | — | The line-coding layer between raw bits and HDLC; usable alone. |
| `ax25` | `ax25/` | `nrzi` | Frames imply line coding on the air; addresses/FCS/HDLC come as one layer. |
| `aprs` | `aprs/` | `ax25` | APRS payloads ride UI frames by definition. |
| `micE` | `aprs/mic_e.rs` | `aprs` | Sizeable table-driven codec; opt-in on top of `aprs`. |
| `digipeat` | `digipeat.rs` | `ax25` | Digipeater policy is a *consumer* of the frame layer, not part of it. It is not folded into `tnc`, which stays a pure modem pipeline. |
| `kiss` | `kiss.rs` | — (standalone) | Byte-level serial framing; independent of the radio stack. |
| `g3ruh` | `scrambler.rs`; with `mod`/`demod` also `baseband.rs` | — (standalone) | The scrambler is a pure bit stage; the baseband PHY appears only with the DSP features. |
| `fx25` | `rs.rs`, `fx25.rs` | — (standalone; the tag-hunting receiver additionally needs `ax25`) | FEC is a sidecar around AX.25 frames, not a layer of them. |
| `il2p` | `il2p.rs` (and `rs.rs`) | `ax25` | The header codec translates AX.25 addresses and frames, so it needs that layer; it *replaces* HDLC rather than wrapping it. |
| `wspr` | `wspr.rs`; the buffered receive engine (`wspr/rx.rs`) additionally needs `std` | — (standalone) | A different mode family, not part of the streaming TNC chain. TX and the decode math are no_std; only the whole-capture engine needs a heap. |
| `ft8` | `ft8.rs`; the buffered receive engine (`ft8/rx.rs`) additionally needs `std` | — (standalone) | Sibling of `wspr`, same TX/no_std-math vs std-engine split. |
| `m17` | `m17.rs` | — (standalone) | A baseband PAM modem with its own framing; shares nothing with the AFSK chain. Entirely no_std and alloc-free, so no std-gated half exists. |
| `tnc` | `tnc.rs` | `aprs`, `mod`, `demod` | The orchestrator needs the full column. |
| `cli` | `bin/yodel/` | `wav`, `tnc`, `micE`, `kiss`, `fx25`, `il2p`, `wspr`, `ft8`, `m17`, `ptt`, `dep:clap` | Aggregate for the binary. `kiss` is exercised by `yodel serve` (the KISS TNC server, described under "The serve shape and the async verdict" below). Note `g3ruh` is **not** in this list, so a `--features cli` build has no `--preset g3ruh`; `--all-features` does. |
| `capture` | `examples/live_capture.rs` only | `std`, `dep:cpal` | Sound-card input for ONE example; off by default, enabled by nothing else, never a library-consumer dependency (`cargo tree -e normal` is unchanged). `--all-features` compiles cpal, so tests must never OPEN a device. The example's plumbing is pure and proven with a fake source. |
| `async` | `asynk/` (tokio adapters: frame `Stream`s, one-call KISS server, many-feeds decoder) | `std`, `tnc`, `kiss`, `dep:tokio`, `dep:futures-core`, `dep:tokio-stream` | Operator override of the async NO verdict (see below). Off by default, enabled by nothing else; default and embedded builds never compile it or its dependencies. |
| `embassy` | `embassy.rs` (async chunk-drain decode loop over `SampleRing` + `TncReceiver`, embassy-time TX ticker) | `tnc`, `dep:embassy-time` | Operator override of the pattern-doc-only verdict (see "The embassy verdict" below). no_std-first: the feature does NOT imply `std`. Off by default, enabled by nothing else; default and embedded-matrix builds never compile it or its dependency. |

Everything except `std`, `wav`, `cli`, `capture` and `async` is
`no_std`, and everything except those and `alloc` is allocation-free.
`scripts/check-embedded.sh` cross-builds every no_std feature set for
two bare-metal targets, plus the detached `examples/esp32-riscv`
sub-crate (`../CONTRIBUTING.md`, "Embedded matrix", for how to run it). (`embassy` is no_std as well, but is
not in that matrix: `embassy-time` needs a platform time driver at
link time.) The two weak-signal receive engines are the one place
where a feature is *partly* no_std: `wspr`/`ft8` TX and decode math
cross-build, while `WsprDecoder`/`Ft8Decoder` require `std`.

## The input edge: live audio

The core is device-agnostic: it consumes `i16` sample slices and knows
nothing about files, pipes, or sound cards. Live audio input therefore
lands entirely at the edges, as a **combination** of three thin
adapters rather than one device abstraction in the library:

1. **CLI input abstraction** (`bin/yodel/decode.rs`): `yodel decode`
   accepts a WAV path (unchanged, byte-stable output), or `-` for
   stdin. Stdin is sniffed for a `RIFF` header. WAV streams decode
   with their own header; anything else is raw PCM read until EOF
   (live-pipe friendly) under `--sample-rate` (required; raw PCM has
   no rate) and `--format` (an enum with `s16le` as its only member
   today, so new encodings are a variant away). All three inputs
   funnel into one `decode_samples` core over
   `Iterator<Item = Result<i16, String>>`, so the frame/stats output
   format is provably identical.
2. **Pipe recipes** (README §"Live decode from your sound card"):
   any capture tool (ALSA recorder, sox, ffmpeg) becomes a live
   front end via `... | yodel decode --sample-rate 48000 -`. This
   is the zero-new-dependency path and the recommended default.
3. **Optional cpal example** (`examples/live_capture.rs`, `capture`
   feature): for users who want the binary-free "just open the mic"
   experience. cpal is optional and enabled by nothing else; the
   device-facing code is confined to the example's `main`, while the
   rate planning / downmix / conversion / chunk-feed plumbing is a
   pure module proven with a fake (synthesized-audio) source in
   `tests/cli.rs`, so CI never opens a device.

**Why the combination and not a `--device` flag on the CLI?** A
device flag would make cpal (a heavy, platform-specific audio stack)
a dependency of the `cli` feature, reproducing the
accidental-dependency shape the `std -> hound` fix removed. Pipes keep
the CLI's dependency surface unchanged, work over ssh/containers, and
compose with every capture tool's own resampling; the cpal example
exists for discoverability and as tested reference plumbing, priced at
zero for library and CLI consumers. Proper fractional resampling is
out of scope: the example decimates exact integer ratios
(96k/192k → 48k) and otherwise refuses with guidance, because decoding
at a wrong clock silently yields nothing.

## The serve shape and the async verdict

**The serve shape.** `yodel serve` is the third input/output edge:
instead of printing decoded frames or writing a WAV once, it holds
both edges open and bridges them to KISS byte streams. The core lives
in the binary (`src/bin/yodel/serve.rs`, `mod serve`) as transport
glue over the EXISTING layers: KISS framing from `src/kiss.rs`
(encoder iterator + streaming deframer, nothing reimplemented), the
modem from `src/tnc.rs`, FX.25 from `src/fx25.rs`. Two transports:
`--tcp` (a listener admitting up to 8 clients, with received frames
broadcast to all and transmit accepted from any) and `--stdio` (one
KISS stream on stdin/stdout, the classic direct-attach shape).
Threads and bounded channels only:

```text
audio in ─→ decode thread ─→ bounded chan ─→ broadcast thread ─→ every client
client N ─→ reader thread ─→ bounded chan ─→ TX writer ─→ audio out
```

The module stays clap-free and free of process-global I/O so that it
can be tested directly: `tests/serve.rs` `#[path]`-includes it and
proves both directions on a loopback listener bound to `127.0.0.1:0`
(OS-assigned port, no fixed ports in CI), plus two-client broadcast
and timeout-guarded clean shutdown. Audio edges reuse the
`decode`/`encode` plumbing, WAV files or raw-PCM pipes, so live audio
arrives exactly the way the live-decode section above describes.

**ASYNC VERDICT: NO** *(superseded; see below)*. The KISS server
forced a question: should yodel grow an async feature (tokio
adapters, an async `Stream` of frames)? It was evaluated and answered
**no**, for four reasons, recorded here so the decision does not get
re-litigated by accident:

1. **The lending RxFrame API.** `RxFrame` borrows the receiver's
   buffer until the next push. An `async Stream` adapter would force
   an owned-frame copy layer plus `spawn_blocking`/mpsc glue: the
   very ~20–50 lines of generic tokio idiom every async user already
   knows, frozen into our API.
2. **Zero-dep identity.** Shipping the adapter costs tokio + futures
   in the dependency tree of a crate whose documented identity is
   zero-dependency `no_std`.
3. **The worker-pool idiom suffices.** The workload behind the
   question (N concurrent feeds into a database) is fully served by
   sync DSP on worker threads + bounded channels: at ~88 ns/sample
   (the MEASURED 11-chain row in [`BENCHMARKS.md`](BENCHMARKS.md)) one
   core decodes hundreds of real-time feeds. The pattern ships as
   `examples/decode_many_threads.rs` (std threads, backpressure
   proven in `tests/app_examples.rs`), and its header documents the
   tokio transposition (async edges + `spawn_blocking` + bounded
   `tokio::sync::mpsc`), as does the README's "Using yodel from
   async (tokio)" subsection.
4. **Ecosystem norm.** Sans-io cores keep async adapters out of the
   core crate (quinn-proto, rustls, embedded-hal); a separate adapter
   crate remains possible if demand ever materializes.

The KISS server is therefore `std::net` + `std::thread`: a handful of
clients, CI-testable, no runtime. That is still true of `yodel serve`
today; what changed is that an optional adapter layer now sits beside
it.

**SUPERSEDED (operator decision).** yodel now ships an optional
`async` cargo feature: the tokio adapter layer in `src/asynk.rs` (a
`Stream` of `OwnedFrame`s from a WAV or any `AsyncRead`, a one-call
KISS-over-TCP server, and a concurrent many-feeds decoder). This is a
priority call rather than a reversal of the reasoning above. Points
1–4 argued about the *core*, and they still hold for the core: the
sync, allocation-free, dependency-free `no_std` core is untouched and
remains the source of truth, and embedded users pay nothing for async
(the feature is off by default, enabled by no other feature, the
embedded matrix never sees it, and default builds pull no
tokio/futures). What changed is the weight given to beginner
ergonomics on capable machines: the "~20 lines of tokio idiom every
async user already knows" turned out to be exactly the 20 lines a
newcomer gets wrong (unbounded channels, DSP on the reactor, dropped
frames), so yodel now ships them once, correctly, behind a flag. The
result is one crate that covers the whole stack from a bare-metal
microcontroller to a multicore server. The adapter stays thin:
`asynk` duplicates no DSP, reuses the library `kiss` and `wav`
modules, keeps every channel bounded (backpressure, never loss), and
runs the sync receiver on `spawn_blocking`. The std-threads `yodel
serve` and `examples/decode_many_threads.rs` remain for people who
want no runtime.

## The embassy verdict

(The README's "Sharing the MCU" section is the beginner-facing
walkthrough of all three concurrency paths: superloop, embassy and
RTIC. This section and the next record the design decisions behind
it.)

The planning verdict on embedded-executor integration was **pattern
docs only, no cargo feature**: the core composes with any executor via
plain function calls (`SampleRing` + bounded `TncReceiver::push_i16`
chunks), so an embassy adapter looked like dependencies bought for a
one-page wrapper. That reasoning was sound for the core. Mirroring the
async supersede above, the **operator superseded it for platform
reach**: embassy users get the correct chunk-drain/yield/ticker glue
once, shipped and tested, instead of each rediscovering it.

The result is the opt-in `embassy` feature and `src/embassy.rs`, held
to the same discipline as `asynk`:

- **The sync core stays the source of truth.** The adapter contains no
  decode logic: `run_decoder` only moves samples from an async
  `SampleSource` into bounded `push_i16` calls with a cooperative yield
  between chunks, and `TxTicker` wraps `embassy_time::Ticker` for
  beacon cadence. Decode-equivalence with the plain sync loop is
  proven in `tests/embassy.rs`.
- **One dependency, justified.** The library pulls only `embassy-time`
  (no_std, alloc-free), used solely by `TxTicker`; the decode loop is
  plain `async` Rust and needs no embassy crate at all. No executor is
  a library dependency: host tests and
  `examples/balloon_tracker_embassy.rs` use dev-dependency
  `embassy-futures`/`embassy-time/std` only.
- **no_std-first.** The feature does NOT imply `std`, unlike `async`.
  The target is bare-metal use, with the platform HAL supplying the
  time driver at link time.
- **Zero cost when off.** Off by default, enabled by no other feature;
  the default `cargo tree` and the embedded matrix are unchanged.

## The rtic verdict

The operator asked for either an opt-in `rtic` feature (embassy
discipline: off by default, deps confined) **or** a justified
example-only route. The evaluation landed on **example-only, no cargo
feature, no new dependency**. Unlike the embassy case, this rests on a
structural fact about RTIC rather than on a re-run of the
pattern-docs-only argument:

- **There is no library code an adapter could contain.** RTIC apps are
  macro-generated (`#[rtic::app]`) around a device PAC and its
  hardware interrupt vectors; resource locking, task priorities, and
  scheduling are all provided *by RTIC itself*, not by traits a
  library could implement. The embassy adapter had glue to ship (an
  async chunk-drain loop with a yield point, a ticker over
  `embassy-time`); the RTIC equivalents are RTIC's own `#[shared]`
  lock and monotonic, so yodel's role reduces to *types placed in
  resources*: a `SampleRing` in a `#[shared]` resource pushed by the
  DMA ISR, a `TncReceiver` in the decode task's `#[local]`.
- **A `rtic` feature would be an empty namespace.** With nothing to
  put behind it, the feature would exist only to pull an `rtic`
  dependency the library never calls, and it would break the host
  gates as well: `rtic` cannot compile without a target-specific
  backend selection, so the feature could not be part of
  `--all-features`.
- **The worked example carries the weight instead.**
  `examples/balloon_tracker_rtic.rs` puts the full RTIC 2 app skeleton
  in its header comment: tasks, priorities 3/2/1/idle,
  `#[shared]`/`#[local]` resources. The task bodies *themselves* then
  compile, run and self-assert under a mock priority-ordered
  dispatcher on the host, with a worst-case-latency table; the ISR
  ring push, the bounded chunk drain through `push_i16` and the beacon
  synthesis are unmocked. What is mocked (the scheduler, the PAC) and
  why is stated in the file header. The example is compile-checked by
  the normal
  `tnc`-feature lanes (`cargo test --all-features`,
  `cargo clippy --all-targets`) and self-asserts when run
  (`cargo run --example balloon_tracker_rtic --features tnc`); no test
  binary drives it.

If a lock-free handoff type ever becomes worth shipping for RTIC users
(e.g. an SPSC ring with split ownership, impossible today under
`forbid(unsafe_code)` without a dependency), that would reopen the
feature question. Until then the route stays example-only.

## Shape notes for contributors

**Why `tnc.rs` holds more than pipeline glue.** The module named for
"pipeline glue" contains a **multi-chain diversity receiver**: a gain-sweep bank
(`SpaceGainSweep`, up to `MAX_SWEEP` = 9 geometric space-tone gains
covering ±dB channel tilt) running parallel decision chains over three
input variants (raw / band-passed / pre-emphasized), each chain with its
own slicer/DPLL, NRZI decoder and HDLC deframer, plus cross-chain
bit-history **voting** and bit-flip salvage, with content
de-duplication so each transmission is emitted once. None of that is
"glue", but all of it is receiver behavior, so it lives with the
receiver.

The two parts that are *not* receiver behavior have been lifted out.
`tnc/config.rs` holds `TncConfig` and the policy types, which both
directions read and which change for reasons of their own, and
`tnc/tx.rs` holds the transmitter. Both are private modules re-exported
from `tnc`, so every public path is unchanged; the receiver keeps the
file it needs.

The *architecture* is prior art, not ours: parallel demodulators over
differently emphasized audio with duplicate removal is the design in
Sivan Toledo (4X6IZ), "A High-Performance Sound-Card AX.25 Modem",
*QEX* July/August 2012, which also originates the
replay-the-test-CD-and-count-frames method
[`BENCHMARKS.md`](BENCHMARKS.md) uses. See the "Prior art" section on
`SpaceGainSweep` for the citation and for which parts are ours. The
*tuning* is ours; it accreted while chasing (and beating) the
reference corpus numbers.

**Where things live** (three files, one module):

1. `tnc/config.rs`: `SpaceGainSweep`, `TncError`, `InputBandPass`,
   `ChainVoting`, `TncConfig` (incl. the cfg'd
   `Option<BasebandModulator>` PHY branch).
2. `tnc/tx.rs`: `TxI16Samples`/`TxF32Samples` iterator wrappers (with
   baseband enum arms), `TncTransmitter`, `alloc`-gated `Vec`
   conveniences.
3. `tnc.rs`: `RxFrame`, `OwnedFrame`, `TncStats`, then the diversity
   machinery: `ChainInput`, `Chain`, `BitHistory` (voting ring),
   `BandPass`, `PreEmphasis`, `SeenFrame` (dedup), `BasebandRx`, and
   finally `TncReceiver` itself.

**Remaining split** (deferred): the receive half is still one file.
Moving it to `tnc/rx.rs` (with the chain bank as `tnc/rx/chains.rs`)
touches every line of the tuned receiver and puts the pinned benchmark
and SNR rows at risk, so it needs its own slice with the full
ignored-test benchmark suite run before and after. The config and
transmit halves came out first precisely because they could move
without touching a decision chain.

## Performance note

The `i16` decode/modulate paths are integer-only per sample at runtime
(float appears only in `const fn` table generation, one-time FIR/RRC
tap design at construction, and the separate `*_f32` twins), which is
what makes the no-FPU riscv32 targets viable. Their measured host
throughput and the extrapolated ESP32-class cycle budgets live in the
README section **"Will it run on my chip? (ESP32 RISC-V
feasibility)"**, backed by the re-runnable
`examples/throughput.rs` benchmark. The multi-chain diversity receiver
described above is the dominant per-sample cost of the Bell-202
default configuration (11 chains), so budget-constrained targets can
trade sensitivity for cycles via `SpaceGainSweep::UNITY`.

Hot-path strength reduction: the shared `ToneCorrelator::push` no
longer divides or takes a modulo per sample. The window normalization
runs on a hoisted round-up fixed-point reciprocal multiply (exactness
proof on `ToneCorrelator::scale`, valid up to `RECIP_MAX_LEN` = 181
samples, so Bell 202 never divides; only the stretched 300-baud
orthogonal windows fall back to a real division), and the ring index
wraps with a conditional reset. Both are proven bit-exact against the
original division-based implementation by a permanent
sample-by-sample equivalence test over LCG-random streams
(`push_matches_division_reference_on_random_streams`), and every
pinned corpus/differential benchmark row was re-run unchanged.
MEASURED host effect: 1200-baud 11-chain decode ~116 →
~110 ns/sample (best-of-5, same machine and run). A faster
software-estimate `isqrt` for the envelope path was prototyped and
MEASURED SLOWER than `u64::isqrt` on the host, so it was reverted. The
envelope amplitudes feed per-chain gain-scaled comparisons through two
smoothing stages, so a squared-domain comparison rewrite is not
available bit-exactly.

G3RUH FIR modulo removal: `BasebandFilter::push` no longer computes a
per-tap `(pos + k) % taps_len` ring index. The convolution runs as a
two-slice linear sum (`history[pos..len]` against `taps[..len-pos]`,
then `history[..pos]` against `taps[len-pos..]`) and the ring advance
is a conditional reset, both bit-exact by construction (i64
accumulation makes the split-sum order immaterial; the headroom bound
is documented at the site). The PLL slicer was audited and already has
no per-sample division or modulo (wrapping adds and shifts).
Symmetric-coefficient folding was considered and skipped: the
unity-DC rounding residue is pushed into the center tap and folding a
split ring costs more index bookkeeping than the ≤15 multiplies it
saves. Proven bit-exact by a permanent equivalence test
(`tests/equivalence.rs`) that freezes the original modulo
implementation and asserts identical demodulator output
sample-by-sample over 200k-sample LCG-random i16 streams at 4
rates × 3 seeds. MEASURED host effect: 9600 G3RUH decode 13.1 → ~10.8
ns/sample (≈17% faster); all pinned corpus rows re-measured unchanged.

## Scope: which modes fit these seams

This section asks which neighboring modes fit the seams above, which
need new ones, and which do not belong here at all. The **binding
exclusion list is policy and lives in [`../CONTRIBUTING.md`](../CONTRIBUTING.md)
§"Scope: what belongs in this crate"**; this section is the
architecture-fit reasoning behind it. The verdict vocabulary was
**IMPLEMENT**, **SPEC-ONLY** (write the rigorous spec now, build later
or never) and **OUT OF SCOPE**.

| Candidate | Fit to the seams | Verdict, and where it landed |
| --- | --- | --- |
| Arbitrary baud/tone + named presets | Excellent, ~90% there; the config layer only | **SHIPPED**: `ModemProfile`/`TonePair` in `src/types.rs`, `TncConfig::from_profile`, CLI `--preset` |
| 300-baud HF APRS (1600/1800 Hz) | Excellent; falls out of the row above as a config preset | **SHIPPED**: `ModemProfile::HF_APRS_300`, CLI `--preset hf300` |
| Bell 103 (300 bd, 1270/1070 or 2225/2025 Hz) | Excellent; another config preset | **SHIPPED**: `ModemProfile::BELL_103`/`_ORIGINATE`/`_ANSWER` |
| 9600-baud G3RUH | Framing layers reusable; the baseband front end is new DSP at the bit-source boundary, plus a scrambler stage | **SHIPPED** (was SPEC-ONLY): `src/scrambler.rs`, `src/baseband.rs`, `ModemProfile::G3RUH_9600` |
| FX.25 FEC wrapper | Clean additive frame-wrapper seam; the RS decoder is a real project on its own | **SHIPPED** (was SPEC-ONLY): `src/rs.rs`, `src/fx25.rs`, CLI `--fx25` |
| IL2P | Reuses the RS(255,k) GF(256) codec but **not** NRZI, which the specification forbids; a parallel bit consumer beside `HdlcDeframer`, on the `Fx25Receiver` pattern | **SHIPPED**: `src/il2p.rs`, CLI `--il2p` |
| FT8 / FT4 / FST4 / WSPR / JT65 | First judged "a different universe"; re-examined, they need a weak-signal block-decode engine module, NOT a refactor of the streaming chain | OUT OF SCOPE, **superseded**: WSPR (`src/wspr.rs`, `src/wspr/rx.rs`) and FT8 (`src/ft8.rs`, `src/ft8/rx.rs`) shipped on that engine; FST4 remains a candidate for it |
| DMR / C4FM / D-STAR / M17 | 4FSK/GMSK + vocoders/TDMA | OUT OF SCOPE for the vocoder modes, permanently; **superseded** for M17's data framing, which needs no vocoder: `src/m17.rs` |
| JS8 | Pure layering on the FT8 engine; variants change the symbol period, so it sequences last | **PLAN OF RECORD**, no implementation (see "Roadmap") |
| VARA | Proprietary/closed protocol | OUT OF SCOPE |
| LoRa APRS | Different PHY entirely (CSS) | OUT OF SCOPE |
| PSK31 | Phase keying, not FSK | OUT OF SCOPE |

README documents every shipped row for users, under "Other modes /
presets", "Command-line tool" (`--fx25`), "IL2P", "WSPR", "FT8" and
"M17 (data)", so the protocols are not re-explained here. One caveat
it does not carry: M17's P1/P3 puncture patterns, randomizer bytes,
QPP coefficients and Golay polynomial are transcribed from the
published spec tables and flagged as such on each const, and every
internal round trip is pattern-agnostic, so a transcription fix is a
one-line change; but **over-the-air interop against reference M17 gear
has not been exercised**, so confirm against reference vectors before
fielding.

### The bar every new mode must clear

| Deliverable | Meaning |
| --- | --- |
| Preset | Named config preset (the `ModemProfile`/`DevicePreset` pattern) |
| Example | A runnable `examples/` program |
| README walkthrough | Beginner-facing narrative section |
| CLI integration | Subcommand/flag wiring in the `yodel` binary |
| Feasibility table | RAM/CPU numbers, MEASURED/ESTIMATED labelled |

A framing layer over an existing waveform is excused the preset row,
as IL2P and FX.25 both are.

## Modes considered and declined

The exclusions above are hard noes, not a "not yet" list, and the
reasons sit in the fit column. Two verdicts were overturned, and both
are worth reading before proposing the next one.

The **weak-signal family** was judged "separate-crate territory by any
measure, never a bolt-on to an AFSK TNC" on the grounds of heavy FEC,
rigid time-slot synchronisation and tight frequency estimation. All of
that was true and the conclusion was still wrong: WSPR and FT8 both
shipped, with the decode math `no_std` and only the capture engine
`std`-gated. The split is what made it work, since the expensive part
is buffering, not arithmetic. Capture plus FFT candidate search need
~85–330 KB buffers, std/alloc-gated; that estimate held for the
*surviving* baseband, ≈ 342 KB for WSPR and ≈ 98 KB for FT8, but
MEASURED **peak** heap is ≈ 14.9 MiB and ≈ 2.13 MiB. The decoders are
`no_std`-feasible by contrast: LDPC(174,91) min-sum takes ~3 KB RAM
and WSPR K=32 Fano ~2 KB, with a node-visit cap mandatory, and N-tone
CPFSK TX is nearly free because the modulator's `u32` phase
accumulator never resets. The original survey did not weigh that
split.

**M17** was grouped with DMR, C4FM and D-STAR under "4FSK/GMSK +
vocoders/TDMA", which is right about the family and wrong about M17
data. Packet mode needs no vocoder at all, and it shipped. The
AMBE-based modes stay out permanently, and M17 voice stays out until
the Codec2 dependency question below is settled. Both overturned
verdicts predate the open-standards mandate now recorded in
`CONTRIBUTING.md`; everything else in the exclusion table stands.

## In scope, not yet taken

Not exclusions: these would fit, they have been thought through, and
they are not in the library today. The reasoning is recorded so that
"we did not do it" stays distinguishable from "we did not think of
it".

**PHY unification**, the `BitSource`-style trait sketched under "The
PHY seam", deferred until a third PHY makes the benchmark risk worth
taking. **Frame-wrapper wiring**: the IL2P levers and the FX.25
tag-hunter fusion listed under "The frame-wrapper seam: FX.25 and
IL2P". **Continuous symbol-clock tracking for M17**, whose receiver
re-acquires timing per 40 ms frame; a Gardner-style continuous tracker
is the obvious next lever for real soundcard clock skew.

### `yodel::json`: a serialization projection of the APRS/AX.25 types

**Status: shipped in the binary, not promoted to the library.**
`yodel decode --output-format jsonl` emits one JSON object per
decoded frame (schema in `README.md`), and the writer lives in
`src/bin/yodel/json.rs`, not in `src/`.

*Why it is not in the library.* The core is `#![no_std]`,
allocation-free and **zero-dependency**, the combination the crate
advertises as its headline property. Two ways to put JSON in it, both
currently rejected:

- **A `serde` feature.** `serde` itself is `no_std`-capable, but
  deriving `Serialize` on `Position`, `MicE`, `WeatherReport` and
  friends would freeze the *field layout* of those types into a public
  wire format. They are `#[non_exhaustive]` so that they can grow; a
  derived serializer would turn every added field into a silent schema
  change for every consumer, and every *renamed* one into a breaking
  change nobody noticed. The projection should be hand-written and
  reviewed, and that is how it is written today.
- **A hand-rolled `json` feature.** Plausible, and the current module
  is already shaped for it (see below). But it would need `alloc` (or
  a `core::fmt::Write` sink), and it would commit the library to a
  *stability promise about a text format*, which is a much stronger
  promise than "these accessors exist". Nobody has asked for it from a
  library entry point yet; every user so far wants the CLI.

*What the shape already buys.* `src/bin/yodel/json.rs` is written as
if it were already `yodel::json`: the writer (`Object`, `Array`)
appends to a caller-owned `&mut String` with closing braces written by
`Drop`, every projection is a free function of one library type
(`push_position_body`, `push_weather_fields`, `push_third_party`, …)
with no trait and no derive, and the byte-versus-string problem is
solved once in `Object::field_bytes` by the `_hex` sibling rule
(`info` always, `info_hex` only when the bytes are not valid UTF-8).
That is the signature set a `yodel::json` module would export, so
promoting it is a file move and a feature gate, not a rewrite.

*What promoting it would cost.* A `json = ["alloc"]` feature; deciding
whether the sink is `String`, `core::fmt::Write` or a `&mut [u8]`
cursor (the last is the only one an allocation-free target could use,
and it changes every signature to return `Result<usize, _>`); a
stability policy for the schema version key; and a differential test
proving the library and CLI renderings agree. Perhaps a slice's work,
and no current consumer needs it. **Revisit when someone wants JSON
from an embedded target or from a library-only host application.**

## Roadmap

Two items are planned and unbuilt.

### Codec2 voice for M17 stream mode

M17 stream mode carries 3200 bit/s Codec2 voice. Implementing it needs
a Codec2 vocoder, which this crate will not hand-roll (a vocoder is a
DSP project of this crate's own size) and must not take as a
dependency without operator sign-off:

- **Dependency candidate:** a pure-Rust Codec2 implementation (e.g.
  the `codec2` crate, a port of the reference library). **Licensing
  note: the reference Codec2 library and its Rust ports are
  LGPL-2.1.** That is acceptable as an *optional* dynamic-boundary dep
  for many consumers but a real policy decision for a MIT/Apache-2.0
  crate; static linking obligations must be reviewed by the operator.
  No AMBE anything, ever, per the exclusion table in
  [`../CONTRIBUTING.md`](../CONTRIBUTING.md).
- **Feature shape:** `m17-voice = ["m17", "dep:codec2"]`, off by
  default, enabled by nothing, and `std`-tolerated if the port demands
  it. Stream-mode framing (LICH chunking via the already-shipped
  Golay(24,12), stream sync 0xFF5D, 16-bit frame counter) lands with
  it.
- **Estimated footprint:** Codec2 3200 encode+decode ≈ 20–30 k mul/acc
  per 20 ms frame (well under one modern-MCU-core MIPS at 4800 sym/s
  duty), ≈ 4–8 KB state RAM per direction plus the existing modem
  buffers; comfortably real-time on a Cortex-M4F class part, trivial
  on a host.
- **Status: PENDING OPERATOR APPROVAL.** Until granted, voice stays
  out; M17 packet data is complete without it.

This is why `golay24_encode`/`golay24_decode` ship as a public
building block with no in-crate consumer. Their consumer is stream
mode's LICH, and stream framing waits on the decision above. Shipping
the codec now means the voice slice starts from a proven part, where
wiring LICH framing without a vocoder would be dead code.

### JS8

JS8 is a keyboard-to-keyboard conversational protocol derived from
FT8, sequenced last in the open-standards arc because it is purely a
layer over the FT8 engine. This is the plan of record, ahead of any
implementation: **no JS8 code exists in the tree**.

**Waveform relationship.** JS8's *normal* mode reuses the FT8 waveform
geometry (8-tone GFSK, the same 0.16 s symbol period and 6.25 Hz tone
spacing, 79 symbols with three 7-symbol Costas sync blocks) but
changes the **payload semantics and the Costas arrays**, using its own
sync sequences so that FT8 decoders ignore it and vice versa. On top
of normal mode it adds **slow / fast / turbo variants** that change
the symbol period, and with it the tone spacing and cycle length: slow
trades throughput for sensitivity, fast/turbo the reverse. Our FT8
engine's TX/RX split (`src/ft8.rs` no_std math + `src/ft8/rx.rs` std
engine) is the substrate: the LDPC(174,91) codec, Gray demap, GFSK
synthesis and the STFT/Costas search all carry over.

**Scope sketch (the protocol layer above the waveform):**

- **Frame types.** Several 77-bit frame kinds (heartbeat/CQ, directed
  message, compound-callsign, data frames that chain into longer
  messages), distinguished by their own type bits, not FT8's i3/n3.
- **Keyboard-message alphabet.** A variable-length coding of free text
  (Huffman-style character table) so multi-frame messages pack densely;
  messages span transmission cycles and are reassembled in order.
- **Repeat/ack semantics.** Directed messages carry ack requests, and
  the station layer retransmits unacked traffic.
- **Store-and-forward / relay.** Directed commands allow relaying
  through intermediate stations and mailbox-style message pickup.
- **APRS gateway semantics.** Defined directed commands (`@APRSIS`
  grid/message forms) let a gateway station inject traffic into APRS-IS;
  relevant to us because yodel already speaks APRS.

**GPL boundary (binding).** JS8Call, the only existing
implementation, is **GPL**, so its code cannot be a source for this
crate. JS8 support here means **implementing from the published
protocol descriptions** (frame layouts, sync arrays, alphabets and
directed-command tables), citing that documentation on each constant:
the implement-from-specification rule in
[`../CONTRIBUTING.md`](../CONTRIBUTING.md) applied with force.

**Open questions.** Variant symbol geometries need engine
parameterization: the FT8 engine hard-codes 1920 samples/symbol at
12 kHz and 6.25 Hz spacing, so slow/fast/turbo need tone spacing and
symbol length to become parameters. Whether that geometry is
const-generic (`Geometry<const SPS: usize, …>`) or a runtime config
struct is an open design call: const-generic keeps no_std buffer
sizing static, a config struct is less invasive to the shipped FT8
types. Also open: whether normal-mode RX can share `Ft8Decoder`'s
candidate search verbatim (different Costas arrays are a table swap)
or needs per-variant search tuning, and how much of the station layer
(acks, relay, mailboxes) belongs here rather than in a consumer's
application layer. The first slice is frames + alphabet + normal-mode
waveform reuse, with the conversational state machine staged behind
it.

**Size estimate.** Normal-mode waveform reuse is small (Costas table +
type-bit handling over the existing engine, ~1 slice). The protocol
layer is the real work: alphabet + frame types + reassembly is a
WSPR-RX-sized slice, acks/relay/APRS gateway another, variant
geometries a third. That comes to **3–4 bounded slices** for a useful
subset, and more for the complete directed-command table.
