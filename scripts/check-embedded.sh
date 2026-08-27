#!/usr/bin/env bash
# Feature-matrix gates for yodel. Two independent passes:
#
#   1. embedded — cross-compiles the LIBRARY for two representative
#      bare-metal targets across every no_std feature set, proving the core
#      stays free of std and allocation. Requires the targets:
#
#        rustup target add riscv32imac-unknown-none-elf \
#            riscv32imc-unknown-none-elf thumbv7em-none-eabihf
#
#   2. tests — compiles (but does not run) the TEST SUITE for the HOST
#      target across a matching set of feature sets. The bare-metal targets
#      of pass 1 cannot host a test binary, so this concern is deliberately
#      separate: pass 1 answers "does the library build for no_std?" and
#      pass 2 answers "does the test suite still COMPILE for a partial
#      feature set?". Neither `check-embedded.sh`'s pass 1 nor CI's
#      `cargo test --all-features` used to ask the latter, so unresolved
#      imports and alloc-dependent unit tests could rot in silence for
#      every feature set between "nothing" and "everything".
#
# Every cargo invocation below passes `--locked`. Cargo.lock is tracked
# (see .gitignore for why), and a resolver that is free to update it on
# the fly turns "the matrix is green" into "the matrix is green against
# whatever crates.io served this morning" -- which is the property the
# tracked lockfile exists to rule out. It also makes a lockfile that has
# drifted out of sync with Cargo.toml a loud failure instead of a silent
# rewrite of a tracked file.
#
# Usage: scripts/check-embedded.sh [all|embedded|tests]   (default: all)
set -euo pipefail

cd "$(dirname "$0")/.."

mode="${1:-all}"
case "${mode}" in
    all | embedded | tests) ;;
    *)
        echo "usage: $0 [all|embedded|tests]" >&2
        exit 2
        ;;
esac

# ---------------------------------------------------------------------------
# Pass 1: bare-metal LIBRARY cross-compilation (no host, no test harness).
# ---------------------------------------------------------------------------
targets=(
    riscv32imac-unknown-none-elf
    thumbv7em-none-eabihf
)
# The commas below are cargo feature-list syntax, not array separators:
# each element is one `--features` argument naming a whole feature SET,
# and `mod,demod` has to reach cargo as a single word. shellcheck reads a
# comma inside an array as the C-programmer slip of writing `(a, b, c)`,
# which is the only thing this suppression waives.
# shellcheck disable=SC2054
feature_sets=(
    mod
    demod
    mod,demod
    nrzi
    ax25
    aprs
    aprs,mod,demod
    digipeat
    micE
    kiss
    g3ruh
    g3ruh,mod
    g3ruh,demod
    g3ruh,mod,demod
    fx25
    fx25,mod,demod
    il2p
    wspr
    wspr,mod
    ft8
    ft8,mod
    m17
    tnc
    tnc,micE
    tnc,g3ruh
    mod,demod,nrzi,ax25,aprs,micE,kiss,tnc,g3ruh,fx25,il2p,wspr,ft8,m17,digipeat
)

if [[ "${mode}" != tests ]]; then
    for target in "${targets[@]}"; do
        for features in "${feature_sets[@]}"; do
            echo "==> cargo build --locked --no-default-features --features ${features} --target ${target}"
            cargo build --locked --no-default-features --features "${features}" --target "${target}"
        done
    done

    # The detached ESP32 RISC-V examples sub-crate (examples/esp32-riscv):
    # a #![no_std] library exercising the tnc feature set, built for both
    # riscv32 flavors of the ESP32-C3/C6 class. `imc` (no atomics A
    # extension) builds cleanly today because yodel's core is atomics-free;
    # `imac` matches the matrix target above.
    for target in riscv32imac-unknown-none-elf riscv32imc-unknown-none-elf; do
        #
        # The one cargo invocation in this file WITHOUT `--locked`, and
        # deliberately so: this sub-crate gitignores its lockfile (it is a
        # library whose only dependency is `yodel` by path, so there is
        # nothing to pin), and `--locked` against a lockfile that does not
        # exist yet is a hard error on any fresh checkout -- CI's included.
        echo "==> (examples/esp32-riscv) cargo build --target ${target}"
        (cd examples/esp32-riscv && cargo build --target "${target}")
    done

    echo "embedded matrix: all builds green"
fi

# ---------------------------------------------------------------------------
# Pass 2: HOST test-suite COMPILATION per feature set.
#
# `--no-run` (compile, do not execute) is the deliberate cost/benefit
# choice: it catches every unresolved import, every `Vec` that slipped into
# a no_std unit test, and every missing `required-features` declaration,
# without paying for the slow seeded-noise and BER suites that
# `cargo test --all-features` already runs in CI.
#
# Runs on the HOST target on purpose — no `--target` flag. A test binary
# needs a harness, a runtime and an allocator, none of which
# thumbv7em-none-eabihf / riscv32imac-unknown-none-elf provide; pass 1 above
# is the only place bare-metal targets appear.
#
# The list mirrors pass 1's `feature_sets` and then adds what only makes
# sense on a host: `alloc`, `std`, `wav`, the std-gated weak-signal receive
# engines, and the two adapter layers (`async`, `embassy`). `capture` is
# left out on purpose: it pulls cpal and a system audio stack, and CI's
# `cargo test --all-features` job already compiles it.
# ---------------------------------------------------------------------------
# Comma-joined feature sets again; see the note on `feature_sets` above.
# shellcheck disable=SC2054
test_feature_sets=(
    mod
    demod
    mod,demod
    nrzi
    ax25
    aprs
    aprs,mod,demod
    digipeat
    micE
    kiss
    g3ruh
    g3ruh,mod,demod
    fx25
    il2p
    wspr
    wspr,std
    ft8
    ft8,std
    m17
    alloc
    std
    wav
    tnc
    tnc,micE
    tnc,fx25
    tnc,g3ruh
    tnc,digipeat
    tnc,micE,il2p,fx25,wav
    embassy
    async
    mod,demod,nrzi,ax25,aprs,micE,kiss,tnc,g3ruh,fx25,il2p,wspr,ft8,m17,digipeat,alloc,std,wav,async,embassy
)

if [[ "${mode}" != embedded ]]; then
    for features in "${test_feature_sets[@]}"; do
        echo "==> cargo test --locked --no-default-features --features ${features} --no-run"
        cargo test --locked --no-default-features --features "${features}" --no-run
    done

    echo "test-compilation matrix: all feature sets compile"
fi
