#!/usr/bin/env bash
set -euo pipefail
launcher=$(realpath "$1")
work=$(mktemp -d)
server_pid=
app_pid=
cleanup() {
  for pid in "$app_pid" "$server_pid"; do
    if [[ -n "$pid" ]]; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  cat "$work"/*.log 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
# An isolated server, with no stream-restore state from previous launches.
export PULSE_SERVER="unix:$work/pulse.socket"
pulseaudio -n --daemonize=no --exit-idle-time=-1 \
  --load="module-native-protocol-unix socket=$work/pulse.socket auth-anonymous=1" \
  --load="module-null-sink sink_name=display" \
  --load="module-null-sink sink_name=speakers" >"$work/server.log" 2>&1 &
server_pid=$!
for attempt in {1..30}; do
  if pactl info >/dev/null 2>&1; then break; fi
  sleep 0.2
done
pactl info
export GDK_BACKEND=x11 GSETTINGS_BACKEND=memory LISTENMOE_APP_NON_UNIQUE=1
# Reopen each station on the chosen default, then change the default and repeat.
# Also switch stations using the real window's keyboard shortcut and require
# a newly created output stream on the same selected sink.
for target in speakers display; do
  pactl set-default-sink "$target"
  for station in jpop kpop; do
    "$launcher" --no-discord --autoplay "--$station" >"$work/$target-$station.log" 2>&1 &
    app_pid=$!
    found=false
    for attempt in {1..50}; do
      kill -0 "$app_pid"
      pactl -f json list sink-inputs >"$work/inputs.json"
      pactl -f json list sinks >"$work/sinks.json"
      if python3 - "$work" "$target" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
inputs = json.loads((root / "inputs.json").read_text())
sinks = json.loads((root / "sinks.json").read_text())
expected = next(s["index"] for s in sinks if s["name"] == sys.argv[2])
assert inputs and all(i["sink"] == expected for i in inputs)
PY
      then found=true; break; fi
      sleep 0.2
    done
    [[ "$found" == true ]] || { echo "Wrong output for $station / $target" >&2; exit 1; }
    echo "Audio routing passed: $station -> $target"
    cp "$work/inputs.json" "$work/previous.json"
    window=$(timeout 10s xdotool search --sync --onlyvisible --name 'Listen Moe' | head -n1)
    xdotool windowfocus --sync "$window"
    if [[ "$station" == jpop ]]; then shortcut=ctrl+k; else shortcut=ctrl+j; fi
    xdotool key --clearmodifiers "$shortcut"
    switched=false
    for attempt in {1..50}; do
      kill -0 "$app_pid"
      pactl -f json list sink-inputs >"$work/inputs.json"
      if python3 - "$work" "$target" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
inputs = json.loads((root / "inputs.json").read_text())
previous = {i["index"] for i in json.loads((root / "previous.json").read_text())}
sinks = json.loads((root / "sinks.json").read_text())
expected = next(s["index"] for s in sinks if s["name"] == sys.argv[2])
sys.exit(0 if inputs and all(i["sink"] == expected and i["index"] not in previous for i in inputs) else 1)
PY
      then switched=true; break; fi
      sleep 0.2
    done
    [[ "$switched" == true ]] || { echo "Station switch changed or lost output: $target" >&2; exit 1; }
    echo "Station switch passed: $station / $shortcut -> $target"
    kill "$app_pid"
    wait "$app_pid" || true
    app_pid=
    # Ensure the previous process really released its output stream.
    for attempt in {1..30}; do
      if [[ -z "$(pactl list short sink-inputs)" ]]; then break; fi
      sleep 0.2
    done
    [[ -z "$(pactl list short sink-inputs)" ]]
  done
done
