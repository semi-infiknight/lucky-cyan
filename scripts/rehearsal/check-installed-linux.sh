#!/bin/sh
# Run the installed Debian desktop on its own X11 display and PipeWire graph.
# Usage: scripts/rehearsal/check-installed-linux.sh NEW_EVIDENCE_DIRECTORY [SECONDS]
set -eu
umask 077
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "Usage: $0 NEW_EVIDENCE_DIRECTORY [SECONDS]" >&2
  exit 2
fi
script_dir=$(cd "$(dirname "$0")" && pwd)
evidence=$(realpath -m "$1")
seconds=${2:-60}
case "$seconds" in ''|*[!0-9]*) echo "Duration must be whole seconds" >&2; exit 2;; esac
# Each run gets fresh, independently reviewable evidence.
mkdir "$evidence"
scratch=$(mktemp -d)
export XDG_RUNTIME_DIR="$scratch" XDG_DATA_HOME="$scratch/data" XDG_CONFIG_HOME="$scratch/config"
export XDG_CACHE_HOME="$scratch/cache" XDG_STATE_HOME="$scratch/state"
unset PIPEWIRE_RUNTIME_DIR PIPEWIRE_REMOTE
export INKUE_REHEARSAL_PRIVATE_RUNTIME="$scratch"
export LIBGL_ALWAYS_SOFTWARE=1 GDK_BACKEND=x11
trap 'rm -rf -- "$scratch"' EXIT
graph_quantum=${INKUE_REHEARSAL_QUANTUM:-}
case "$graph_quantum" in ''|64|128|256|512|1024|2048) ;; *) echo "Unsupported rehearsal quantum" >&2; exit 2;; esac
app_buffer=${INKUE_REHEARSAL_APP_BUFFER:-}
case "$app_buffer" in ''|64|128|256|512|1024|2048) ;; *) echo "Unsupported rehearsal audio buffer" >&2; exit 2;; esac
export evidence seconds script_dir graph_quantum app_buffer
dbus-run-session -- sh -eu <<'SESSION'
children=''
cleanup() {
  for pid in $children; do kill "$pid" 2>/dev/null || true; done
  wait || true
  if test -r /sys/fs/cgroup/cpu.stat; then cat /sys/fs/cgroup/cpu.stat > "$evidence/cpu-end.txt"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$XDG_CONFIG_HOME/Inkue" "$XDG_DATA_HOME"
binary=$(command -v inkue)
show=$(dpkg-query -L inkue | awk '/\/rehearsal\/Operator rehearsal\.inkue$/ {print;exit}')
test -f "$show"
desktop=$(dpkg-query -L inkue | awk '/\/applications\/[^/]+\.desktop$/ {print;exit}')
test -f "$desktop"
grep -E '^(Exec|MimeType)=' "$desktop" >"$evidence/association.txt"
grep -Eq '^Exec=.*%[fFuU]' "$desktop"
grep -Fq 'application/x-inkue-workspace' "$desktop"
test -f /usr/share/mime/packages/inkue-workspace.xml
gio info -a standard::content-type "$show" >>"$evidence/association.txt"
grep -Fq 'standard::content-type: application/x-inkue-workspace' "$evidence/association.txt"
{
  cat /usr/share/doc/inkue/desk-build.json
  dpkg-query -W -f='Package ${Package} ${Version} ${Architecture}\n' inkue
  sha256sum "$binary" "$show" "$(dirname "$show")/standby.wav"
  uname -srmo
  date -u
} > "$evidence/installation.txt"
port=$(python3 -c 'import socket; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
printf '{"enabled":true,"port":%s,"allowed_ips":["127.0.0.1"]}\n' "$port" >"$XDG_CONFIG_HOME/Inkue/osc.json"
if test -n "$app_buffer"; then
  printf '{"buffer_size":%s}\n' "$app_buffer" > "$XDG_CONFIG_HOME/Inkue/audio.json"
  cp "$XDG_CONFIG_HOME/Inkue/audio.json" "$evidence/audio-config.json"
fi
if test -r /sys/fs/cgroup/cpu.stat; then cat /sys/fs/cgroup/cpu.stat > "$evidence/cpu-start.txt"; fi
Xvfb -displayfd 1 -screen 0 1280x800x24 -nolisten tcp >"$XDG_RUNTIME_DIR/display" 2>"$evidence/display.log" &
children="$! $children"
pipewire >"$evidence/pipewire.log" 2>&1 &
children="$! $children"
for i in $(seq 1 100); do
  if test -s "$XDG_RUNTIME_DIR/display" && pw-cli info 0 >/dev/null 2>&1; then break; fi
  sleep .1
done
test -s "$XDG_RUNTIME_DIR/display"
pw-cli info 0 >/dev/null
if test -n "$graph_quantum"; then
  pw-metadata -n settings 0 clock.force-rate 48000 >"$evidence/graph-clock.txt"
  pw-metadata -n settings 0 clock.force-quantum "$graph_quantum" >>"$evidence/graph-clock.txt"
fi
wireplumber >"$evidence/wireplumber.log" 2>&1 &
children="$! $children"
DISPLAY=:$(cat "$XDG_RUNTIME_DIR/display")
export DISPLAY
pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=rehearsal media.class=Audio/Sink object.linger=true audio.position=[ FL FR ] }'
# The policy manager must expose the new sink before the desk opens its output.
for i in $(seq 1 100); do
  if wpctl inspect @DEFAULT_AUDIO_SINK@ 2>/dev/null | grep -q 'node.name = "rehearsal"'; then break; fi
  sleep .1
done
wpctl inspect @DEFAULT_AUDIO_SINK@ >"$evidence/sink.txt"
grep -q 'node.name = "rehearsal"' "$evidence/sink.txt"
wpctl set-volume @DEFAULT_AUDIO_SINK@ 1
wpctl set-mute @DEFAULT_AUDIO_SINK@ 0
wpctl get-volume @DEFAULT_AUDIO_SINK@ >>"$evidence/sink.txt"
if test "${INKUE_REHEARSAL_TRACE:-0}" = 1; then
  stdbuf -oL pw-top -b > "$evidence/graph.log" 2>&1 &
  children="$! $children"
fi
"$binary" "$show" >"$evidence/desk.log" 2>&1 &
children="$! $children"
for i in $(seq 1 300); do
  if grep -Fq "Opened workspace: $show" "$evidence/desk.log"; then break; fi
  sleep .1
done
grep -Fq "Opened workspace: $show" "$evidence/desk.log"
# The desktop should forward a second file-open request to the existing desk.
timeout 8 gio launch "$desktop" "$show" >>"$evidence/second-launch.log" 2>&1
for i in $(seq 1 80); do
  if test "$(grep -Fc "Opened workspace: $show" "$evidence/desk.log")" -ge 2; then break; fi
  sleep .1
done
test "$(grep -Fc "Opened workspace: $show" "$evidence/desk.log")" -ge 2
# Let the desk adopt the newly opened workspace before sending numbered OSC GO.
sleep 2
if test "${INKUE_REHEARSAL_SCREENSHOT:-0}" = 1; then
  main=$(xdotool search --onlyvisible --name '^Inkue$' | head -1)
  xdotool windowfocus --sync "$main" key F5
  sleep .5
  xwd -silent -id "$main" -out "$evidence/show-mode.xwd"
  xdotool key F5
fi
python3 "$script_dir/linux_soak.py" --osc-port "$port" --device-loss-log "$evidence/desk.log" \
  --failure-evidence "$evidence" >"$evidence/device-loss.json" 2>"$evidence/device-loss.log"
python3 "$script_dir/linux_soak.py" --osc-port "$port" --seconds "$seconds" \
  --failure-evidence "$evidence" >"$evidence/cycles.jsonl" 2>"$evidence/check.log"
date -u >"$evidence/completed.txt"
SESSION
