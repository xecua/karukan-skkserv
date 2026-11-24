#!/usr/bin/env bash
# Register karukan-skkserv as a per-user service that starts automatically:
# a launchd LaunchAgent on macOS, a systemd user unit on Linux.
#
# Usage: scripts/install-service.sh [--bin PATH] [-- SERVER_ARGS...]
#
# Arguments after `--` are passed to karukan-skkserv as they are, for example:
#   scripts/install-service.sh -- --port 11780 --dict ~/dict.bin
set -euo pipefail

NAME=karukan-skkserv
LABEL=io.github.xecua.karukan-skkserv
REPO_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

usage() {
	sed -n '2,8s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"
}

die() {
	echo "error: $*" >&2
	exit 1
}

bin=
while [ $# -gt 0 ]; do
	case $1 in
	--bin)
		[ $# -ge 2 ] || die "--bin needs a path"
		bin=$2
		shift 2
		;;
	--bin=*)
		bin=${1#--bin=}
		shift
		;;
	-h | --help)
		usage
		exit 0
		;;
	--)
		shift
		break
		;;
	*)
		die "unknown option: $1 (put server options after --)"
		;;
	esac
done

# Find the binary: --bin, then PATH (e.g. `cargo install --path .`), then
# the release build in this repository.
if [ -z "$bin" ]; then
	if command -v "$NAME" >/dev/null 2>&1; then
		bin=$(command -v "$NAME")
	elif [ -x "$REPO_DIR/target/release/$NAME" ]; then
		bin=$REPO_DIR/target/release/$NAME
	else
		die "$NAME not found; run \`cargo build --release\` or pass --bin PATH"
	fi
fi
[ -x "$bin" ] || die "not executable: $bin"
bin_dir=$(cd "$(dirname "$bin")" && pwd)
bin=$bin_dir/$(basename "$bin")

# Replace every occurrence of a placeholder in stdin with an arbitrary string.
# awk reads the value from the environment, so no character needs escaping.
replace() {
	PLACEHOLDER=$1 VALUE=$2 awk '
		{
			out = ""
			while ((i = index($0, ENVIRON["PLACEHOLDER"])) > 0) {
				out = out substr($0, 1, i - 1) ENVIRON["VALUE"]
				$0 = substr($0, i + length(ENVIRON["PLACEHOLDER"]))
			}
			print out $0
		}'
}

# sed instead of ${s//...}: bash 5.2 treats `&` in the replacement specially.
xml_escape() {
	printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'
}

# Quote one word for systemd's ExecStart. `%` and `$` would otherwise be
# expanded as specifiers and environment variables.
systemd_quote() {
	local s=$1
	s=${s//\\/\\\\}
	s=${s//\"/\\\"}
	s=${s//%/%%}
	s=${s//\$/\$\$}
	printf '"%s"' "$s"
}

install_launchd() {
	local agents_dir=$HOME/Library/LaunchAgents
	local plist=$agents_dir/$LABEL.plist
	local log_file=$HOME/Library/Logs/$NAME.log
	local domain
	domain=gui/$(id -u)

	local program_args arg
	program_args=$(printf '\t\t<string>%s</string>' "$(xml_escape "$bin")")
	for arg in "$@"; do
		program_args+=$'\n'$(printf '\t\t<string>%s</string>' "$(xml_escape "$arg")")
	done

	mkdir -p "$agents_dir" "$(dirname "$log_file")"
	# Stop the running instance before overwriting its plist.
	launchctl bootout "$domain/$LABEL" 2>/dev/null || true

	replace @PROGRAM_ARGUMENTS@ "$program_args" \
		<"$REPO_DIR/contrib/launchd/$LABEL.plist" |
		replace @LOG_FILE@ "$(xml_escape "$log_file")" >"$plist"
	plutil -lint "$plist" >/dev/null

	launchctl bootstrap "$domain" "$plist"
	echo "Installed $plist"
	echo "Log: $log_file"
	echo "Status: launchctl print $domain/$LABEL"
}

install_systemd() {
	command -v systemctl >/dev/null 2>&1 || die "systemctl not found"
	local unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
	local unit=$unit_dir/$NAME.service

	local exec_start arg
	exec_start=$(systemd_quote "$bin")
	for arg in "$@"; do
		exec_start+=" $(systemd_quote "$arg")"
	done

	mkdir -p "$unit_dir"
	replace @EXEC_START@ "$exec_start" \
		<"$REPO_DIR/contrib/systemd/$NAME.service" >"$unit"

	systemctl --user daemon-reload
	systemctl --user enable "$NAME.service"
	# restart (not start) so that a reinstall picks up the new unit.
	systemctl --user restart "$NAME.service"
	echo "Installed $unit"
	echo "Log: journalctl --user -u $NAME"
	echo "Status: systemctl --user status $NAME"

	if command -v loginctl >/dev/null 2>&1 &&
		[ "$(loginctl show-user "$USER" --property=Linger --value 2>/dev/null)" != yes ]; then
		echo
		echo "The service starts when you log in. To start it at boot without"
		echo "logging in, run: loginctl enable-linger $USER"
	fi
}

case $(uname -s) in
Darwin) install_launchd "$@" ;;
Linux) install_systemd "$@" ;;
*) die "unsupported OS: $(uname -s)" ;;
esac
