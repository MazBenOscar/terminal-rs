#!/usr/bin/env bash
# Regenerate the app icon from the site's logo (site/assets-src/icon.svg): app/assets/AppIcon.icns
# for the macOS bundle and app/assets/icon.png, which the app sets as its Dock icon at runtime.
# Needs macOS (sips, iconutil) and Google Chrome or Chromium to render the SVG; set CHROME to the
# browser's path or command name if it isn't found.
set -euo pipefail
cd "$(dirname "$0")/.."

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
	echo "build-icon.sh: Chrome or Chromium not found; set CHROME to its path or command name." >&2
	exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
"$chrome" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
	--default-background-color=00000000 --window-size=1024,1024 --screenshot="$tmp/icon.png" \
	"file://$PWD/site/assets-src/icon.svg" 2>/dev/null

set=$tmp/AppIcon.iconset
mkdir "$set"
for size in 16 32 128 256 512; do
	sips -z $size $size "$tmp/icon.png" --out "$set/icon_${size}x${size}.png" >/dev/null
	sips -z $((size * 2)) $((size * 2)) "$tmp/icon.png" --out "$set/icon_${size}x${size}@2x.png" >/dev/null
done
mkdir -p app/assets
iconutil -c icns "$set" -o app/assets/AppIcon.icns
cp "$set/icon_256x256@2x.png" app/assets/icon.png
echo "wrote app/assets/AppIcon.icns and app/assets/icon.png"
