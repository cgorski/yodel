#!/usr/bin/env bash
# Reports public functions that nothing outside their own module calls.
#
# CONTRIBUTING.md, "Checking that every public function is exercised":
# no public function should be reachable by users and by nothing else.
# The recipe there requires a CALL, not a mention, and splits each src/
# file at its `#[cfg(test)]` boundary so implementation code cannot
# vouch for itself. It found four unexercised functions the first time
# it was run by hand. This is that recipe, automated.
#
# A call site is any of:
#   * tests/ or examples/
#   * README.md
#   * a `///` or `//!` doc comment (a doctest is a call a user can read)
#   * an in-module `#[cfg(test)]` body
#
# Deliberately NOT a call site: another function's body in the same
# implementation region. That is the whole point -- it is how a
# function reachable only from its own crate looked exercised.
#
# Names are matched bare, so this UNDER-reports: a generic name (`new`,
# `parse`, `len`) is cleared by any one hit among dozens of definitions.
# It cannot over-report, which is what makes a non-zero answer worth
# acting on.
set -euo pipefail
cd "$(dirname "$0")/.."

work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT

: >"${work}/impl"
: >"${work}/testbody"

# Split every library source at its first `#[cfg(test)]`.
while IFS= read -r f; do
    boundary=$(grep -n '^#\[cfg(test)\]' "${f}" | head -1 | cut -d: -f1 || true)
    if [ -n "${boundary}" ] && [ "${boundary}" -gt 1 ]; then
        head -n "$((boundary - 1))" "${f}" >>"${work}/impl"
        tail -n "+${boundary}" "${f}" >>"${work}/testbody"
    else
        cat "${f}" >>"${work}/impl"
    fi
done < <(find src -name '*.rs' -not -path 'src/bin/*' | sort)

# Definitions: `pub fn` / `pub const fn` / `pub async fn` in
# implementation regions. Doc-comment lines start with `///`, so a `pub
# fn` inside a doctest cannot be picked up here.
grep -oE '^[[:space:]]*pub (const |async |extern )*fn [a-z_][a-z0-9_]*' "${work}/impl" \
    | awk '{print $NF}' | sort -u >"${work}/defs"

# The haystack: everywhere a call may legitimately come from.
{
    find tests examples -name '*.rs' -exec cat {} + 2>/dev/null || true
    cat README.md
    # Doc comments anywhere in the library, including the impl regions.
    grep -hE '^[[:space:]]*(///|//!)' "${work}/impl" || true
    cat "${work}/testbody"
} >"${work}/haystack" 2>/dev/null

# Every identifier that is called in the haystack.
grep -oE '[a-z_][a-z0-9_]*\(' "${work}/haystack" | tr -d '(' | sort -u >"${work}/called"

comm -23 "${work}/defs" "${work}/called" >"${work}/unexercised"

defs=$(wc -l <"${work}/defs" | tr -d ' ')
missing=$(wc -l <"${work}/unexercised" | tr -d ' ')

echo "==> ${defs} distinct public function names in src/ (excluding src/bin/)"

if [ "${missing}" -ne 0 ]; then
    echo
    echo "Public functions with no call site outside their own implementation:"
    sed 's/^/  /' "${work}/unexercised"
    echo
    echo "${missing} unexercised. Each is public API whose error paths and edge"
    echo "behaviour nothing verifies. tests/coverage_fill.rs is where the answers go;"
    echo "see CONTRIBUTING.md, \"Checking that every public function is exercised\"."
    exit 1
fi

echo "==> every public function has a call site"
