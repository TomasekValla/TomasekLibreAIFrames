#!/bin/sh
# Installs TomasekLibreAIFrames on Linux for the current user (x86-64, glibc 2.38+):
#   curl -fsSL https://raw.githubusercontent.com/TomasekValla/TomasekLibreAIFrames/main/install/linux.sh | sh
# Build variant: WebGPU (Vulkan, any GPU) by default. Override with TLAF_VARIANT=cuda|rocm|cpu|webgpu.
# TLAF_PRERELEASE=1 takes the newest pre-release instead of the newest stable release.
set -eu
REPO="TomasekValla/TomasekLibreAIFrames"
APP="TomasekLibreAIFrames"

[ "$(uname -m)" = "x86_64" ] || { echo "Only x86-64 Linux is supported for now." >&2; exit 1; }
command -v curl >/dev/null || { echo "curl is needed." >&2; exit 1; }
command -v sha256sum >/dev/null || { echo "sha256sum is needed." >&2; exit 1; }

variant="${TLAF_VARIANT:-webgpu}"
case "$variant" in webgpu|cuda|rocm|cpu) ;; *) echo "Unknown TLAF_VARIANT: $variant" >&2; exit 1 ;; esac
name="$APP-linux-x64-$variant.tar.gz"

# Newest stable release (or newest of all with TLAF_PRERELEASE=1, or when there is no stable one yet).
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
echo "Downloading $APP $tag ($variant)..."
curl -fL --progress-bar -o "$tmp/$name" "$base/$name"
curl -fsSL -o "$tmp/$name.sha256" "$base/$name.sha256"
(cd "$tmp" && sha256sum -c "$name.sha256" >/dev/null) || { echo "Checksum mismatch, nothing was installed." >&2; exit 1; }
echo "Checksum verified."

dest="${XDG_DATA_HOME:-$HOME/.local/share}/$APP/app"
bin="$HOME/.local/bin"
mkdir -p "$dest" "$bin"
tar -xzf "$tmp/$name" -C "$tmp"
cp -rf "$tmp/$APP-linux-x64-$variant/." "$dest/"
chmod +x "$dest/$APP"
ln -sf "$dest/$APP" "$bin/$APP"

apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
mkdir -p "$apps"
cat > "$apps/$APP.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=$APP
Comment=AI frame interpolation and upscaling
Exec="$dest/$APP" %F
Terminal=false
Categories=AudioVideo;Video;
MimeType=video/mp4;video/x-matroska;video/quicktime;video/webm;video/x-msvideo;
EOF

echo "$APP $tag is installed in $dest."
echo "Start it from your app menu or run: $APP"
case ":$PATH:" in *":$bin:"*) ;; *) echo "Note: $bin is not on your PATH." ;; esac
