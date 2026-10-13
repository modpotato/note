#!/bin/sh
# Builds assets/icon/note.ico: a dark rounded square with a light lowercase "n" in
# Paper Mono Bold and an accent caret bar. Needs ImageMagick (convert). Every size is drawn
# from the same proportions, so the frames match. Re-run after changing the design.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
font="$here/../fonts/PaperMono-Bold.ttf"
out="$here/note.ico"

bg='#1b1b1f'      # tile
fg='#ececf0'      # the "n"
accent='#569cd6'  # caret bar
sizes='16 24 32 48 64 256'

command -v convert >/dev/null 2>&1 || { echo "make-icon: ImageMagick 'convert' not found" >&2; exit 1; }
[ -f "$font" ] || { echo "make-icon: missing font $font" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

frames=
for s in $sizes; do
  # Proportions of the canvas size s, rounded to whole pixels.
  radius=$(awk -v s="$s" 'BEGIN { printf "%d", s * 0.1875 + 0.5 }')
  pt=$(awk -v s="$s" 'BEGIN { printf "%d", s * 0.83 + 0.5 }')
  gap=$(awk -v s="$s" 'BEGIN { printf "%d", s * 0.07 + 0.5 }')
  caret=$(awk -v s="$s" 'BEGIN { c = int(s * 0.055 + 0.5); print (c < 1 ? 1 : c) }')

  # The "n", trimmed to its ink so its height is the caret's height.
  convert -background none -font "$font" -pointsize "$pt" -fill "$fg" \
    label:n -trim +repage "$tmp/n.png"
  gw=$(identify -format '%w' "$tmp/n.png")
  gh=$(identify -format '%h' "$tmp/n.png")

  # Centre the "n" and caret as one group.
  x0=$(awk -v s="$s" -v gw="$gw" -v gap="$gap" -v cw="$caret" 'BEGIN { printf "%d", (s - (gw + gap + cw)) / 2 }')
  y0=$(awk -v s="$s" -v gh="$gh" 'BEGIN { printf "%d", (s - gh) / 2 }')
  cx=$((x0 + gw + gap))

  convert -size "${s}x${s}" xc:none -fill "$bg" \
    -draw "roundrectangle 0,0 $((s - 1)),$((s - 1)) $radius,$radius" \
    PNG32:"$tmp/tile.png"
  convert "$tmp/tile.png" "$tmp/n.png" -geometry "+$x0+$y0" -composite PNG32:"$tmp/glyph.png"
  convert "$tmp/glyph.png" -fill "$accent" \
    -draw "rectangle $cx,$y0 $((cx + caret - 1)),$((y0 + gh - 1))" \
    PNG32:"$tmp/frame-$s.png"
  frames="$frames $tmp/frame-$s.png"
done

# shellcheck disable=SC2086 # the frame list is words we built ourselves
convert $frames "$out"
echo "wrote $out"
