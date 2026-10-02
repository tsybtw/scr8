#!/usr/bin/env bash
# Runs scr8's self-test on a macOS CI machine: one screenshot via a bind,
# the settings window and the area picker, captured into $2 for review.
set -uo pipefail
APP="$1"
OUT="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
DATA="$(mktemp -d)"
SHOTS="$DATA/shots"
mkdir -p "$SHOTS" "$OUT"
cat > "$DATA/config.json" <<JSON
{"binds":[{"id":1,"name":"CI","hotkey":{"ctrl":true,"alt":true,"shift":false,"meta":false,"key":"F9"},"region":{"x":0,"y":0,"w":600,"h":30},"folder":"$SHOTS","enabled":true}],"png_level":"Fast","autostart":false,"keep_settings_open":true,"next_id":1}
JSON

# Screen Recording permission for scr8 (CI machines run with SIP off).
sw_vers > "$OUT/system.txt"
for db in "/Library/Application Support/com.apple.TCC/TCC.db" "$HOME/Library/Application Support/com.apple.TCC/TCC.db"; do
  echo "== $db" >> "$OUT/tcc.txt"
  sudo sqlite3 "$db" ".schema access" >> "$OUT/tcc.txt" 2>&1
  sudo sqlite3 "$db" "INSERT OR REPLACE INTO access (service, client, client_type, auth_value, auth_reason, auth_version, flags, last_modified) VALUES ('kTCCServiceScreenCapture','com.tsybtw.scr8',0,2,4,1,0,$(date +%s));" >> "$OUT/tcc.txt" 2>&1
done

# macOS 15+ also asks every direct screen-capture app to confirm it may
# "bypass the system private window picker" (users click Allow once).
# Pre-approve it here; the dialog would cover the screen being checked.
EXE="$(cd "$APP" && pwd)/Contents/MacOS/scr8"
defaults write "$HOME/Library/Group Containers/group.com.apple.replayd/ScreenCaptureApprovals" "$EXE" -date "3000-01-01 00:00:00 +0000" >> "$OUT/tcc.txt" 2>&1
defaults read "$HOME/Library/Group Containers/group.com.apple.replayd/ScreenCaptureApprovals" >> "$OUT/tcc.txt" 2>&1
killall replayd >> "$OUT/tcc.txt" 2>&1 || true

# Launched through `open` so macOS treats it as its own app (not as part of
# this shell) when checking permissions.
open -n --env SCR8_DATA_DIR="$DATA" --env SCR8_SELFTEST="$OUT" "$APP" --args --hidden
for i in $(seq 1 120); do
  [ -f "$OUT/report.json" ] && break
  # Memory of the scr8 processes a few seconds in (RSS in KB).
  [ "$i" = 6 ] && ps -axo pid,rss,command | grep -i "[s]cr8" > "$OUT/memory.txt"
  sleep 1
done
sleep 2
cp -R "$SHOTS" "$OUT/shots"
ps aux | grep -i "[s]cr8" > "$OUT/processes.txt"
pkill -f "scr8.app" || true
echo "report:"
cat "$OUT/report.json" 2>/dev/null || echo "(none)"
