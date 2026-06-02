#!/usr/bin/env bash
#
# scripts/record-demo-gif.sh — capture docs/images/sentinel-demo.gif
#
# Drives a headless Chrome at the running console, takes 30 screenshots
# over ~30 seconds (one per second, cycling through console states), and
# assembles them into a GIF with ffmpeg's two-pass palette encode.
#
# The console at https://localhost:18001 must already be up. Start it via
#   ./scripts/demo.sh
# in another terminal first.
#
# Output: docs/images/sentinel-demo.gif (~1-2 MB)
#
# Knobs:
#   DASHBOARD_URL   override base URL (default https://localhost:18001)
#   FRAMES          override frame count (default 30)
#   CHROME          chrome binary (default: google-chrome)

set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

dashboard_url="${DASHBOARD_URL:-https://localhost:18001}"
frames="${FRAMES:-30}"
chrome="${CHROME:-google-chrome}"
work=$(mktemp -d -t sentinel-gif-XXXXXX)
out_dir="docs/images"
out_gif="$out_dir/sentinel-demo.gif"

cleanup() { rm -rf "$work"; }
trap cleanup EXIT

mkdir -p "$out_dir"

if ! curl -kfsS --max-time 5 "$dashboard_url/api/health" >/dev/null; then
    echo "Console not reachable at $dashboard_url — start ./scripts/demo.sh first" >&2
    exit 2
fi

echo "[gif] capturing $frames frames from $dashboard_url"

routes=(
    "$dashboard_url/"
    "$dashboard_url/"
)

for i in $(seq 1 "$frames"); do
    url="${routes[$(( (i-1) % ${#routes[@]} ))]}"
    frame=$(printf '%s/frame-%03d.png' "$work" "$i")

    "$chrome" --headless --disable-gpu --hide-scrollbars --no-sandbox --ignore-certificate-errors \
        --window-size=1280,800 --virtual-time-budget=1500 \
        --screenshot="$frame" "$url" >/dev/null 2>&1 || true

    if [ -f "$frame" ]; then
        printf '.'
    else
        printf 'x'
    fi
done
echo ""

shot_count=$(find "$work" -name 'frame-*.png' | wc -l)
if [ "$shot_count" -lt 5 ]; then
    echo "[gif] FAIL: only $shot_count frames captured" >&2
    exit 3
fi
echo "[gif] $shot_count/$frames frames captured, encoding GIF..."

palette="$work/palette.png"
ffmpeg -y -loglevel error -framerate 2 -i "$work/frame-%03d.png" \
    -vf "scale=960:-1:flags=lanczos,palettegen=stats_mode=diff" \
    "$palette"
ffmpeg -y -loglevel error -framerate 2 -i "$work/frame-%03d.png" -i "$palette" \
    -lavfi "scale=960:-1:flags=lanczos[x];[x][1:v]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle" \
    -loop 0 "$out_gif"

bytes=$(stat -c %s "$out_gif")
printf "[gif] wrote %s (%d bytes, %d frames)\n" "$out_gif" "$bytes" "$shot_count"
