#!/usr/bin/env bash
# Regenerate the optimized images in static/ from the sources in assets-src/.
# Needs ImageMagick (with AVIF), cwebp and Google Chrome or Chromium (renders the SVG icon and the
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

# Screenshots: 3200x1920 window captures, served at 1080 and 2160 px wide, plus a crop of the
# watchlist and chart for narrow screens, where the whole window is too small to read.
for theme in dark light; do
	for w in 1080 2160; do
		magick "$src/hero-$theme.png" -strip -filter Lanczos -resize "${w}x" "$tmp/hero-$theme-$w.png"
	done
	magick "$src/hero-$theme.png" -strip -crop 1840x1000+0+0 +repage -filter Lanczos -resize 1080x \
		"$tmp/hero-$theme-crop.png"
	for name in "$theme-1080" "$theme-2160" "$theme-crop"; do
		cwebp -quiet -q 82 -m 6 -sharp_yuv "$tmp/hero-$name.png" -o "$out/hero-$name.webp"
		magick "$tmp/hero-$name.png" -quality 60 "$out/hero-$name.avif"
	done
done

# Icon: the master keeps the macOS icon grid's padding; favicons crop to the tile;
# the touch icon fills the square because iOS applies its own mask.
sed 's/viewBox="0 0 1024 1024"/viewBox="100 100 824 824"/' "$src/icon.svg" >"$tmp/tight.svg"
sed -e 's|<rect x="100" y="100" width="824" height="824" rx="185" fill="url(#bg)" />|<rect x="100" y="100" width="824" height="824" fill="url(#bg)" />|' \
	-e '/stroke="url(#rim)"/d' "$tmp/tight.svg" >"$tmp/touch.svg"
render "file://$PWD/$src/icon.svg" 1024 1024 "$tmp/icon.png"
render "file://$tmp/tight.svg" 1024 1024 "$tmp/tight.png"
render "file://$tmp/touch.svg" 1024 1024 "$tmp/touch.png"
magick "$tmp/icon.png" -filter Lanczos -resize 256x256 -strip "$out/icon.png"
magick "$tmp/tight.png" -filter Lanczos -resize 64x64 -strip "$out/favicon.png"
magick "$tmp/touch.png" -filter Lanczos -resize 180x180 -background '#100f0d' -flatten -strip "$out/apple-touch-icon.png"
sed 's/ width="1024" height="1024"//' "$tmp/tight.svg" >"$out/favicon.svg"

# Open Graph card, 1200x630.
cp "$src/og.html" "$src/icon.svg" "$src/hero-dark.png" "$tmp/"
render "file://$tmp/og.html" 1200 630 "$tmp/og.png"
magick "$tmp/og.png" -background '#0b0b0a' -flatten -strip -sampling-factor 4:4:4 -quality 86 "$out/og.jpg"

ls -l "$out" | awk 'NR>1 {printf "%8d  %s\n", $5, $9}'
