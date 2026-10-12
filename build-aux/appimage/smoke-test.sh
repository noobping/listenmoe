#!/usr/bin/env bash
set -euo pipefail
image=$(realpath "$1")
work=$(mktemp -d)
app_pid=
cleanup() {
  if [[ -n "$app_pid" ]]; then
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
  fi
  cat "$work/app.log" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
cd "$work"
echo 'Extracting AppImage'
timeout 60s "$image" --appimage-extract >/dev/null
echo 'Checking command-line startup'
timeout 30s ./squashfs-root/AppRun --version
timeout 30s ./squashfs-root/AppRun --help

# Exercise the actual bundled Rodio/ALSA playback path against a desktop sound
# server. No network or physical sound card is needed to open its output stream.
bash /project/build-aux/appimage/test-audio.sh "$work/squashfs-root/AppRun"

# A real window must remain alive, even with no network and no audio device.
export GDK_BACKEND=x11 GSETTINGS_BACKEND=memory
export LISTENMOE_APP_NON_UNIQUE=1
./squashfs-root/AppRun --no-discord --verbose >app.log 2>&1 &
app_pid=$!
for attempt in {1..30}; do
  if ! kill -0 "$app_pid" 2>/dev/null; then
    cat app.log
    echo 'App exited before displaying a window' >&2
    exit 1
  fi
  if timeout 5s xwininfo -root -tree | grep -qi 'listen'; then
    sleep 12
    kill -0 "$app_pid" || { cat app.log; exit 1; }
    cat app.log
    if grep -Ei 'Failed to load icon|Unrecognized image file format' app.log; then
      echo 'Bundled image loading failed' >&2
      exit 1
    fi
    echo 'AppImage window smoke test passed'
    exit 0
  fi
  sleep 1
done
cat app.log
echo 'No LISTEN.moe window appeared' >&2
exit 1
