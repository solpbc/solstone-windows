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

for entry in $TRAY_MARKS; do
    name=${entry%%:*}
    stem=${entry#*:}
    src="$BRAND_DIR/$stem.svg"
    if [ ! -f "$src" ]; then
        echo "brand: missing source $src" >&2
        exit 1
    fi
    frames=""
    for size in $FRAME_SIZES; do
        frame="$work/$name-$size.png"
        rsvg-convert -w "$size" -h "$size" "$src" -o "$frame"
        frames="$frames $frame"
    done
    # shellcheck disable=SC2086
    icotool -c -o "$OUT_DIR/$name.ico" $frames
    echo "  tray: $name.ico  <- $stem.svg  ($FRAME_SIZES)"
done
