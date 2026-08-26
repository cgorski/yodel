#!/usr/bin/env bash
# Decode-performance shootout: yodel vs the reference decoder, on the
# real-world corpus (corpus/*.wav, downloaded separately) and the synthetic
# increasing-noise corpus (generated fresh each run).
#
# Usage: scripts/benchmark.sh
#
# Requires the reference project's generator and decoder binaries. Point
# YODEL_REF_GEN / YODEL_REF_DECODE at them (the same two variables the
# oracle and differential test suites use; see CONTRIBUTING.md). Both
# are required: this script never guesses a path.
set -euo pipefail
cd "$(dirname "$0")/.."

# The reference binaries are operator-provided and are located only
# through the environment, never by a path baked into this file.
REF_DECODE="${YODEL_REF_DECODE:?set YODEL_REF_DECODE to the reference decoder}"
REF_GEN="${YODEL_REF_GEN:?set YODEL_REF_GEN to the reference generator}"

if [ ! -x "$REF_DECODE" ] || [ ! -x "$REF_GEN" ]; then
    echo "error: reference binaries not found." >&2
    echo "  decoder:   $REF_DECODE" >&2
    echo "  generator: $REF_GEN" >&2
    echo "Set YODEL_REF_DECODE and YODEL_REF_GEN to their paths." >&2
    exit 1
fi

# The synthetic inputs live in scratch/ under the names tests/benchmark.rs
# looks for, so that generating them here also enables the three pinned
# synthetic rows in that suite. See scripts/gen-bench-inputs.sh.
scripts/gen-bench-inputs.sh

cargo build --features cli,g3ruh --release --quiet
YODEL=target/release/yodel

# Preflight: the reference decoder must actually decode something.
#
# Every `ref` column below is a `grep -oE '^[0-9]+'` over the decoder's
# trailer, so a decoder that fails to RUN contributes an empty string
# and the table prints a blank cell -- next to yodel's real number, in a
# shootout whose entire purpose is the comparison. That is the worst
# possible failure mode for this script and it is completely silent.
#
# Seen on macOS: a Homebrew upgrade moves the gpsd symlink, the decoder
# is left linked against a libgps that no longer exists, and dyld kills
# it before main(). Note also that macOS strips DYLD_* from the
# environment of SIP-protected shells, so exporting a library path and
# then running this script may not reach the decoder -- pass it inline
# on the command that invokes the binary, or relink it.
# `|| true` is load-bearing: `set -e` plus `pipefail` would otherwise
# abort the script on the failing decoder before the diagnosis below
# could be printed, which is the same silence in a different costume.
probe=$("$REF_DECODE" scratch/bench_noise.wav 2>&1 | tail -1 || true)
if ! printf '%s' "$probe" | grep -qE '[0-9]+ packets decoded'; then
    echo "error: the reference decoder ran but printed no packet count." >&2
    echo "  binary: $REF_DECODE" >&2
    echo "  last line of its output: ${probe:-<nothing>}" >&2
    echo "Every 'ref' column would be blank, so the shootout would compare" >&2
    echo "yodel against nothing while looking like it had run. Fix the decoder" >&2
    echo "first: run it by hand on scratch/bench_noise.wav." >&2
    exit 1
fi

count_ref()    { "$REF_DECODE" "$1" 2>/dev/null | tail -1 | grep -oE '^[0-9]+'; }
count_ref_ep() { "$REF_DECODE" -P E+ "$1" 2>/dev/null | tail -1 | grep -oE '^[0-9]+'; }
count_yodel() { "$YODEL" decode "$1" 2>/dev/null | grep -c '>' || true; }
# 300-baud variants (reference tools auto-select 1600/1800 Hz below 600 Bd).
count_ref_300()    { "$REF_DECODE" -B 300 "$1" 2>/dev/null | tail -1 | grep -oE '^[0-9]+'; }
count_ref_300_ep() { "$REF_DECODE" -B 300 -P E+ "$1" 2>/dev/null | tail -1 | grep -oE '^[0-9]+'; }
count_yodel_300() { "$YODEL" decode --preset hf300 "$1" 2>/dev/null | grep -c '>' || true; }
# 9600-baud G3RUH variants (scrambled direct-baseband, no audio tones).
count_ref_9600()    { "$REF_DECODE" -B 9600 "$1" 2>/dev/null | tail -1 | grep -oE '^[0-9]+'; }
count_ref_9600_ep() { "$REF_DECODE" -B 9600 -P '+' "$1" 2>/dev/null | tail -1 | grep -oE '^[0-9]+'; }
count_yodel_9600() { "$YODEL" decode --preset g3ruh "$1" 2>/dev/null | grep -c '>' || true; }

echo "corpus                              ref  ref(E+)  yodel"
echo "------                              ---  -------  ------"
printf "%-35s %4s %8s %7s\n" "synthetic-noise-100" \
    "$(count_ref scratch/bench_noise.wav)" \
    "$(count_ref_ep scratch/bench_noise.wav)" \
    "$(count_yodel scratch/bench_noise.wav)"
for w in corpus/0*.wav; do
    [ -f "$w" ] || continue
    printf "%-35s %4s %8s %7s\n" "$(basename "$w" | cut -c1-35)" \
        "$(count_ref "$w")" "$(count_ref_ep "$w")" "$(count_yodel "$w")"
done
# Additive 300-baud row (does not touch the five pinned rows above).
printf "%-35s %4s %8s %7s\n" "synthetic-noise-100-300bd" \
    "$(count_ref_300 scratch/bench_noise_300.wav)" \
    "$(count_ref_300_ep scratch/bench_noise_300.wav)" \
    "$(count_yodel_300 scratch/bench_noise_300.wav)"
# Additive 9600-baud G3RUH row (does not touch the five pinned rows above).
printf "%-35s %4s %8s %7s\n" "synthetic-noise-100-9600bd" \
    "$(count_ref_9600 scratch/bench_noise_9600.wav)" \
    "$(count_ref_9600_ep scratch/bench_noise_9600.wav)" \
    "$(count_yodel_9600 scratch/bench_noise_9600.wav)"
# Additive FX.25 row (does not touch the five pinned rows above):
# reference FX.25 TX (-X 1) at 1200 baud with increasing noise, decoded
# by the reference (FX.25 receive always on) and by yodel's FX.25-aware
# path (single demodulator chain, see docs/BENCHMARKS.md).
count_yodel_fx25() { "$YODEL" decode --fx25 "$1" 2>/dev/null | grep -c '>' || true; }
printf "%-35s %4s %8s %7s\n" "synthetic-noise-100-fx25" \
    "$(count_ref scratch/bench_noise_fx25.wav)" \
    "$(count_ref_ep scratch/bench_noise_fx25.wav)" \
    "$(count_yodel_fx25 scratch/bench_noise_fx25.wav)"
echo
echo "target: yodel >= ref on every 1200-baud row (stretch: >= ref(E+));"
echo "the 300-, 9600-baud and fx25 rows are informational (single-chain receivers, see docs/BENCHMARKS.md)"
