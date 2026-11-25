#!/usr/bin/env bash
# Stop karukan-skkserv and remove the service installed by install-service.sh.
# The binary, dictionary, and models are left in place.
set -euo pipefail

NAME=karukan-skkserv
LABEL=page.caffeine.karukan-skkserv

uninstall_launchd() {
	local plist=$HOME/Library/LaunchAgents/$LABEL.plist
	launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
	if [ -e "$plist" ]; then
		rm -f "$plist"
		echo "Removed $plist"
	else
		echo "Not installed: $plist"
	fi
	echo "The log is kept: $HOME/Library/Logs/$NAME.log"
}

uninstall_systemd() {
	local unit=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$NAME.service
	systemctl --user disable --now "$NAME.service" 2>/dev/null || true
	if [ -e "$unit" ]; then
		rm -f "$unit"
		echo "Removed $unit"
	else
		echo "Not installed: $unit"
	fi
	systemctl --user daemon-reload
	systemctl --user reset-failed "$NAME.service" 2>/dev/null || true
}

case $(uname -s) in
Darwin) uninstall_launchd ;;
Linux) uninstall_systemd ;;
*)
	echo "error: unsupported OS: $(uname -s)" >&2
	exit 1
	;;
esac
