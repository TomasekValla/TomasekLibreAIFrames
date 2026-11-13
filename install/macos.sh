#!/bin/sh
# Installs TomasekLibreAIFrames on macOS 13.4+ with Apple Silicon into ~/Applications:
#   curl -fsSL https://raw.githubusercontent.com/TomasekValla/TomasekLibreAIFrames/main/install/macos.sh | sh
# TLAF_PRERELEASE=1 takes the newest pre-release instead of the newest stable release.
set -eu
REPO="TomasekValla/TomasekLibreAIFrames"
APP="TomasekLibreAIFrames"
name="$APP-macos-apple-silicon.zip"

[ "$(uname -m)" = "arm64" ] || { echo "Only Apple Silicon Macs are supported." >&2; exit 1; }

# The release feed, not the GitHub API: the API allows only 60 anonymous requests an hour.
# Newest first; a tag with "-" (v1.2.0-pre.3) is a pre-release.
tags=$(curl -fsSL "https://github.com/$REPO/releases.atom" | grep -o 'releases/tag/[^"<]*' | sed 's|releases/tag/||' | awk '!seen[$0]++')
# Stable releases first (all tags with TLAF_PRERELEASE=1). A tag whose release is still being built
# has no files yet, so take the first one whose checksum file is really there.
if [ "${TLAF_PRERELEASE:-0}" = "1" ]; then
    cands="$tags"
else
    cands=$( (printf '%s\n' "$tags" | grep -v -- '-'; printf '%s\n' "$tags" | grep -- '-') || true)
fi
tag=""
for t in $cands; do
    if curl -fsL -r 0-0 -o /dev/null "https://github.com/$REPO/releases/download/$t/$name.sha256"; then
        tag="$t"
        break
    fi
done
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
