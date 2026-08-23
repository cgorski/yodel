# Decode benchmarks

Measured with `scripts/benchmark.sh` (warble decoded-frame counts per
corpus track). Targets from the shootout partner decoder: plain profile
synthetic 72, T01 1005, T02 982, T03 100, T04 101; its E+ profile reaches
75 / 1014 / 1000 / 100 / 107. Columns are warble counts; `-` means the row
was a tuning iteration measured on Track 02 only.

> **Read this as a lab notebook, not as a specification.** Many rows
> below record configurations that were tried and *rejected*, and
> labels such as "as committed" were true when that row was measured.
>
> **Not every row is re-runnable.** Where a row names a script or a
> test, that is how to reproduce it. Rows that name neither were
> measured with throwaway probes that are not in the repository; treat
> those as recorded observations rather than as checks. Rows marked
> ESTIMATED are arithmetic on other rows, and the arithmetic is shown.
> For the constants that ship today read the source rather than this
> file: `SpaceGainSweep::DEFAULT` and `TncReceiver::new` in
> `src/tnc.rs`. As of now those are:
>
> * `SpaceGainSweep::DEFAULT` = Q8 gains `[156, 202, 262, 340, 441, 572,
>   741, 961, 1246]`, i.e. 0.609×..4.867× (−4.30 dB..+13.75 dB) in steps
>   of `8^(1/8) ≈ 1.297` (2.26 dB).
> * The effective Bell-202 bank built by `TncReceiver::new` is **11
>   chains** with Q8 gains `[156, 202, 262, 340, 194, 572, 256, 961, 441,
>   215, 345]`. Slots 4/6/8 are *overridden* to the emphasized trio
>   194/256/441, so the sweep's own 741 and 1246 go unused in this
>   configuration, and the two appended chains are **215/345**.
> * With `InputBandPass::On` the bank is instead 9 all-band-passed chains
>   at the nominal sweep gains above.
> * `discriminator::MAX_WINDOW` = **240** samples. The correlator's
>   observation window is derived from the rate, baud *and tone shift*:
>   one bit period normally, but the shortest whole multiple of `1/Δf`
>   covering a bit when the one-bit crosstalk exceeds 0.3 (equivalently
>   `4·shift < 3·baud`). Bell 202 therefore keeps one bit; the 300-baud
>   profiles get 1.5 bits. See the 300-baud section below.
> * `QuadratureCorrelator`'s `Discriminator` metric is the **smoothed
>   amplitude** difference, not the raw power difference. It is the same
>   tap `TncReceiver`'s chains take, and it no longer saturates. See the
>   FX.25 section below.
> * The receiver **skips correlator banks no active chain reads**, so
>   `SpaceGainSweep::UNITY` costs one bank and one chain instead of
>   three banks and one chain.

## The bit-clock loop is the Bell 202 sensitivity limiter

Found by the tier-1 BER suite (`tests/ber.rs`), which measures the
demodulator's raw bit error rate *before* any framing exists. Every
other noise test in the crate counts frames, and a frame count cannot
separate the tone discriminator from the clock recovery.

**The loop loses lock catastrophically and never re-acquires.** Scoring
a continuous random-data stream in 2000-bit segments (48 kHz, Bell 202,
verified twice with independent probes):

| SNR | per-segment error %, ten consecutive segments |
|---|---|
| 0 dB | 0.0 0.0 0.0 0.0 0.0 0.1 0.0 0.0 0.0 0.0 |
| −1 dB | 27.4 50.0 49.5 49.8 50.5 48.2 50.5 48.8 49.2 49.4 |
| −2 dB | 45.5 47.3 49.6 50.5 52.4 48.8 51.6 50.4 51.1 48.7 |

That is a cliff, not a roll-off, and 50% is chance: once lock is lost
the loop does not come back. The tone correlator sampled on a
**perfect clock** errs on only 7.0e−4 of bits at −1 dB, so 2–3 dB is
lost in timing recovery rather than tone discrimination.

Mechanism, from the code: `Slicer::push` nudges phase on *every* metric
sign change with no magnitude or plausibility qualification. At low SNR
the metric hovers near zero and noise supplies spurious crossings that
drag the sampling instant; as `lock` drains, the loop switches to the
**faster** search gain, which makes it *more* sensitive to the same
noise. The feedback is positive and the loop goes unstable.

HDLC frames re-acquire from their flag preamble, so this is less
severe than it sounds: the measured 50%-frame-recovery threshold is
−2.50 dB, *below* where continuous lock fails. But a full 330-byte
frame is ~2640 bits, and at −1 dB lock was lost inside the first 2000,
so long frames are exposed.

### The smoothing change already moved the cliff 3 dB, and that closed the question

Re-measured after the discriminator's decision statistic was smoothed
(see the FX.25 section). Same probe, same segments:

| SNR | per-segment error %, ten consecutive segments |
|---|---|
| −2 dB | 0.1 0.1 0.1 0.1 0.1 0.1 0.1 0.1 0.0 0.1 |
| −3 dB | 0.5 0.5 0.7 0.5 0.8 0.7 0.3 0.6 0.7 0.8 |
| −4 dB | 38.6 34.5 50.5 24.6 30.2 48.9 49.1 49.5 51.8 49.3 |

**The cliff moved from −1 dB to −4 dB**, covering the 2–3 dB the
analysis predicted, and the gain came from the statistic instead of
the loop. Those are the same mechanism: the loop's effective bandwidth
is set by its update rate, and the update rate is the metric's
crossing rate. Since **Bell 202's measured 50%-frame-recovery
threshold is −2.50 dB**, the remaining cliff at −4 dB sits *below the
SNR at which frames decode at all*, and the clock is no longer the
binding constraint.

The frame counts confirm it. Six gain-schedule policies were measured
(drain 4 / 2 / 1, latching, no schedule, and a search-gain sweep of
1–4), and **every one of them** gives the same picture:

| policy | syn | 300 | 9600 | fx25 | T01 | T02 | T03 | T04 |
|---|---|---|---|---|---|---|---|---|
| shipped (drain 4) | 74 | 74 | 61 | 92 | **999** | **985** | 100 | 98 |
| latching | 74 | 74 | 62 | 93 | 1002 | 982 | 100 | 98 |
| drain 2 | 74 | 74 | 62 | 93 | 1000 | 984 | 100 | 98 |

Drain 1, no schedule and search shift 3 land in the same band: ±1
frame on 9600 and FX.25, +1 to +3 on T01, and −1 to −3 on T02, which
puts T02 below its pinned floor of 985. **Not adopted**: the benefit
now lies entirely below the useful SNR range, and it is not worth a
ratchet.

#### T02's 985 is a lottery, not a performance level

This will mislead the next person. With `ChainVoting::Off` the same
policies collapse to a ±1 spread:

| policy | T01 | T02 | T03 | T04 |
|---|---|---|---|---|
| shipped | 998 | 982 | 100 | 98 |
| latching | 999 | 981 | 100 | 97 |
| drain 1 | 998 | 982 | 100 | 97 |

T02's voting-on number moves ±3 under *any* timing perturbation,
because `try_vote` aligns chain bit histories by correlation and a
one-sample timing shift changes the qualifying set and the voted bits
chaotically. The shipped configuration sits at the **top** of that
range, so treating 985 as a reproducible sensitivity floor blocks every
future timing change for what is largely luck. **For future timing
work**, evaluate with `ChainVoting::Off` as the primary signal, where
the corpus is stable to ±1, and re-enable voting only for the final
acceptance run.

### Gating the nudge on edge plausibility: measured, rejected

The obvious fix is to apply the nudge only when the edge lands within
`WINDOW` of where it was expected (the code already computes that
predicate, but uses it only for hysteresis). Against the committed
74 / 74 / 61 / 999 / 985 / 100 / 98 across 1200 synth, 300 Bd, 9600
and the four tracks, the gated nudge measured 74 / 73 / **62** /
**996** / **984** / 100 / 98. **Not adopted**: it breaks two pinned
corpus rows to buy one frame at 9600. The "implausible" edges carry
real information on off-air audio (multipath and inter-frame timing
jumps), and rejecting them slows re-acquisition more than it rejects
noise.

Untried, and more promising: a magnitude-qualified detector (gate the
nudge by the metric's confidence, so a crossing between two near-zero
metrics contributes nothing), or an early-late or Gardner error, which
does not depend on where a crossing falls. Either addresses the
positive feedback directly. The search gain being *faster* than the
locked gain is also worth questioning.

## What the synthetic ramp does and does not contain

Measured before trusting any result from it, because several proposed
techniques address an impairment the benchmark may not contain:

**The reference-generated ramps carry zero frequency offset.** Welch-
averaged spectra of the 300-baud, 1200-baud and FX.25 vectors put the
tone-pair midpoint at 1699.5 / 1699.8 / 1700.0 Hz against a nominal
1700, i.e. mistuning of 0.0 ± 0.5 Hz. An independent
instantaneous-frequency estimator agrees (300-baud tones at
1604.7 / 1795.3 Hz, midpoint 1700.0).

Consequences, stated so they are not rediscovered:

- **AFC, frequency diversity and mistuning-compensating adaptive
  slicers must measure as worthless on these ramps**, whatever their
  value on real air. Measuring ~zero gain from them here is *not*
  evidence against them; it is evidence the benchmark does not exercise
  them. Validate that class of work against the real corpus instead.
- The gain the correlator-window change *did* show at 300 baud is
  therefore attributable to tone non-orthogonality alone, which is
  present regardless of tuning, and not to accidental offset
  compensation.

Two measurement traps found on the way, worth keeping:

- A single long FFT of these files resolves the **frame-repetition
  comb**, not the modulation. Picking "the tallest bin near 1200 Hz"
  picks whichever comb line landed there and reports a spurious ±25 Hz
  displacement. Average short windows (Welch) instead.
- The number that indicates mistuning is the tone-pair **midpoint**. A
  symmetric outward displacement of the spectral peaks with the
  midpoint intact is the CPFSK spectrum shape, not an offset.

| Change | synthetic | T01 | T02 | T03 | T04 |
|---|---|---|---|---|---|
| baseline (pre-normalization) | 61 | 961 | 586 | 100 | 87 |
| magnitude (isqrt) envelopes, cross >>2, floor 1<<14 | - | - | 626 | - | - |
| diagnostic: fixed +5 dB space gain, no norm | - | - | 660 | - | - |
| metric smoothing >>3 | - | - | 688 | - | - |
| envelope pre-smoothing >>3 | - | - | 789 | - | - |
| final: pre-smooth >>3, attack >>2, decay >>12, span-floor >>2, metric smoothing >>3 | 61 | 966 | 802 | 100 | 90 |
| 9-chain bank, gains 0.5x..4x, as committed | 70 | 985 | 721 | 100 | 93 |
| gains 156..1246, env smoothing >>3 (full row) | 71 | 987 | 791 | 100 | 93 |
| gains 156..1246, cascaded double one-pole >>3 | 70 | 985 | 922 | 100 | 95 |
| lock-adaptive DPLL: search >>1 / locked >>4, thr 4, drain 2, window 1<<30 | 73 | 990 | 921 | 100 | 95 |
| locked >>3, thr 6, drain 4 (kept) | 73 | 989 | 923 | 100 | 95 |
| + input band-pass HP900/LP2500 (opt-in shift-coefficient one-poles) | 71 | 991 | 940 | 100 | 95 |
| band-pass HP900/LP3500 | 75 | 991 | 944 | 100 | 94 |
| sampling-phase stagger: chain i starts at i/9 of a bit period | 73 | 989 | 923 | 100 | 95 |
| per-chain filter mix: odd chains band-passed (HP900/LP3500), even raw, + phase stagger | 74 | 991 | 938 | 100 | 96 |

Those rows are milestones out of about seventy variants. Every knob
was swept until it went flat; the families below are recorded by
variant count and by the Track 02 range they covered.

| family swept | variants | T02 span | outcome |
|---|---|---|---|
| envelope normalization (attack, decay, span-floor, metric smoothing, pre-smoothing, peak-only vs peak-to-valley span, mid-span centering, DC block) | 33 | 577-802 | kept as in the final row above: magnitude envelopes beat power envelopes, smoothing the metric beats not smoothing it, and pre-smoothing ahead of the trackers was the largest single step |
| chain-bank gain range (128..1024, 128..1280, 156..1246, 181..1436) | 4 | 752-901 | 156..1246 kept |
| envelope smoothing shift (>>3, >>4, >>5) | 3 | 655-878 | >>3 kept; >>4 is 63/982/878/100/92, buying T02 for 8 synthetic frames |
| cascade shape (symmetric double, asymmetric >>2 and >>4, triple) | 4 | 890-924 | symmetric double >>3 kept; the triple is 64/983/924/100/96, the same trade |
| correlator window (3/4 and 7/8 of a bit period) | 2 | 417-590 | rejected; narrowing is bad at 1200 baud |
| lock-adaptive DPLL (locked >>2..>>5, thr 4 and 6, drain 2 and 4, window 1<<29 and 1<<30) | 8 | 919-923 | flat, and 72-73 synth / 986-990 T01; locked >>3, thr 6, drain 4 kept on that flatness |
| band-pass corners (HP600/HP900 against LP2500/LP3500/LP5000) | 4 | 934-944 | HP900/LP3500 kept; LP5000 identical, so the upper corner is not critical above ~3.5 kHz |
| PLL-inertia diversity (odd chains locked >>2, >>4, >>5) | 3 | 923 | rejected at 73 / 987-988 / 923 / 100 / 95, T01 −1 to −2 and nothing else moved; the filter mix took its place |

The phase-stagger row was neutral on its own but is kept, because it
is free and helps the mixed bank's short-preamble acquisition.

Status: current best (default config) is the 9-chain bank with the
lock-adaptive DPLL plus **input-filter diversity**: odd-indexed chains
consume a band-passed copy of the input (one-pole HP ~900 Hz cascaded
with one-pole LP ~3.5 kHz, shift coefficients, Q8 state, second
correlator instance) while even chains stay on the raw stream, and
each chain's DPLL starts phase-staggered i/9 of a bit period apart.
That scores **74 / 991 / 938 / 100 / 96**, with every real-world row
improved or held (T01 +2, T02 +15, T04 +1, synth +1, T03 exactly 100).
Forcing the filter on every chain (now
`TncConfig::with_band_pass(InputBandPass::On)`; at measurement time the
toggle was a bool) measured 75 / 991 / 944 / 100 / 94, stronger on the
noisy rows but one T04 frame short, so the mixed bank is the default
and `with_band_pass` remains available for known de-emphasized/noisy
channels.

Four variations on that mix are **all rejected**, none strictly
better: filtering 2 of 3 chains (i%3!=0) 75/990/943/100/94, filtering
even chains instead of odd 72/988/942/100/93, and adding an inertia
mix at locked >>4 74/990/940/100/96 or at locked >>2 73/991/939/100/96.
Each trades one or two frames on T01/T04/synth for T02 gains.

**Pre-emphasis EQ chains** are the next adopted step: a
first-difference high boost `y[n] = x[n] − a·x[n−1]` with Q8 `a`,
followed by the existing band-pass to strip the boosted hiss, on a
third correlator instance. The post-band-pass is what makes the lever
work, since without it the boost is a wash at 938 on both chain pairs
tried. With it, a=216 on chains 0+8 gives 991/955/100/95, one
emphasized chain instead of a pair gives 947 (chain 0) or 949
(chain 8), and sweeping the coefficient on chains 6+8 at a=192, 216,
240, 250 and 256 lifts T02 monotonically 947, 955, 963, 968, 969. The
pure first difference wins; only a=192 buys a T04 frame
(74/993/947/100/97) and it gives back 22 frames on T02. Adopted: even
chains 4/6/8 emphasized at `a`=1.0 in Q8=256
(`y[n] = x[n] − x[n−1]`) with gains 194/256/441, odd chains
band-passed, chains 0/2 raw, so the bank runs **three input
variants**. **74 / 995 / 970 / 100 / 96**, T02 +32 and T01 +4 with
every other row held, which closes most of the T02 gap against the 982
target.

Two **single-bit-flip repair passes** ship, and both recover zero
additional frames on this corpus. `RecoveryPolicy::SingleBitFlip`
(default-on in `TncConfig`, per chain before dedup so repaired bytes
dedup against clean copies) measured 70 / 985 / 922 / 100 / 95 with
recovery on, off, and with the FCS-field-flip case additionally
accepted. `RecoveryPolicy::PreDestuffFlip` (also default-on: on FCS
failure, and on the non-byte-aligned closes that signal corrupted
stuffing bits, the deframer retries from the raw pre-destuff bit window
with each single line bit flipped, re-destuffs, and accepts only
candidates with a valid FCS that parse as a sane UI frame, FCS-field
self-repairs rejected) held 74 / 995 / 970 / 100 / 96 both with the
FCS-failure trigger alone and with the misaligned-close salvage
trigger added. T03 stayed at exactly 100 with zero FCS errors in every
case, so both stay enabled; the pre-destuff pass is bounded (fixed
4096-bit window, no allocation) and unit tests cover the
corrupted-stuffing-run case the post-destuff repair provably cannot
fix. The null result establishes that post-bank CRC failures carry
multi-bit bursty damage, so the remaining gaps (T01 995 vs 1005,
T02 970 vs 982, T04 96 vs 101) are not single-bit phenomena.

The **HDLC dense-traffic audit** (`tests/hdlc_edge.rs`) covers
shared-flag back-to-back frames, one-flag preamble, abort-then-flag
recovery, FCS-failure not eating the next opening flag, and runt
garbage (byte-aligned and misaligned) between flags. **All five edge
cases already pass**, so the deframer needed no fix, the counts are
unchanged (74/995/970/100/96), and the tests are kept as regression
guards.

**Fade-freeze DPLL** (aimed at T04 flutter): a slow one-pole average
of total tone energy (mark + scaled space, per chain) froze phase
nudges and lock-counter updates while the instantaneous energy sat
below a fraction of that average, with hysteresis on the exit
threshold. Against the 96 baseline on T04, freezing below avg/4 with
resume above avg/2 scored 96 at energy-average shift 9 and 96 again at
the slower shift 11; the aggressive freeze below avg/2 with resume
above avg lost a frame at 95. **Rejected and reverted**: the locked
DPLL's >>3 gain plus the lock-hysteresis drain already coasts through
flutter fades, and an energy gate only adds a way to miss real
re-acquisition edges.

**Chain-bank retune**, re-purposing chain 2 as a fourth emphasized
chain at an intermediate gain (2/4/6/8 at Q8 gains 194/330/256/441):
T01 988, T02 979. T02 gained +9 but T01 lost 7 frames (995 → 988,
below the floor), because chain 2 (raw, gain 262) is a winner on the
flat dense-traffic track. **Rejected and reverted.** With 9 chains,
any reassignment toward the de-emphasized channel trades T01/T04
frames for T02 frames, so more emphasized coverage has to come from
new chains.

**11-chain bank**, which is what that implies. Keep the 9 committed
chains byte-for-byte identical and *append* two emphasized chains at
intermediate Q8 gains, between the existing trio's 194/256/441, with
half-step-offset phase stagger. Active only for the full-width default
sweep, since custom sweeps and band-pass-on configurations keep 9 or
fewer chains. RAM cost is two more deframer buffers (~660 B at the
default `N`), and the embedded matrix still builds. Gains 225/330
measured 74/996/979/100/96; 205/370 and 220/340 gave T02 979 and 981
at the same T01 997; 210/350 gave 74/997/982/100/96. The adopted
**215/345** is a strict improvement at **74 / 997 / 982 / 100 / 98**:
T01 +2, T02 +12 and T04 +2 over the 9-chain best with every other row
held (T03 exactly 100), and the **T02 target of 982 is met**.

**Bit-clock-aligned cross-chain candidate voting** (landed,
default-on). Alignment is the difficulty: chains carry diverse DPLL
phase staggers, lock states and input filters by design, so their bit
clocks are not sample-aligned and a naive majority vote is
meaningless. Each chain keeps a bit-packed ring of its recent
post-NRZI bits. When a chain's deframer closes a frame that fails the
FCS check (after every bit-flip recovery pass), the receiver takes that
chain's raw pre-destuff frame window and, for each other chain, slides
that chain's bit history 0..8 bits against the window and keeps the
best whole-window agreement; copies agreeing on ≥ 90% of bits join a
per-bit majority vote (ties keep the failed chain's own bit). With at
least three voters, the voted window is destuffed and FCS-checked once,
plus one single-bit-flip pass, and must still parse as a sane UI frame
and pass content dedup before being emitted. Bounded work only on FCS
failures; fixed buffers (11 × 514 B history rings); embedded matrix
still green. Config: `TncConfig::with_voting` (now the two-variant
`ChainVoting` enum; a bool at measurement time), default on.

That first form scores **998 / 983 / 100 / 98**, as do a wider slide
of ≤16 and a relaxation to agree ≥80% at ≥3 voters. The kept form,
slide ≤8 with agree ≥80%, ≥2 voters and other-chain weight 2, reaches
**998 / 984 / 100 / 98**, because weighting each qualified other-chain
copy 2 against the failed chain's 1 lets a single well-aligned healthy
chain outvote the damaged copy. That is T01 +1 and T02 +2 over
997/982 with T03 exactly 100 and T04 held at 98. No row regresses, so
it is **kept and default-on**, and `tests/benchmark.rs` was raised to
998/984/100/98.

Re-sweeping the DPLL on top of the voting bank, locked gain >>4 loses
T01 (997/984/100/98), and lock thresholds 4 and 5 lift T01 hard
(1000/983/100/97 and 999/984/100/97) at the cost of a T04 flutter
frame. **Lock threshold 7 is kept**: one more consecutive in-window
crossing before dropping to the low locked gain, a strict win at
**999 / 985 / 100 / 98**.

A closing grid of nine variants (slide ≤2 and ≤4, agreement ≥70% and
≥75%, ≥3 and ≥5 voters required, DPLL locked gain >>4, lock threshold
8, and the previous threshold 6 at 998/984/100/98) beat that on no row
and moved T04 on none. Tightening the slide or requiring ≥3 voters
changes nothing; relaxing agreement below 80% or demanding ≥5 voters
costs a T02 frame; locked >>4 costs a T01 frame. Every alternative
left T03 at exactly 100, no false positives anywhere. The
single-bit-flip pass on the voted window was confirmed already applied
(`try_voted_window` runs a plain destuff + FCS check and then one full
single-bit-flip pass on the voted bits). The grid is exhausted.


## Final status

Current best (default config), pinned by `tests/benchmark.rs`
(999/985/100-exact/98 for the four real-world tracks):

| corpus | target | warble | met? |
|---|---|---|---|
| synthetic-noise-100 | 72 | **74** | yes (+2) |
| T01 40-min traffic | 1005 | **999** | no (−6) |
| T02 de-emphasized | 982 | **985** | yes (+3) |
| T03 flat Mic-E canary | 100 | **100** | yes (exact) |
| T04 drive-test flutter | 101 | **98** | no (−3) |

Won from the pre-normalization baseline: T02 586 → 985 (target beaten
by 3), synthetic 61 → 74 (target beaten), T01 961 → 999, T04 87 → 98,
T03 held at exactly 100 throughout.

Remaining gaps: T01 999 vs 1005 (−6) and T04 98 vs 101 (−3). Voting
recovers frames only when at least one other chain decoded the window
nearly cleanly. The residual T04 failures are flutter fades where
*every* chain's copy is damaged in the same fade, and the residual T01
misses are dense-traffic collisions and weak openers; neither is
reachable by re-weighting the existing copies. Plausible next levers,
all new mechanisms: soft-decision (per-bit confidence-weighted) voting
using the correlator metric magnitude, voting triggered on misaligned
closes as well as FCS failures, or a wider gain bank. The `#[ignore]`d
`tests/benchmark.rs` guards the record: raise its constants with any
new best, never lower them.


## 300 baud (first-class baud/tone presets)

Arbitrary baud+tone is first-class (`ModemProfile` presets
`BELL_202`, `HF_APRS_300`, `BELL_103[_ORIGINATE/_ANSWER]`).
The non-Bell-202 profiles run a **single balanced decision chain**
(`SpaceGainSweep::UNITY`): the 11-chain bank's gains and emphasis
chains encode Bell-202-specific channel-tilt compensation measured on
the VHF corpus and were not extrapolated to 300 baud.

Methodology: synthetic corpus via the reference generator's
increasing-noise mode at 300 baud (`-B 300`, auto-selecting
1600/1800 Hz, 100 frames), decoded by the reference decoder
(`-B 300`, standard and `E+` profiles) and by
`warble decode --preset hf300`. The five pinned 1200-baud rows above
are untouched; the 300-baud row is additive and informational.

The clean-audio differential leg (`tests/differential.rs::
differential_300_baud`, 100-frame sub-corpus, 44.1 kHz) is exact in
both directions: our 300-baud TX into the reference decoder `-B 300`
is **100/100** byte-for-byte, and the reference generator `-B 300`
into our receiver is **100/100** byte-for-byte. Bell 103 presets have
no reference leg and are verified by self-loopback (`tests/tnc.rs`).

### The 300-baud deficit was tone non-orthogonality

Under the increasing-noise ladder the single-chain 300-baud receiver
first trailed the reference by 12 frames, and this document diagnosed
that as a missing 300-baud-tuned chain bank. **That diagnosis was
wrong.** No chain bank was needed: the deficit was a **single
constant**, the correlator's observation window. 300 baud now leads
the reference.

| corpus | ref | ref (E+) | warble (before) | warble (after) |
|---|---|---|---|---|
| synthetic-noise-100 at 300 Bd | 70 | 72 | 58 | **74** |

Two tones are orthogonal under non-coherent detection only when the
shift `Δf` and the observation time `T` satisfy `Δf·T ∈ ℤ`; off that
grid the correlators leak into each other by `ρ = |sin(πh)/(πh)|`,
`h = Δf·T`, and that crosstalk is a noise term no averaging removes.
The window was one bit period, so `h = Δf/baud`:

| profile | Δf | baud | `h` | crosstalk `ρ` |
|---|---|---|---|---|
| Bell 202 | 1000 Hz | 1200 | 0.833 | 0.191 |
| HF APRS 300 | 200 Hz | 300 | 0.667 | **0.413** |

At 300 baud a 1.5-bit window is 5 ms = exactly `1/200 Hz`, so `h`
becomes 1.000 and `ρ` goes to zero. Measured sweep (each point is a
rebuild of the one constant, decoding the same two ramps):

| window | 300 Bd | 1200 Bd |
|---|---|---|
| 1.00 bit | 58 | **74** |
| 1.10 bit | 64 | 73 |
| 1.20 bit | 71 | 73 |
| 1.33 bit | 73 | 69 |
| **1.50 bit** | **75** | 65 |
| 1.67 bit | 73 | 55 |
| 1.75 bit | 71 | 50 |
| 2.00 bit | 63 | 28 |

The 300-baud column peaks at the orthogonal point, and the ordering
1.5 > 2.0 > 1.0 reproduces the predicted Eb/N0 ordering
(6.8 < 7.4 < 9.5 dB) for crosstalk 0.000 < 0.207 < 0.413: a
three-point confirmation of the mechanism, not a curve fit.

**Bell 202 was left alone.** Its orthogonal point is 1.2 bits, and
widening to it measured *worse* (73 vs 74): there was only
`ρ = 0.191` to recover and the extra transition smearing costs more
than that. The rule applied is "orthogonalize only when the one-bit
crosstalk exceeds 0.3". Since `ρ` decreases monotonically on
`h ∈ (0,1]`, that is exactly `h < 0.75`, i.e. `4·shift < 3·baud`.
Bell 202 sits above the line, the 300-baud profiles below it. All four
pinned 1200-baud corpus rows re-measured **bit-identical**
(999 / 985 / 100 / 98), as did the synthetic row (74).

Costs: `MAX_WINDOW` rose 160 → 240 samples, and it backs a fixed
array, so `DefaultTncReceiver` grew 33 168 → 40 848 B and
`AfskDemodulator` 5272 → 7832 B. Every profile pays that; only the
300-baud ones use it. Windows past 181 samples also leave the
multiply-shift reciprocal's proven exactness bound (`n²·2²⁴ < 2³⁹`)
and take a real division instead; that cost lands only on the profiles
that gained.

Bell 103 shares the 200 Hz / 300 baud geometry and is orthogonalized
by the same rule, but it has no reference leg, so the improvement
there is inferred from the shared geometry rather than measured.


## 9600 baud G3RUH (scrambled baseband)

The G3RUH 9600-baud scrambled-baseband scheme
(`ModemProfile::G3RUH_9600`, `g3ruh` feature): TX synthesizes
band-limited baseband pulses directly (no audio tones) after the
`x^17 + x^12 + 1` LFSR scrambler; RX is a windowed-sinc low-pass,
baseline/amplitude AGC under quantized decision feedback, a
zero-threshold slicer and a fractional-N DPLL, descrambling raw sliced
bits before NRZI decode. The receiver is a single chain (the
multi-chain bank's gains encode Bell-202 emphasis compensation and do
not apply to baseband).

Interoperability at 9600 baud is exact on clean audio in both
directions, asserted by the differential leg (`tests/differential.rs::
differential_9600_baud`, 100-frame sub-corpus, 44.1 kHz): our
9600-baud TX into the reference decoder `-B 9600` is **100/100**
byte-for-byte, and the reference generator `-B 9600` into our receiver
is **100/100** byte-for-byte. Everything below left that untouched.

Synthetic increasing-noise comparison (reference generator
`-B 9600 -n 100`, additive `scripts/benchmark.sh` row; the five
pinned 1200-baud rows are untouched):

| corpus | ref | ref (E+) | warble (was) | warble (now) |
|---|---|---|---|---|
| synthetic-noise-100 at 9600 Bd, 44.1 kHz | 62 | 62 | 35 | **61** |
| synthetic-noise-100 at 9600 Bd, 48 kHz | 66 | 67 | 35 | **66** |

### What the 27-frame deficit was

The original diagnosis in this document was **wrong, and the fix it
proposed measured worthless**. It read "the zero-threshold slicer plus
a single DPLL inertia setting loses lock", prescribing "a small bank
of DPLL inertia/phase variants, mirroring the 1200-baud chain-bank
win".

**Timing diversity buys nothing here.** Running independent receivers
at every sub-sample start offset yields the *identical* frame set at
both rates. The DPLL converges to the same decisions regardless of
start phase, so timing recovery was never the limiter and a phase bank
would have cost RAM for zero gain.

The limiter came from a different probe: **hard-limiting the input**
(clipping at ~20% of peak) lifted recovery from 35 to 56 on its own.
That points at the amplitude domain, and at the baseline estimator,
which was a **peak/valley midpoint**. Such a midpoint is an **order
statistic**, set by the single largest excursion in its window, which
at low signal-to-noise is a noise spike rather than a symbol; that is
why clipping helped. It had been borrowed from the AFSK envelope path,
where mark and space differ in amplitude and no balance can be
assumed. But G3RUH scrambles the data to make the channel stream
DC-balanced: over any window of more than a few bits ones and zeros are
equiprobable, so the **mean** of the filtered signal is exactly the
decision threshold, and being an average it drives noise down instead
of chasing it. Swapping the order statistic for a slow mean
(`BASELINE_SHIFT`, 2^9 samples ≈ 102 bit periods, comfortably inside
the default 32-flag preamble) was the single biggest win.

### The receive filter

Two further corrections, both from G3RUH's own 1995 design paper:

* **Span.** One bit period leaves only ~5 taps at 9600 baud, far too
  short for a usable low-pass. Three bit periods (15 taps) is
  materially better, and costs nothing at lower baud rates because they
  already clamp at `MAX_FIR_TAPS`.
* **Cutoff.** The paper is explicit that the **transmitter** carries the
  matched filter: its FIR pre-equalises the whole channel so the pulse
  arriving at the detector is a Nyquist pulse (flat to 3300 Hz, −6 dB at
  4800 Hz, band-limited to 6300 Hz, i.e. a raised cosine with roll-off
  ≈ 0.31). The receive filter is only an anti-alias/noise filter; the
  paper's own hardware used a 3rd-order Butterworth at 6 kHz. So
  lowering our cutoff toward the theoretical band edge *hurt*, roughly
  halving recovery at 0.5·baud: it distorts an already-correct pulse
  and pays in inter-symbol interference. The committed 0.8·baud sits
  above the 0.66 band edge by design, because at 15 taps the
  windowed-sinc transition band is wide enough that the real −3 dB
  point lands near the theoretical edge.

### The decision threshold, again

A plain mean fixed the outlier sensitivity but left a subtler error.
"Equiprobable ones and zeros" only holds *asymptotically*: over a finite
window the imbalance is a random walk, so the mean carries a
data-dependent error scaling as `1/sqrt(window)`, an RMS wander of
roughly a tenth of the eye at a 2^9-sample time constant and
occasionally far worse. That error is **common to every bit in the
window**, so the slicer cannot average it away. Subtracting the
*decided* symbol before averaging removes the data term at source:
what gets averaged is the residual, which holds only channel offset and
noise. The technique is classically "quantized feedback", the same idea
as baseline-wander correction in AC-coupled Ethernet PHYs. That took
65 → 66 at 48 kHz and 58 → 61 at 44.1 kHz. One consequence: under
decision feedback a *constant* input reads as an unbroken run of ones
instead of decaying to an ambiguous zero metric. Scrambling guarantees
transitions on any real signal, so the degenerate case does not arise
on the air, but the old "DC settles to zero metric" unit test was
encoding a property of the plain mean, and was replaced with a test of
the property that matters (the baseline converges onto a channel offset
under modulation).

### What is left

48 kHz now matches the reference exactly; 44.1 kHz is one frame behind.
The residual difference is almost certainly the non-integer sampling:
48 kHz gives exactly 5.000 samples/bit, 44.1 kHz gives 4.59375.
Nearest-sample selection at a fractional ratio lands the strobe up to
0.109 symbol off the ideal instant, and because the pulse is Nyquist
that costs inter-symbol interference rather than amplitude. Modelling
it as a raised cosine with roll-off 0.31 puts the penalty at roughly
0.2–0.4 dB, the right order for one frame on this ramp. A
fractional-delay interpolator (an 8-tap MMSE filter, or a 32-arm
polyphase bank, driven by the timing NCO) is the standard fix and the
obvious next lever. Unlike the phase bank, it has not been ruled out by
measurement.

#### The bit clock itself is exonerated

Before building an interpolator, the cheaper hypothesis was checked:
that the strobe advance might be pinned to one integer instead of
dithering between 4 and 5 samples. **It is not, and the clock is
excellent.** `src/slicer.rs` is a fractional-N NCO (Bresenham) phase
accumulator, not a `mu`-basepoint design; the advance emerges from
where the accumulator overflows and is never stored, so no line *can*
pin it. Measured over 800 000 bits of real G3RUH audio:

| rate | adv 4 | adv 5 | mean samples/bit | expected | error |
|---|---|---|---|---|---|
| 44 100 Hz | 40.625% | 59.375% | 4.5937502 | 4.59375 | +5e−6 % |
| 48 000 Hz | — | 100% | 5.0000000 | 5.0 | 0 % |

The 44.1 kHz mix is exactly the theoretical `13/32 : 19/32`, with no
drift across the run, and accumulated timing error over a full
330-byte frame is ~5e−4 samples. So the remaining frame is *not* clock
rate or clock drift; if it is timing at all it is the sub-sample strobe
**position**, which is what an interpolator would address.

Two observations from that measurement point the opposite way and are
worth checking before any interpolator work:

- **44.1 kHz has the *tighter* strobe placement of the two**: jitter
  σ = 0.062 bit cell against 48 kHz's 0.104. The 4/5 dither appears to
  act as beneficial dither on the crossing detector's quantization.
- **At 48 kHz on clean audio the loop sits in a ±1-sample limit cycle**
  (25/50/25% across advances 4/5/6, versus 100% fives open-loop), which
  *improves* to 5/90/5% once noise is added. Suspected cause: the
  crossing is detected on the first sample *after* the sign change
  (`src/slicer.rs:178`), biasing the edge estimate ~half a sample late;
  at exactly 5.0 samples/bit that bias is identical every transition so
  the loop never averages it out. Unverified as a frame-count effect.


## FX.25 (forward error correction wrapper)

The FX.25 FEC layer (`fx25` feature) is `src/rs.rs`
(RS(255,k) codec over GF(256), parity 16/32/64) and `src/fx25.rs`
(the 11 published correlation tags, `wrap`/`wrap_with` TX,
`Fx25Receiver` tag-hunting RX beside a parallel plain HDLC path), plus
CLI wiring: `warble encode --fx25` wraps each frame in an FX.25
codeblock before modulation, `warble decode --fx25` uses the
FX.25-aware receive path (which also still decodes plain AX.25).

Reference-tooling notes (verified by running the tools): the reference
generator transmits FX.25 with `-X 1` (16/32/64 select a specific
check-byte count); the reference decoder decodes FX.25 *always*. Its
`-d x` flag only adds FX.25 debug detail, and there is no switch to
disable FX.25 receive, so a "reference decoding our FX.25 audio as
plain AX.25 only" direction cannot be isolated on the reference side.
The additive guarantee is instead demonstrated with **our own plain
(non-FX.25) receiver** on reference-generated FX.25 audio.

The clean-audio differential leg (`tests/differential.rs::
differential_fx25`, 100-frame sub-corpus, 1200 baud, 44.1 kHz) is
exact in all three directions, **100/100** byte-for-byte each:
reference generator `-X 1` into our FX.25-aware receiver; the same
reference FX.25 WAV into our **plain** receiver (the additive
guarantee, since the embedded frame keeps flags, stuffing and FCS);
and our FX.25-wrapped TX into the reference decoder, which logs `FEC
complete with no errors` under `-d x`.

Under the increasing-noise ladder (reference generator `-X 1 -n 100`,
additive `scripts/benchmark.sh` row; the five pinned 1200-baud rows are
untouched) the FX.25-aware path first scored **60**, against the
reference's 82, because it runs a **single** demodulator chain. The
11-chain bank with its FCS-repair and voting machinery lives in
`TncReceiver`, whose tone paths were held byte-identical to the
pre-FX.25 crate, and the tag hunter sits on a separate single-chain
pipeline. For comparison, `warble decode` *without* `--fx25` scores
70 on the same WAV, because the multi-chain plain receiver decodes the
embedded frames directly but ignores the RS parity. The noisy row is
informational; the pinned interop floors live in the differential test.

### Where the 60 comes from

A standing hypothesis held that the 60 might be the **plain AX.25
path**, since FX.25 keeps the embedded frame legacy-decodable by
design and the RS stage could be contributing nothing. Measured by
replicating `Fx25Receiver`'s state machine with per-outcome counters
over the same ramp:

| outcome | count |
|---|---|
| correlation tags detected | 86 |
| RS decode, block arrived **clean** (0 corrections) | 58 |
| RS decode, block **corrected** | 2 (8 symbols) |
| RS decode, **uncorrectable** | 26 |
| frames via the FX.25 path | **60** |
| frames via the parallel plain path | **0** |
| frames a legacy-only HDLC deframer gets from the same bits | 60 |

**The hypothesis is refuted.** Every frame comes through the FX.25
path and the plain path contributes zero, because once a tag locks the
receiver is in `Collect` and the embedded HDLC never reaches the
parallel deframer. RS is alive but earns only 2 of the 60.

### The fix was the decision statistic, not anything FX.25

| corpus | ref | ref (E+) | warble before | warble after |
|---|---|---|---|---|
| synthetic-noise-100 FX.25 | 82 | 91 | 60 | **92** |

**One change, and it is not in `src/fx25.rs` at all.** The bare
[`QuadratureCorrelator`] `Discriminator` metric returned the
*unsmoothed* power difference, while `TncReceiver`'s chains have always
taken the smoothed amplitude tap. That bare metric is the one
`AfskDemodulator` slices, and therefore the one the FX.25 and IL2P
receive chains run on. Since only the sign reaches the slicer and
`sign(√a − √b) ≡ sign(a − b)`, the square roots change nothing; the
gain comes from the smoothing.

Why it matters so much is a nice piece of theory. The bit-clock loop
retimes on every **sign change** of the metric, so what it cares about
is the metric's zero-crossing *rate*, and by Rice's formula that rate
is set by the second spectral moment, not the noise power. A one-bit
boxcar correlator has a triangular autocorrelation whose corner at the
origin makes that moment diverge in continuous time, bounded only by
the sample rate, so the unsmoothed statistic floods the loop with noise
crossings exactly when the signal is weakest. MEASURED, crossings per
bit at 48 kHz:

| input | unsmoothed | smoothed |
|---|---|---|
| noise only | **4.40** | 0.899 |
| signal, −1 dB | 0.729 | 0.498 |
| signal, −3 dB | 0.926 | 0.499 |

0.498 is exactly the transition density of random data. Because a
first-order loop's bandwidth scales with its update rate, the
unsmoothed statistic inflates the effective loop bandwidth ~9× at the
worst possible moment. Same root cause as the Bell 202 lock-loss cliff
above, seen from the other side.

Every pinned corpus row, the synthetic 1200-baud row, the 300-baud row
and the 9600 row were **byte-identical** after the change, since those
paths already used the smoothed tap or (G3RUH) a different front end
entirely. Cost: two integer square roots per sample, MEASURED 0.091 s →
0.165 s on a single-chain FX.25 decode of a 100-frame file;
`TncReceiver` already paid it.

It also supersedes the "feed the tag hunter the 11-chain bank"
prescription below as the *first* lever: diversity measured **nothing**
on a flat channel, the union over 11 chains equalling the best single
chain. Its value is worst-case robustness on tilted channels, a real
but separate argument.

Two further conclusions fall out:

- **Codeblock shortening is provably correct.** 58 blocks decoded with
  a zero syndrome. A misplaced parity boundary (the `k_radio` versus
  `k_rs` question: parity sits at the on-air data length, with the
  virtual zeros prepended conceptually and never transmitted) would
  produce nonzero syndromes on *clean* blocks. It does not, so that
  whole class of suspicion is closed.
- **The deficit is demodulator-limited, not FEC-limited.** The losses
  are 14 frames whose tag was never detected plus 26 whose block was
  uncorrectable, both upstream of RS. RS(255,239) corrects 8 symbol
  errors, and at the SNR where bits start failing a single bad bit
  spoils a whole byte, so blocks go uncorrectable quickly. Feeding the
  tag hunter better bits (the 11-chain bank) is the lever; tuning the
  RS stage is not.



## Embedded cost: throughput, cycle budget and RAM

Everything above measures **decode accuracy**, meaning how many frames
come back. This section measures **cost**: how much CPU and RAM the
same decoders need, and whether that fits a no-FPU riscv32 ESP32-class
part.

Every number is labelled **MEASURED** (on a desktop-class x86_64/arm64
host, so machine-dependent) or **ESTIMATED** (extrapolated to rv32 with
the assumptions stated inline). **No on-device number is claimed as
verified.** Re-run the host side yourself with:

```sh
cargo run --release --example throughput --features tnc,g3ruh,fx25
```

The user-facing summary of all this is the guidance table in
[EMBEDDED.md](EMBEDDED.md#will-it-run-on-my-chip-esp32-risc-v-feasibility):
which chip runs what, and which `DevicePreset` to pick. What follows is
the derivation behind it.

#### The fixed-point story

The `i16` decode and modulate paths, covering 1200-baud AFSK, G3RUH
9600 and the FX.25 Reed-Solomon layer, are **float-free at runtime**:
the only floating point anywhere near them is `const fn` table
generation (evaluated at compile time), the explicitly separate `*_f32`
API twins, and non-DSP APRS position conveniences. On the no-FPU
RISC-V cores (ESP32-C3/C6/H2) the recommended `push_i16`/`transmit_i16`
path is the native integer path, so there is **no soft-float penalty**.
The `_f32` APIs, by contrast, would be software-emulated on those chips
and should be avoided there.

#### Per-mode cost character

* **1200 AFSK.** Per sample: ~4 `i64` multiply-accumulates (two tones
  × I/Q quadrature correlators with sliding-window updates) plus
  envelope (integer isqrt / one-pole) work, then up to **11 cheap
  slicer chains** in the default Bell-202 emphasis-compensating bank
  (each chain is shifts/adds, but ×11 adds up; see the numbers below).
* **9600 G3RUH.** Per sample: a ≤15-tap Q15 FIR (`i64` MACs) +
  baseline/amplitude AGC under quantized decision feedback + one PLL
  slicer, at the same 48 kHz tested rate but with a single decision
  chain. (An earlier revision said "peak/valley AGC"; that design was
  replaced because it cost roughly half the frames at 9600 baud.)
* **FX.25.** Everything 1200 AFSK costs, plus an RS(255,k) decode
  over GF(256) **per frame** (a spike, not a per-sample cost).

#### MEASURED host throughput (desktop-class x86_64/arm64 host)

One representative run (your machine will differ):

| mode (i16 path) | ns/sample | xRealtime @ 48 kHz |
|---|---:|---:|
| 1200 AFSK decode, `TncReceiver`, full 11-chain default bank | ~88 | ~235× |
| 1200 AFSK decode, `TncReceiver`, single chain (`SpaceGainSweep::UNITY`) | ~25 | ~830× |
| FX.25 1200 decode (**bare `AfskDemodulator`** + tag hunter + RS) | ~23 | ~900× |
| 9600 G3RUH decode | ~10 | ~2050× |
| 1200 AFSK modulate | ~2 | ~10000× |
| 9600 G3RUH modulate | ~4 | ~5200× |
| FX.25 RS(255,239) decode | ~10 µs/frame clean, ~24 µs/frame at max 8 byte errors | per-frame spike |

Re-measured by `cargo run --release --features tnc,g3ruh,fx25 --example
throughput`, median of three runs on an arm64 host. One row moved for a
reason rather than by machine noise: **FX.25 decode went from ~7 to ~23
ns/sample** when the discriminator gained envelope smoothing (two
`isqrt` per sample on the bare `AfskDemodulator` path). That change is
why the FX.25 benchmark row rose from 60 to 92 frames, so the cost
bought something; see the FX.25 section above.

Three things in that table are easy to misread, so they are called out
explicitly:

* **The full default 1200-baud receiver is heavier per sample than
  9600 G3RUH** (~88 vs ~10 ns), because the Bell-202 preset runs an
  11-chain diversity bank while G3RUH runs one chain.
* **`SpaceGainSweep::UNITY` now skips the front-end work its single
  chain does not consume.** The receiver builds three correlator banks
  (raw, band-passed, and pre-emphasized), but which of them any active
  chain reads is fixed when the chains are built, so the unused ones
  are skipped per sample instead of being computed and discarded. A
  `UNITY` bank is one raw chain, so two thirds of the front end (two
  correlator pairs, the band-pass, the pre-emphasis and a second
  band-pass) is skipped entirely. MEASURED: **60.7 → 24.2 ns/sample, a
  2.5× speedup** when measured; the ratio still holds at today's
  ~88 → ~25. Earlier revisions of this section said `UNITY` bought
  "about 28%, not an order of magnitude". That was true when the banks
  ran unconditionally, and is now superseded. The full 11-chain bank
  consumes all three banks, so it gains nothing from the skip.
* **The FX.25 row is not a single-chain `TncReceiver` measurement.**
  It times a bare [`AfskDemodulator`]: one correlator pair, no chain
  bank, no band-pass, no pre-emphasis. Going from that to a one-chain
  `TncReceiver` now costs roughly **2×** rather than the 8–10× it did
  when the unused banks ran anyway.

#### ESTIMATED rv32 cycle budget (the arithmetic, shown)

Assumptions: the host is taken as ~4 GHz (so host cycles ≈
ns/sample × 4); a further **×4 discount** covers the in-order,
single-issue rv32 pipeline (IPC well below the host's out-of-order
superscalar core) *and* the several 32-bit `mul`/`mulhu` instructions
that each `i64` multiply compiles to on rv32. Net:
**rv32 cycles/sample ≈ host ns/sample × 16** (a conservative figure;
real silicon may do better).

Cycles *available* per 48 kHz sample: 160 MHz / 48 kHz = **3333**;
96 MHz / 48 kHz = **2000**; 400 MHz / 48 kHz = **8333**.

**Provenance, because the arithmetic does not reproduce from the table
above.** The cycle column below was derived from the measurement round
that produced 83.1 / 24.4 / 7.8 ns/sample, not from the re-measured host
table in the previous section (~88 / ~25 / ~10). Applying x16 to the
current figures gives ~1410 / ~400 / ~160 and a full-to-single ratio of
3.5x rather than 3.4x. The difference is inside the noise of a x16
discount that is itself an estimate, so the column is left as measured
rather than silently re-scaled. Re-derive it in the same commit as any
re-measurement, and re-check the percentages and both chunk tables with
it. The `bare AfskDemodulator` row has no ns counterpart in the host
table at all, because the only bare-demodulator row there includes the
tag hunter and RS.

| mode | ESTIMATED rv32 cycles/sample | @160 MHz (3333 avail) | @96 MHz (2000 avail) |
|---|---:|---:|---:|
| 1200 AFSK, `TncReceiver`, full 11-chain bank | ~1330 | ~40% of core | ~66% |
| 1200 AFSK, `TncReceiver`, single chain (UNITY) | ~390 | ~12% | ~20% |
| 9600 G3RUH | ~125 | ~4% | ~6% |
| bare `AfskDemodulator` (no chain bank, no framing) | ~190 | ~6% | ~10% |
| FX.25 RS decode (per frame) | ~0.14–0.3 M cycles/frame | ~0.9–1.8 ms spike | ~1.4–3.0 ms spike |

The full bank costs about **3.4×** the single chain. That ratio used to
be ~1.4×, because the receiver built all three correlator banks whatever
the sweep length and threw the unused ones away; it now skips banks no
active chain reads, so a `UNITY` receiver pays for one chain and one
bank. When budgeting, count the front end your configuration consumes
plus your chains.

The RS spike is amortized over a whole frame (~1 s of air time at 1200
baud), so it is negligible *on average*. It must still fit in your
buffering headroom, which a few ms of sample FIFO provides.

#### Chunk-size budget: what a bounded decode chunk costs (ESTIMATED)

If you share the MCU the way [Sharing the MCU](#sharing-the-mcu)
recommends, with an ISR filling a [`SampleRing`] and the main loop (or
task) draining a fixed-size chunk through `push_i16`, then the number
you care about is *chunk cost vs chunk period*: how long one chunk takes
to decode versus how long real time takes to deliver it. Chunk period
is pure arithmetic (chunk ÷ sample rate); decode cost extends the
MEASURED host ns/sample above through the same **×16** rv32
extrapolation (ESTIMATED, same caveats), at 160 MHz (C3/C6 class).
All rows assume the tested **48 kHz** sample rate.

**1200 baud AFSK @ 48 kHz, 160 MHz** (per-sample cost: full 11-chain
bank ~1330 ESTIMATED rv32 cycles ≈ 8.3 µs; single chain ~390
≈ 2.4 µs):

| chunk (samples) | chunk period | full-bank cost (ESTIMATED) | full-bank headroom | single-chain cost (ESTIMATED) | single-chain headroom |
|---:|---:|---:|---:|---:|---:|
| 64 | 1.33 ms | ~532 µs | ~60% (~0.80 ms slack) | ~156 µs | ~88% (~1.18 ms slack) |
| 128 | 2.67 ms | ~1.06 ms | ~60% (~1.6 ms slack) | ~312 µs | ~88% (~2.35 ms slack) |
| 256 | 5.33 ms | ~2.13 ms | ~60% (~3.2 ms slack) | ~624 µs | ~88% (~4.7 ms slack) |
| 512 | 10.67 ms | ~4.25 ms | ~60% (~6.4 ms slack) | ~1.25 ms | ~88% (~9.4 ms slack) |

**9600 baud G3RUH @ 48 kHz, 160 MHz** (per-sample cost: ~125 ESTIMATED
rv32 cycles ≈ 0.78 µs, single decision chain):

| chunk (samples) | chunk period | decode cost (ESTIMATED) | headroom for other duties |
|---:|---:|---:|---:|
| 64 | 1.33 ms | ~50 µs | ~96% (~1.28 ms slack) |
| 128 | 2.67 ms | ~100 µs | ~96% (~2.57 ms slack) |
| 256 | 5.33 ms | ~200 µs | ~96% (~5.13 ms slack) |
| 512 | 10.67 ms | ~400 µs | ~96% (~10.27 ms slack) |

Reading the table: headroom (the fraction of each chunk period left
for sensors, logging, TX, radio housekeeping) is a per-sample ratio,
so it does not change with chunk size. What changes is the *shape* of
the slack: bigger chunks mean longer contiguous stretches for other
duties but a longer stall while a chunk decodes, and the ring must
hold at least a couple of chunks of intake (a `SampleRing<1024>` at
48 kHz is ~21 ms of audio). At a lower rate the budget only improves:
the balloon-tracker examples run 24 kHz, doubling every period above
for the same cost. These are steady-state numbers; the frame-close
spikes (repair sweep, FX.25 RS) are covered in
[Sharing the MCU](#sharing-the-mcu).

#### RAM footprint (MEASURED; the three struct totals are printed by the same benchmark)

`size_of` on a 32-bit-comparable layout: `DefaultTncReceiver` =
**40 848 B (~40 KiB)**, `AfskDemodulator` alone = **7832 B (~7.6 KiB)**,
`Fx25Receiver` = **2424 B (~2.4 KiB)**. Against the ESP32-C3's 400 KiB
SRAM (C6: 512 KiB, H2: 320 KiB) the full receiver is around 10% of
memory. Exact numbers for your build: the benchmark prints them.

Two caveats that the bare `size_of` hides:

* **The chain bank is a fixed-size array**, so `size_of` is *identical*
  for `SpaceGainSweep::UNITY` and the 11-chain default, both 40 848 B.
  Choosing a shorter sweep saves CPU, not RAM. Over half the struct is
  the three correlator banks (23 448 B), which are allocated whatever
  the sweep length; the rest is the 11 chains. A single-chain
  `TncReceiver` is **not** "a few KiB". If you need that, use
  `AfskDemodulator` (~7.6 KiB) with the `ax25` framing layer directly.
* **The correlator window is sized for the widest supported profile.**
  Each tone correlator holds `MAX_WINDOW` = 240 sample contributions so
  the 300-baud profiles can use their orthogonal 1.5-bit observation
  window (worth +16 frames there; see the tone-orthogonality section of
  `src/discriminator.rs`). Bell 202 still *uses* only 40 of those
  entries at 48 kHz, but pays for the array: this is what grew the
  receiver from 33 168 B. If you need the old footprint on a
  Bell-202-only build, `MAX_WINDOW` is the single constant to lower.
* **Peak stack is separate from struct size, and is ESTIMATED.** With
  the default `RecoveryPolicy::PreDestuffFlip` and `ChainVoting::On`,
  closing a frame bursts stack in the voting path, ESTIMATED at roughly
  **9 KiB**. Unlike the three struct totals above, no check in this
  repository prints or bounds that figure. Size the task stack with
  margin, or use [`TncConfig::bounded_latency`], which turns both
  off.
