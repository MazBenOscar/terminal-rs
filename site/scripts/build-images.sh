#!/usr/bin/env bash
# Regenerate the optimized images in static/ from the sources in assets-src/.
# Needs ImageMagick, cwebp and Google Chrome or Chromium (renders the SVG icon and the
# OG card). Set CHROME to the browser's path or command name if it isn't found.
set -euo pipefail
cd "$(dirname "$0")/.."

src=assets-src
out=static
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

if [[ -n ${CHROME:-} ]]; then
	candidates=("$CHROME")
else
	candidates=(google-chrome google-chrome-stable chromium chromium-browser
		"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
fi
chrome=
for candidate in "${candidates[@]}"; do
	if chrome=$(command -v "$candidate"); then
		break
	fi
done
if [[ -z $chrome ]]; then
	echo "build-images.sh: Chrome or Chromium not found; set CHROME to its path or command name." >&2
	exit 1
fi

render() { # render <url> <width> <height> <out.png>
	"$chrome" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
		--default-background-color=00000000 --window-size="$2,$3" --screenshot="$4" "$1" 2>/dev/null
}

# Screenshots: the 3200x1920 window captures behind the hero demo. They stay at full size because
# the demo zooms into the chart pane.
for theme in dark light; do
	cwebp -quiet -q 80 -m 6 -sharp_yuv -metadata none "$src/hero-$theme.png" -o "$out/demo-$theme.webp"
done

# Icon: the master keeps the macOS icon grid's padding; favicons crop to the tile;
# the touch icon fills the square because iOS applies its own mask.
sed 's/viewBox="0 0 1024 1024"/viewBox="100 100 824 824"/' "$src/icon.svg" >"$tmp/tight.svg"
sed -e 's/ rx="185"//g' -e '/stroke="url(#rim)"/d' "$tmp/tight.svg" >"$tmp/touch.svg"
render "file://$PWD/$src/icon.svg" 1024 1024 "$tmp/icon.png"
render "file://$tmp/tight.svg" 1024 1024 "$tmp/tight.png"
render "file://$tmp/touch.svg" 1024 1024 "$tmp/touch.png"
magick "$tmp/icon.png" -filter Lanczos -resize 256x256 -strip "$out/icon.png"
magick "$tmp/tight.png" -filter Lanczos -resize 64x64 -strip "$out/favicon.png"
magick "$tmp/touch.png" -filter Lanczos -resize 180x180 -background '#09090b' -flatten -strip "$out/apple-touch-icon.png"
sed 's/ width="1024" height="1024"//' "$tmp/tight.svg" >"$out/favicon.svg"

# Open Graph card, 1200x630.
cp "$src/og.html" "$src/icon.svg" "$src/hero-dark.png" "$tmp/"
render "file://$tmp/og.html" 1200 630 "$tmp/og.png"
magick "$tmp/og.png" -background '#0a0a0b' -flatten -strip -sampling-factor 4:4:4 -quality 86 "$out/og.jpg"

ls -l "$out" | awk 'NR>1 {printf "%8d  %s\n", $5, $9}'
