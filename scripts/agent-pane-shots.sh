#!/usr/bin/env bash
# Development-only, offline agent pane review matrix. Requires a desktop/GPU session.
# Usage: bash scripts/agent-pane-shots.sh OUTPUT_DIRECTORY
# Set TR_DEMO_PANE_ONLY=1 to write Agent-pane crops instead of complete app frames.
set -euo pipefail

if [[ $# != 1 ]]; then
    printf 'Usage: bash scripts/agent-pane-shots.sh OUTPUT_DIRECTORY\n' >&2
    exit 2
fi
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$root"
mkdir -p -- "$1" "$root/target"
out=$(cd -- "$1" && pwd)
cargo build --offline -p terminal-rs --bin terminal-rs
binary="${CARGO_TARGET_DIR:-$root/target}/debug/terminal-rs"
tmp=$(mktemp -d "$root/target/agent-pane-shots.XXXXXX")
trap 'rm -rf -- "$tmp"' EXIT

states=(dormant missing unsupported signed-out signing-in checking empty-ready
        streaming answer markdown edits undone interrupted queue tools-unavailable
        rate-limited stopped-unsafe stopped-recoverable privacy)
for state in "${states[@]}"; do
    for theme in 'Terminal Dark' 'Terminal Light' 'Bloomberg'; do
        case "$theme" in
            'Terminal Dark') theme_slug=terminal-dark ;;
            'Terminal Light') theme_slug=terminal-light ;;
            Bloomberg) theme_slug=bloomberg ;;
        esac
        for case_name in 1600x960 1100x800 1100x800-narrow360; do
            case "$case_name" in
                1600x960) size=1600x960; width=660 ;;
                1100x800) size=1100x800; width=450 ;;
                1100x800-narrow360) size=1100x800; width=360 ;;
            esac
            png="$out/$state-$theme_slug-$case_name.png"
            TR_DATA_DIR="$tmp" TR_PROVIDER=sample TR_CONGRESS_REFRESH_MIN=0 \
                TR_AGENT_DEMO="$state" TR_THEME="$theme" TR_DEMO_SIZE="$size" \
                TR_DEMO_AGENT_WIDTH="$width" TR_DEMO_PANE_ONLY="${TR_DEMO_PANE_ONLY:-0}" \
                TR_DEMO_SHOT="$png" "$binary" >/dev/null
            printf '%s\n' "$png"
        done
    done
done
