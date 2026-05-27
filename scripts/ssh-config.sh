#!/usr/bin/env bash
#
# ssh-config.sh — manage a bounded, sentinel-delimited block in
# ~/.ssh/config for the dockerized Helmor remote host, so the desktop's
# SSH transport (which resolves hosts through ~/.ssh/config, BatchMode=yes)
# can reach the container exactly as it would a real remote.
#
# Mirrors the block-management the helmor repo's
# `src-tauri/tests/remote_docker_e2e.rs` uses, but for interactive /
# recorded runs rather than `cargo test`.
#
#   ssh-config.sh add <alias> <port> <identity_file>   # idempotent
#   ssh-config.sh remove <alias>
#
# The block is delimited by sentinels so removal never touches the user's
# own config. The host pins the dockerized key, disables host-key
# checking (ephemeral container key), and forces BatchMode (no prompts).

set -euo pipefail

CONFIG="${HOME}/.ssh/config"
BEGIN_FMT="# >>> helmor-taper (%s) >>>"
END_FMT="# <<< helmor-taper (%s) <<<"

ensure_config() {
	mkdir -p "${HOME}/.ssh"
	chmod 700 "${HOME}/.ssh"
	touch "${CONFIG}"
	chmod 600 "${CONFIG}"
}

remove_block() {
	local alias="$1"
	[ -f "${CONFIG}" ] || return 0
	local begin end
	begin="$(printf "${BEGIN_FMT}" "${alias}")"
	end="$(printf "${END_FMT}" "${alias}")"
	# Delete the inclusive block if present.
	awk -v b="${begin}" -v e="${end}" '
		$0 == b {skip=1}
		skip==0 {print}
		$0 == e {skip=0}
	' "${CONFIG}" > "${CONFIG}.tmp" && mv "${CONFIG}.tmp" "${CONFIG}"
}

cmd_add() {
	local alias="$1" port="$2" identity="$3"
	ensure_config
	remove_block "${alias}"
	local begin end
	begin="$(printf "${BEGIN_FMT}" "${alias}")"
	end="$(printf "${END_FMT}" "${alias}")"
	{
		printf '%s\n' "${begin}"
		printf 'Host %s\n' "${alias}"
		printf '    HostName 127.0.0.1\n'
		printf '    Port %s\n' "${port}"
		printf '    User e2e\n'
		printf '    IdentityFile %s\n' "${identity}"
		printf '    IdentitiesOnly yes\n'
		printf '    StrictHostKeyChecking no\n'
		printf '    UserKnownHostsFile /dev/null\n'
		printf '    BatchMode yes\n'
		printf '    LogLevel ERROR\n'
		printf '%s\n' "${end}"
	} >> "${CONFIG}"
	echo "ssh-config: wrote Host ${alias} (127.0.0.1:${port}, ${identity})"
}

cmd_remove() {
	remove_block "$1"
	echo "ssh-config: removed Host $1 block (if present)"
}

case "${1:-}" in
	add) shift; cmd_add "$@" ;;
	remove) shift; cmd_remove "$@" ;;
	*) echo "usage: ssh-config.sh add <alias> <port> <identity> | remove <alias>" >&2; exit 2 ;;
esac
