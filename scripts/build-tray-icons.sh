#!/bin/sh
# Build src-tauri/icons/tray/*.ico from the brand status marks.
#
# Usage:  BRAND_DIR=/path/to/brand sh scripts/build-tray-icons.sh
#
# Each tray ICO carries the six frame sizes Windows picks from for the
# notification area across DPI scalings: 16, 20, 24, 32, 40, 48. Every frame is
# rendered straight from the vector at its exact pixel size, never downsampled
# from a single larger raster — the tray draws at 16px on a 100% display, and
# thin rays turn to mud if they are resampled rather than rendered.
#
# Output is deterministic: the same brand source and the same tool versions
# produce byte-identical ICOs, so re-running this on an unchanged brand source
# leaves the tree unchanged.
#
# Requires:
#   rsvg-convert (librsvg)   apt: librsvg2-bin   brew: librsvg
#   icotool      (icoutils)  apt: icoutils       brew: icoutils

set -eu

FRAME_SIZES="16 20 24 32 40 48"

# tray icon name : brand source stem
TRAY_MARKS="healthy:mark connecting:mark-connecting paused:mark-paused offline:mark-offline error:mark-error"

# Each of the five tray visuals has its own brand mark.

# The attention dot: each mark also gets a "-dot" twin, shown while the tray
# menu names an update. The dot sits in the corner in the mark's orange, on a
# cut-out gap that parts it from the mark, with a thin dark rim that keeps it
# visible on any taskbar colour (an ICO cannot follow the taskbar's theme).
# Units are the mark's own viewBox (2.5 2.5 27 27).
DOT_CX=24.75
DOT_CY=24.75
DOT_R=4.15
DOT_GAP_R=5.75
DOT_RIM=1.2
DOT_FILL="#E8913A"
DOT_RIM_INK="#1F1F1F"

OUT_DIR="${OUT_DIR:-src-tauri/icons/tray}"

if [ -z "${BRAND_DIR:-}" ]; then
    echo "brand: BRAND_DIR is required — point it at your brand asset directory" >&2
    exit 1
fi
if [ ! -d "$BRAND_DIR" ]; then
    echo "brand: BRAND_DIR=$BRAND_DIR not found" >&2
    exit 1
fi
for tool in rsvg-convert icotool; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "brand: $tool not found — install librsvg2-bin and icoutils (apt), or librsvg and icoutils (brew)" >&2
        exit 1
    fi
done

mkdir -p "$OUT_DIR"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

# The mark with the attention dot. The mark's first line opens the svg and its
# last line closes it; everything between is drawn under the gap's mask.
dotted() {
    head -n1 "$1"
    printf '  <mask id="dot-gap" maskUnits="userSpaceOnUse" x="0" y="0" width="32" height="32"><rect width="32" height="32" fill="#fff"/><circle cx="%s" cy="%s" r="%s" fill="#000"/></mask>\n' \
        "$DOT_CX" "$DOT_CY" "$DOT_GAP_R"
    printf '  <g mask="url(#dot-gap)">\n'
    sed '1d;$d' "$1"
    printf '  </g>\n  <circle fill="%s" stroke="%s" stroke-width="%s" cx="%s" cy="%s" r="%s"/>\n</svg>\n' \
        "$DOT_FILL" "$DOT_RIM_INK" "$DOT_RIM" "$DOT_CX" "$DOT_CY" "$DOT_R"
}

pack() {
    frames=""
    for size in $FRAME_SIZES; do
        frame="$work/$1-$size.png"
        rsvg-convert -w "$size" -h "$size" "$2" -o "$frame"
        frames="$frames $frame"
    done
    # shellcheck disable=SC2086
    icotool -c -o "$OUT_DIR/$1.ico" $frames
}

for entry in $TRAY_MARKS; do
    name=${entry%%:*}
    stem=${entry#*:}
    src="$BRAND_DIR/$stem.svg"
    if [ ! -f "$src" ]; then
        echo "brand: missing source $src" >&2
        exit 1
    fi
    pack "$name" "$src"
    echo "  tray: $name.ico  <- $stem.svg  ($FRAME_SIZES)"
    dotted "$src" > "$work/$name-dot.svg"
    pack "$name-dot" "$work/$name-dot.svg"
    echo "  tray: $name-dot.ico  <- $stem.svg + dot  ($FRAME_SIZES)"
done
