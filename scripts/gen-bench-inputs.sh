#!/usr/bin/env bash
# Generates the synthetic benchmark inputs into scratch/.
#
# Three of the seven pinned benchmark rows (CONTRIBUTING.md, "Pinned
# benchmark rows") are synthetic: increasing-noise transmissions from
# the reference generator, decoded by us and floored at a frame count.
# `tests/benchmark.rs` looks for them at fixed paths under `scratch/`
# and skips with a message when they are absent.
#
# They used to be absent on every machine, because the only thing that
# generated them was `scripts/benchmark.sh`, which wrote them to /tmp
# under different names for its own use and threw them away. Running the
# benchmark script therefore did not enable the benchmark tests, and the
# three rows were skipped by everyone who had not reverse-engineered the
# filenames. One generator, both consumers.
#
# Usage:
#   YODEL_REF_GEN=/path/to/reference-generator scripts/gen-bench-inputs.sh
#   scripts/gen-bench-inputs.sh --force      # regenerate even if present
#
# The inputs are deterministic for a given generator, and gitignored
# (scratch/ is), so regenerating is cheap and never affects the tree.
set -euo pipefail
cd "$(dirname "$0")/.."

REF_GEN="${YODEL_REF_GEN:?set YODEL_REF_GEN to the reference generator}"
if [ ! -x "${REF_GEN}" ]; then
    echo "error: YODEL_REF_GEN is not an executable: ${REF_GEN}" >&2
    exit 1
fi

force=""
if [ "${1:-}" = "--force" ]; then
    force=1
fi

mkdir -p scratch

# name:flags. The flag sets mirror scripts/benchmark.sh exactly, because
# the whole point is that the script's rows and the tests' rows describe
# the same audio.
#
# `bench_noise_9600.wav` has no pinned test today -- G3RUH is an
# informational row in the shootout (docs/BENCHMARKS.md) -- but it is
# generated here so the script has one source for every row it prints.
generate() {
    local name="$1"
    shift
    local path="scratch/${name}.wav"
    if [ -f "${path}" ] && [ -z "${force}" ]; then
        echo "==> ${path} present, keeping it (--force to regenerate)"
        return
    fi
    echo "==> ${path}"
    "${REF_GEN}" "$@" -n 100 -o "${path}" >/dev/null 2>&1
    if [ ! -s "${path}" ]; then
        echo "error: the reference generator wrote nothing to ${path}." >&2
        echo "It should accept: -n <count> -o <file> [-B <baud>] [-X 1]" >&2
        exit 1
    fi
}

generate bench_noise
generate bench_noise_300 -B 300
generate bench_noise_9600 -B 9600
generate bench_noise_fx25 -X 1

echo
echo "Synthetic benchmark inputs ready. The three pinned synthetic rows now run:"
echo "  cargo test --release --all-features --test benchmark -- --ignored --nocapture"
