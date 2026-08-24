#!/usr/bin/env bash
# Verifies that every test cited in docs/COVERAGE.md still exists.
#
# docs/COVERAGE.md carries a few hundred citations of the form
# `tests/roundtrip.rs::pinned_bits_for_known_transmission` or
# `src/modulator.rs::mark_tone_48k_first_16_samples_pinned`, and it
# claims they are "checked mechanically against `--list
# --include-ignored` output from the compiled test binaries". This is
# the thing that does the checking. Without it a renamed or deleted test
# leaves a citation behind that reads as evidence and is not.
#
# Exits non-zero listing every citation with no matching test.
set -euo pipefail
cd "$(dirname "$0")/.."

DOC=docs/COVERAGE.md
work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT

echo "==> compiling the test suite"
cargo test --all-features --no-run --message-format=json 2>/dev/null \
    | sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' \
    | grep '/deps/' >"${work}/bins"

echo "==> listing tests from $(wc -l <"${work}/bins" | tr -d ' ') binaries"
: >"${work}/tests"
while read -r bin; do
    # target/debug/deps/roundtrip-1b3d8c30359bee9d -> roundtrip
    name=$(basename "${bin}" | sed 's/-[0-9a-f][0-9a-f]*$//')
    # `--format terse` prints "some::test_name: test" per line.
    "${bin}" --list --include-ignored --format terse 2>/dev/null \
        | sed -n 's/: test$//p' \
        | while read -r test; do
            printf '%s\t%s\n' "${name}" "${test}"
        done >>"${work}/tests"
done <"${work}/bins"

total=$(wc -l <"${work}/tests" | tr -d ' ')
echo "==> ${total} test functions found"

# Citations look like `tests/foo.rs::bar` or `src/a/b.rs::bar`.
grep -oE '`(src|tests)/[a-z0-9_/]+\.rs::[a-zA-Z0-9_:]+`' "${DOC}" \
    | tr -d '`' | sort -u >"${work}/cited"

cited=$(wc -l <"${work}/cited" | tr -d ' ')
echo "==> ${cited} distinct citations in ${DOC}"

missing=0
while IFS= read -r citation; do
    file=${citation%%::*}
    test=${citation#*::}
    case "${file}" in
        # A shared helper module (tests/common/mod.rs) is compiled into
        # every binary that includes it, so it belongs to no one target.
        tests/*/mod.rs) want_bin='*' ;;
        # An integration test lives in the binary named after its file.
        tests/*) want_bin=$(basename "${file}" .rs) ;;
        # A `#[cfg(test)]` unit test is compiled into one of the two
        # `warble-*` binaries (the library's and the binary's).
        *) want_bin=warble ;;
    esac
    # A citation names either a test or a module holding several (the
    # doc's own preamble says so), and the listed name carries the full
    # module path either way. So match on whole `::`-delimited segments:
    # that accepts the test itself, the test nested under modules, and a
    # module prefix covering every test inside it.
    if ! awk -F'\t' -v b="${want_bin}" -v t="${test}" \
        '(b == "*" || $1 == b) && $2 ~ ("(^|::)" t "(::|$)") { found = 1 }
         END { exit !found }' "${work}/tests"; then
        echo "MISSING: ${citation}"
        missing=$((missing + 1))
    fi
done <"${work}/cited"

if [ "${missing}" -ne 0 ]; then
    echo
    echo "${missing} of ${cited} citations in ${DOC} name a test that does not exist."
    echo "Either the test was renamed or deleted, or the citation is a typo."
    exit 1
fi

echo "==> all ${cited} citations resolve"
