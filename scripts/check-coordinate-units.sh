#!/usr/bin/env bash
# Reports coordinate constructions that look like they are on the wrong
# unit.
#
# `Latitude::new` and `Longitude::new` count coordinate STORAGE units,
# of which there are UNITS_PER_DEGREE = 342 833 400 000 000 per degree.
# The unit every APRS wire format actually carries is the 1/100
# arc-minute, which is 57 138 900 000 storage units. Handing `new` a
# hundredths count is therefore off by that factor -- and it is a
# perfectly legal latitude, about nine millionths of a degree, so
# nothing rejects it and every affected fixture quietly moves to
# 0000.00N/00000.00W.
#
# tests/coordinate_paths.rs was written to prevent exactly this and says
# so in its header:
#
#   A fixture saying `Latitude::from_degrees(49.0583)` survives a unit
#   change; one saying `Latitude::new(294_349)` does not.
#
# It happened anyway, in five places, and stayed for as long as it took
# someone to run the `#[ignore]`d suites by hand. See CONTRIBUTING.md,
# "A suite CI compiles but never runs rots at the fixtures".
#
# THE RULE: the argument to `Latitude::new` / `Longitude::new` must name
# its unit. Either it goes through a `UNITS_PER_*` constant, or it names
# some other SCREAMING_CASE constant that does, or it is a plain
# variable or a small literal like 0. A bare multi-digit literal --
# `49 * 6000 + 350` -- names nothing, and neither does an argument whose
# own identifier says "hundredths" while `new` reads storage units.
#
# Deliberately a text scan, like check-public-api-exercised.sh: it needs
# no toolchain, finishes instantly, and the property it checks is about
# how the call is SPELLED, which is precisely what a compiler cannot see
# here -- both units are i64.
set -euo pipefail
cd "$(dirname "$0")/.."

work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT

# Every Rust source that can construct a coordinate. The nested ESP32
# examples sub-crate is included on purpose: it is a reference
# implementation for hardware, and it carried this defect.
find src tests examples -name '*.rs' | sort >"${work}/files"

: >"${work}/suspects"

while IFS= read -r f; do
    awk -v file="$f" '
        # Skip comment lines. A doc comment may legitimately quote the
        # wrong spelling in order to warn about it, which is what
        # tests/coordinate_paths.rs does.
        /^[[:space:]]*(\/\/|\*)/ { next }

        /(Latitude|Longitude)::new\(/ {
            arg = $0
            sub(/.*::new\(/, "", arg)

            # Named its unit through a constant: cleared.
            if (arg ~ /[A-Z][A-Z0-9_]{3,}/) next

            # A bare literal of two or more digits, or an argument that
            # calls itself hundredths while `new` reads storage units.
            if (arg ~ /[0-9][0-9]/ || arg ~ /hundredth/) {
                printf "%s:%d: %s\n", file, FNR, $0
            }
        }
    ' "$f" >>"${work}/suspects"
done <"${work}/files"

scanned=$(wc -l <"${work}/files" | tr -d ' ')
found=$(wc -l <"${work}/suspects" | tr -d ' ')

echo "==> scanned ${scanned} sources for coordinate constructions"

if [ "${found}" -ne 0 ]; then
    echo
    echo "Coordinate constructions whose argument does not name its unit:"
    sed 's/^/  /' "${work}/suspects"
    echo
    echo "${found} suspect. \`Latitude::new\`/\`Longitude::new\` count STORAGE units"
    echo "(UNITS_PER_HUNDREDTH_MINUTE = 57 138 900 000 of them per 1/100 arc-minute)."
    echo "Scale through the named constant, or use \`from_degrees_minutes\`, which"
    echo "takes degrees and hundredths and cannot be misread. If a line here is a"
    echo "false positive, give its argument a named constant rather than relaxing"
    echo "this script."
    exit 1
fi

echo "==> every coordinate construction names its unit"
