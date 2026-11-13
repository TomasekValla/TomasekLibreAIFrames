#!/bin/sh
# Installs TomasekLibreAIFrames on macOS 13.4+ with Apple Silicon into ~/Applications:
#   curl -fsSL https://raw.githubusercontent.com/TomasekValla/TomasekLibreAIFrames/main/install/macos.sh | sh
# TLAF_PRERELEASE=1 takes the newest pre-release instead of the newest stable release.
set -eu
REPO="TomasekValla/TomasekLibreAIFrames"
APP="TomasekLibreAIFrames"
name="$APP-macos-apple-silicon.zip"

[ "$(uname -m)" = "arm64" ] || { echo "Only Apple Silicon Macs are supported." >&2; exit 1; }

api=$(curl -fsSL -H "User-Agent: $APP-installer" "https://api.github.com/repos/$REPO/releases?per_page=20")
tags=$(printf '%s' "$api" | tr ',' '\n' | grep -E '"(tag_name|prerelease|draft)"' | sed 's/[" ]//g')
pick() {
    printf '%s\n' "$tags" | awk -F: -v want="$1" '
        /^tag_name:/ { t = $2 } /^draft:/ { d = $2 }
        /^prerelease:/ { if (d != "true" && (want == "any" || $2 == "false")) { print t; exit } }'
}
tag=""
[ "${TLAF_PRERELEASE:-0}" = "1" ] || tag=$(pick stable)
[ -n "$tag" ] || tag=$(pick any)
[ -n "$tag" ] || { echo "No release found." >&2; exit 1; }

base="https://github.com/$REPO/releases/download/$tag"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "Downloading $APP $tag..."
curl -fL --progress-bar -o "$tmp/$name" "$base/$name"
curl -fsSL -o "$tmp/$name.sha256" "$base/$name.sha256"
(cd "$tmp" && shasum -a 256 -c "$name.sha256" >/dev/null) || { echo "Checksum mismatch, nothing was installed." >&2; exit 1; }
echo "Checksum verified."

mkdir -p "$HOME/Applications"
(cd "$tmp" && ditto -x -k "$name" unpacked)
rm -rf "$HOME/Applications/$APP.app"
ditto "$tmp/unpacked/$APP.app" "$HOME/Applications/$APP.app"
# Downloaded with curl, so no quarantine flag is set; clear it anyway in case.
xattr -dr com.apple.quarantine "$HOME/Applications/$APP.app" 2>/dev/null || true

echo "$APP $tag is installed in ~/Applications."
open "$HOME/Applications/$APP.app"
