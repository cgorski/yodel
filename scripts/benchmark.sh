#!/usr/bin/env bash
# Decode-performance shootout: warble vs the reference decoder, on the
# real-world corpus (corpus/*.wav, downloaded separately) and the synthetic
# increasing-noise corpus (generated fresh each run).
#
# Usage: scripts/benchmark.sh
#
# Requires the reference project's generator and decoder binaries. Point
# WARBLE_REF_GEN / WARBLE_REF_DECODE at them (the same two variables the
# oracle and differential test suites use; see CONTRIBUTING.md). Both
# are required: this script never guesses a path.
set -euo pipefail
cd "$(dirname "$0")/.."

# The reference binaries are operator-provided and are located only
# through the environment, never by a path baked into this file.
REF_DECODE="${WARBLE_REF_DECODE:?set WARBLE_REF_DECODE to the reference decoder}"
REF_GEN="${WARBLE_REF_GEN:?set WARBLE_REF_GEN to the reference generator}"

if [ ! -x "$REF_DECODE" ] || [ ! -x "$REF_GEN" ]; then
    echo "error: reference binaries not found." >&2
    echo "  decoder:   $REF_DECODE" >&2
    echo "  generator: $REF_GEN" >&2
    echo "Set WARBLE_REF_DECODE and WARBLE_REF_GEN to their paths." >&2
    exit 1
fi
cargo build --features cli,g3ruh --release --quiet
WARBLE=target/release/warble

count_ref()    { "$REF_DECODE" "$1" 2>/dev/null | tail -1 | grep -oE '^[0-9]+'; }
count_ref_ep() { "$REF_DECODE" -P E+ "$1" 2>/dev/null | tail -1 | grep -oE '^[0-9]+'; }
count_warble() { "$WARBLE" decode "$1" 2>/dev/null | grep -c '>' || true; }
# 300-baud variants (reference tools auto-select 1600/1800 Hz below 600 Bd).
count_ref_300()    { "$REF_DECODE" -B 300 "$1" 2>/dev/null | tail -1 | grep -oE '^[0-9]+'; }
count_ref_300_ep() { "$REF_DECODE" -B 300 -P E+ "$1" 2>/dev/null | tail -1 | grep -oE '^[0-9]+'; }
count_warble_300() { "$WARBLE" decode --preset hf300 "$1" 2>/dev/null | grep -c '>' || true; }
# 9600-baud G3RUH variants (scrambled direct-baseband, no audio tones).
count_ref_9600()    { "$REF_DECODE" -B 9600 "$1" 2>/dev/null | tail -1 | grep -oE '^[0-9]+'; }
count_ref_9600_ep() { "$REF_DECODE" -B 9600 -P '+' "$1" 2>/dev/null | tail -1 | grep -oE '^[0-9]+'; }
count_warble_9600() { "$WARBLE" decode --preset g3ruh "$1" 2>/dev/null | grep -c '>' || true; }

echo "corpus                              ref  ref(E+)  warble"
echo "------                              ---  -------  ------"
"$REF_GEN" -n 100 -o /tmp/warble_bench_noise.wav >/dev/null 2>&1
printf "%-35s %4s %8s %7s\n" "synthetic-noise-100" \
    "$(count_ref /tmp/warble_bench_noise.wav)" \
    "$(count_ref_ep /tmp/warble_bench_noise.wav)" \
    "$(count_warble /tmp/warble_bench_noise.wav)"
for w in corpus/0*.wav; do
    [ -f "$w" ] || continue
    printf "%-35s %4s %8s %7s\n" "$(basename "$w" | cut -c1-35)" \
        "$(count_ref "$w")" "$(count_ref_ep "$w")" "$(count_warble "$w")"
done
# Additive 300-baud row (does not touch the five pinned rows above).
"$REF_GEN" -B 300 -n 100 -o /tmp/warble_bench_noise_300.wav >/dev/null 2>&1
printf "%-35s %4s %8s %7s\n" "synthetic-noise-100-300bd" \
    "$(count_ref_300 /tmp/warble_bench_noise_300.wav)" \
    "$(count_ref_300_ep /tmp/warble_bench_noise_300.wav)" \
    "$(count_warble_300 /tmp/warble_bench_noise_300.wav)"
# Additive 9600-baud G3RUH row (does not touch the five pinned rows above).
"$REF_GEN" -B 9600 -n 100 -o /tmp/warble_bench_noise_9600.wav >/dev/null 2>&1
printf "%-35s %4s %8s %7s\n" "synthetic-noise-100-9600bd" \
    "$(count_ref_9600 /tmp/warble_bench_noise_9600.wav)" \
    "$(count_ref_9600_ep /tmp/warble_bench_noise_9600.wav)" \
    "$(count_warble_9600 /tmp/warble_bench_noise_9600.wav)"
# Additive FX.25 row (does not touch the five pinned rows above):
# reference FX.25 TX (-X 1) at 1200 baud with increasing noise, decoded
# by the reference (FX.25 receive always on) and by warble's FX.25-aware
# path (single demodulator chain, see docs/BENCHMARKS.md).
count_warble_fx25() { "$WARBLE" decode --fx25 "$1" 2>/dev/null | grep -c '>' || true; }
"$REF_GEN" -X 1 -n 100 -o /tmp/warble_bench_noise_fx25.wav >/dev/null 2>&1
printf "%-35s %4s %8s %7s\n" "synthetic-noise-100-fx25" \
    "$(count_ref /tmp/warble_bench_noise_fx25.wav)" \
    "$(count_ref_ep /tmp/warble_bench_noise_fx25.wav)" \
    "$(count_warble_fx25 /tmp/warble_bench_noise_fx25.wav)"
echo
echo "target: warble >= ref on every 1200-baud row (stretch: >= ref(E+));"
echo "the 300-, 9600-baud and fx25 rows are informational (single-chain receivers, see docs/BENCHMARKS.md)"
