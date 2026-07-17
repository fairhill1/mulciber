#!/bin/sh
# Drives one comparison implementation through the standard KWin resize storm.
#
# Usage: run-resize-storm.sh "<window-caption-substring>" <log-file> -- <command> [args...]
#
# Starts the target command, waits for its window, loads the storm script against it, and waits
# for the command to exit (the storm closes the window). Requires a KDE Plasma Wayland session
# with qdbus6. The presented-frame and configure counts are read from the log afterwards.

set -eu

needle=$1
log=$2
shift 2
[ "$1" = "--" ] && shift

script_dir=$(dirname "$0")
generated=$(mktemp --suffix=.js)
sed "s/%NEEDLE%/$needle/g" "$script_dir/resize-storm.js" > "$generated"

"$@" > "$log" 2>&1 &
app_pid=$!
sleep 2

script_id=$(qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.loadScript "$generated" comparison-storm)
qdbus6 org.kde.KWin "/Scripting/Script$script_id" org.kde.kwin.Script.run 2>/dev/null \
    || qdbus6 org.kde.KWin "/$script_id" org.kde.kwin.Script.run

wait "$app_pid"
status=$?
qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.unloadScript comparison-storm > /dev/null 2>&1 || true
rm -f "$generated"

echo "exit status: $status"
echo "configures:  $(grep -c 'configured at' "$log" || true)"
echo "presented:   $(grep 'presented' "$log" | tail -1)"
exit "$status"
