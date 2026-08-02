#!/usr/bin/env bash
# Per-user Linux install. No package manager and no root access required.
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
binary=${1:-"$repo/src-tauri/target/release/sayit"}
bin_dir=${HOME:?}/.local/bin
data_root=${XDG_DATA_HOME:-"$HOME/.local/share"}
data_dir="$data_root/dev.khalid.sayit"
app_dir="$data_root/applications"
icon_dir="$data_root/icons/hicolor/128x128/apps"

if [[ ! -x "$binary" ]]; then
  printf 'sayit: release binary not found: %s\n' "$binary" >&2
  printf 'Build it first, or pass the downloaded sayit-linux-x86_64 path.\n' >&2
  exit 1
fi

mkdir -p "$bin_dir" "$data_dir" "$app_dir" "$icon_dir"

# Same-directory rename makes installation and later self-updates atomic.
staged=$(mktemp "$bin_dir/.sayit.install.XXXXXX")
trap 'rm -f -- "${staged:-}" "${engine_staged:-}"' EXIT
cp -- "$binary" "$staged"
chmod 755 "$staged"
mv -f -- "$staged" "$bin_dir/sayit"
staged=

# A source checkout may carry the machine-specific CUDA companion. Released
# binaries fetch a signed companion on first run instead.
local_engine="$repo/sidecar/whisper-cuda/whisper-server"
if [[ -x "$local_engine" ]]; then
  mkdir -p "$data_dir/engine"
  engine_staged=$(mktemp "$data_dir/engine/.whisper-server.install.XXXXXX")
  cp --reflink=auto -- "$local_engine" "$engine_staged"
  chmod 755 "$engine_staged"
  mv -f -- "$engine_staged" "$data_dir/engine/whisper-server"
  engine_staged=
  printf 'cuda-12.4-sm86\n' >"$data_dir/engine/sayit-backend"
fi

# Development checkouts can seed assets already downloaded for testing.
if [[ ! -f "$data_dir/models/ggml-small.bin" && -f "$repo/models/ggml-small.bin" ]]; then
  mkdir -p "$data_dir/models"
  cp --reflink=auto -- "$repo/models/ggml-small.bin" "$data_dir/models/ggml-small.bin"
fi
if [[ -d "$repo/soundpack" && ! -d "$data_dir/soundpack" ]]; then
  cp -a -- "$repo/soundpack" "$data_dir/soundpack"
fi

cp -- "$repo/src-tauri/icons/128x128.png" "$icon_dir/dev.khalid.sayit.png"
cat >"$app_dir/dev.khalid.sayit.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=sayit
Comment=The key that listens
Exec=$bin_dir/sayit
TryExec=$bin_dir/sayit
Icon=dev.khalid.sayit
Terminal=false
StartupNotify=false
Categories=Utility;AudioVideo;Accessibility;
EOF
chmod 644 "$app_dir/dev.khalid.sayit.desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$app_dir" >/dev/null 2>&1 || true

printf 'Installed %s\n' "$bin_dir/sayit"
printf 'Data      %s\n' "$data_dir"

if [[ ${SAYIT_NO_LAUNCH:-0} != 1 ]]; then
  nohup "$bin_dir/sayit" >/dev/null 2>&1 &
  disown || true
  printf 'Launched sayit. Complete the one-time setup window.\n'
fi
