#!/bin/sh
# The backups' SFTP account (Plan.md P12.2; ADR-0022, S15, U3): an operator's
# PC pulls afkfleet's backups off this host over SFTP, through an account
# that can list and read them and nothing else. Run it as root:
#
#   sudo sh host/sftp-backups.sh install --key <your key.pub> [--backups <folder>]
#   sudo sh host/sftp-backups.sh uninstall
#
# install adds the account afkfleet-backup: uid and gid 65532, the server's
# own user, the only one that can read its 0600 backups; no shell, no home,
# no password. It installs your one public key, and an sshd drop-in that
# confines the account to the backups' folder's parent (ChrootDirectory),
# read-only (internal-sftp -R), with no TTY and no forwarding. The backups'
# folder is .env's AFKFLEET_BACKUPS_DIR, /srv/afkfleet-backups/backups by
# default.
#
# It never edits sshd_config, and never reloads sshd on a mistake: the
# drop-in must pass `sshd -t`, `sshd -T` for the account must show every
# value it sets, and `sshd -T` for everyone else must be what it was before;
# otherwise it removes the drop-in again and changes nothing. Keep your own
# SSH session open until a new login works. uninstall removes the drop-in,
# the key file and the account, and keeps the folders, which hold the
# backups. It never overwrites a file.
#
# Exit codes: 0 done, 1 refused or failed (it says why), 2 usage error.
# POSIX sh only; a throwaway container proves it (fleet-integration's
# slow_sftp_backups_account).
set -eu
umask 022

ACCOUNT=afkfleet-backup
ACCOUNT_ID=65532
SSHD_CONFIG=/etc/ssh/sshd_config
DROPIN=/etc/ssh/sshd_config.d/10-afkfleet-backup.conf
KEYS=/etc/ssh/afkfleet-backup.authorized_keys
DEFAULT_BACKUPS=/srv/afkfleet-backups/backups

say() {
	printf '%s\n' "$*"
}

refuse() {
	printf 'afkfleet sftp: %s\n' "$*" >&2
	exit 1
}

usage() {
	printf 'usage: sudo sh sftp-backups.sh install --key <file.pub> [--backups <folder>] | uninstall\n' >&2
	exit 2
}

sshd_bin() {
	if command -v sshd >/dev/null 2>&1; then
		command -v sshd
	elif [ -x /usr/sbin/sshd ]; then
		printf '%s' /usr/sbin/sshd
	else
		refuse "sshd isn't installed here"
	fi
}

# Reloads sshd through the init system it finds; open sessions stay.
reload_sshd() {
	if command -v systemctl >/dev/null 2>&1; then
		systemctl reload ssh 2>/dev/null || systemctl reload sshd || return 1
	elif command -v rc-service >/dev/null 2>&1; then
		rc-service sshd reload || return 1
	else
		return 1
	fi
}

# Whether $1 is owned by root and writable by root alone, as
# ChrootDirectory demands of the folder and every one above it.
root_only() {
	set -- "$1" "$(stat -c '%u %a' "$1")"
	owner=${2%% *}
	mode=${2#* }
	[ "$owner" = 0 ] || return 1
	case $mode in
	*[2367][0-7] | *[0-7][2367]) return 1 ;;
	esac
	return 0
}

# Whether the account's name, uid or gid is taken.
taken() {
	grep -q "^$ACCOUNT:" /etc/passwd && return 0
	grep -q "^[^:]*:[^:]*:$ACCOUNT_ID:" /etc/passwd && return 0
	grep -q "^$ACCOUNT:" /etc/group && return 0
	grep -q "^[^:]*:[^:]*:$ACCOUNT_ID:" /etc/group && return 0
	return 1
}

# The sshd -T settings the account must get.
EXPECTED="authenticationmethods publickey
passwordauthentication no
kbdinteractiveauthentication no
authorizedkeysfile $KEYS
permittty no
permituserrc no
allowtcpforwarding no
allowagentforwarding no
allowstreamlocalforwarding no
x11forwarding no
permittunnel no
disableforwarding yes"

install() {
	key=$1
	backups=$2
	[ "$(id -u)" = 0 ] || refuse "run it as root: it adds an account and an sshd drop-in"
	sshd=$(sshd_bin)
	case $backups in
	/*/*) ;;
	*) refuse "the backups' folder must be an absolute path inside a folder of its own: $backups" ;;
	esac
	case $backups in
	*[!A-Za-z0-9./_-]*) refuse "the backups' folder's path holds a character it can't: $backups" ;;
	esac
	chroot=$(dirname "$backups")
	start=/$(basename "$backups")
	[ -d "$backups" ] || refuse "$backups doesn't exist: run setup.sh init first"
	[ "$(stat -c '%u:%g %a' "$backups")" = "$ACCOUNT_ID:$ACCOUNT_ID 700" ] ||
		refuse "$backups must belong to $ACCOUNT_ID:$ACCOUNT_ID with mode 700"
	part=$chroot
	while :; do
		root_only "$part" || refuse "$part must belong to root and be writable by root alone, as sshd's ChrootDirectory demands"
		[ "$part" = / ] && break
		part=$(dirname "$part")
	done
	[ -f "$key" ] || refuse "there's no key file at $key"
	[ "$(grep -c . "$key")" = 1 ] || refuse "$key must hold one public key, on one line"
	ssh-keygen -l -f "$key" >/dev/null 2>&1 || refuse "$key isn't a public key ssh-keygen can read"
	line=$(grep . "$key")
	case $line in
	*'"'* | *"'"* | *\\*) refuse "$key holds a character a key line can't" ;;
	esac
	[ -e "$DROPIN" ] && refuse "$DROPIN exists already: run uninstall first"
	[ -e "$KEYS" ] && refuse "$KEYS exists already: run uninstall first"
	taken && refuse "the account $ACCOUNT, or the uid or gid $ACCOUNT_ID, is taken on this host"
	# Drop-ins apply only when sshd_config includes them, above every Match block.
	include=$(awk '
		/^[ \t]*[Mm][Aa][Tt][Cc][Hh][ \t]/ { if (!seen) { print "match"; exit } }
		/^[ \t]*[Ii][Nn][Cc][Ll][Uu][Dd][Ee][ \t]+\/etc\/ssh\/sshd_config\.d\/\*\.conf[ \t]*$/ { seen = 1; print "include"; exit }
	' "$SSHD_CONFIG")
	[ "$include" = include ] ||
		refuse "$SSHD_CONFIG must include /etc/ssh/sshd_config.d/*.conf above any Match block: add the line 'Include /etc/ssh/sshd_config.d/*.conf' near its top yourself"
	"$sshd" -t || refuse "sshd's config fails sshd -t already: fix it first"
	before=$("$sshd" -T)

	printf 'restrict %s\n' "$line" >"$KEYS"
	chmod 644 "$KEYS"
	cat >"$DROPIN" <<EOF
# afkfleet's backups account (deploy/host/sftp-backups.sh): read-only SFTP
# in the backups' folder, nothing else. Removed by its uninstall.
Match User $ACCOUNT
	AuthenticationMethods publickey
	PasswordAuthentication no
	KbdInteractiveAuthentication no
	AuthorizedKeysFile $KEYS
	TrustedUserCAKeys none
	ChrootDirectory $chroot
	ForceCommand internal-sftp -R -d $start
	PermitTTY no
	PermitUserRC no
	DisableForwarding yes
	AllowTcpForwarding no
	AllowAgentForwarding no
	AllowStreamLocalForwarding no
	X11Forwarding no
	PermitTunnel no
	PermitListen none
	PermitOpen none
EOF
	chmod 644 "$DROPIN"
	if ! "$sshd" -t; then
		rm -f "$DROPIN" "$KEYS"
		refuse "the drop-in fails sshd -t: removed again, nothing changed"
	fi
	got=$("$sshd" -T -C "user=$ACCOUNT,host=example.com,addr=192.0.2.10")
	missing=""
	for setting in "chrootdirectory $chroot" "forcecommand internal-sftp -R -d $start"; do
		printf '%s\n' "$got" | grep -qxF "$setting" || missing="$missing [$setting]"
	done
	while IFS= read -r setting; do
		printf '%s\n' "$got" | grep -qxF "$setting" || missing="$missing [$setting]"
	done <<EOF
$EXPECTED
EOF
	if printf '%s\n' "$got" | grep -qE '^(allowusers|allowgroups) '; then
		allowed=$(printf '%s\n' "$got" | grep -E '^(allowusers|allowgroups) ')
		case $allowed in
		*"$ACCOUNT"*) ;;
		*) missing="$missing [AllowUsers or AllowGroups leaves the account out]" ;;
		esac
	fi
	after=$("$sshd" -T)
	[ "$after" = "$before" ] || missing="$missing [it changed sshd's settings for everyone else]"
	if [ -n "$missing" ]; then
		rm -f "$DROPIN" "$KEYS"
		refuse "an earlier setting wins over the drop-in, so it's removed again and nothing changed:$missing"
	fi

	groupadd --gid "$ACCOUNT_ID" "$ACCOUNT"
	useradd --uid "$ACCOUNT_ID" --gid "$ACCOUNT_ID" --home-dir /nonexistent --no-create-home \
		--shell /usr/sbin/nologin "$ACCOUNT"
	# `*`, not `!`: a key login works, and no password ever does.
	usermod --password '*' "$ACCOUNT"
	if ! reload_sshd; then
		say "sshd wasn't reloaded: reload it with your init system (the drop-in is checked and in place)"
		exit 1
	fi
	say "ok: the account $ACCOUNT reads $backups over SFTP, read-only"
	for hostkey in /etc/ssh/ssh_host_ed25519_key.pub /etc/ssh/ssh_host_ecdsa_key.pub; do
		if [ -f "$hostkey" ]; then
			say "this host's key, to compare at your PC's first connection: $(ssh-keygen -l -f "$hostkey")"
			break
		fi
	done
	say "pull with: sftp -i <your key> $ACCOUNT@<this host> (start a new login before closing your own)"
}

uninstall() {
	[ "$(id -u)" = 0 ] || refuse "run it as root: it removes an account and an sshd drop-in"
	sshd=$(sshd_bin)
	if [ -e "$DROPIN" ]; then
		rm -f "$DROPIN"
		"$sshd" -t || refuse "sshd's config fails sshd -t without the drop-in: fix it before reloading"
		reload_sshd || say "sshd wasn't reloaded: reload it with your init system"
	fi
	rm -f "$KEYS"
	if grep -q "^$ACCOUNT:" /etc/passwd; then
		userdel "$ACCOUNT"
	fi
	if grep -q "^$ACCOUNT:" /etc/group; then
		groupdel "$ACCOUNT"
	fi
	say "ok: the account, its key and the drop-in are gone; the backups' folders stay"
}

[ $# -ge 1 ] || usage
command=$1
shift
case $command in
install)
	key=""
	backups=$DEFAULT_BACKUPS
	while [ $# -gt 0 ]; do
		case $1 in
		--key) [ $# -ge 2 ] || usage; key=$2; shift 2 ;;
		--backups) [ $# -ge 2 ] || usage; backups=$2; shift 2 ;;
		*) usage ;;
		esac
	done
	[ -n "$key" ] || usage
	install "$key" "$backups"
	;;
uninstall)
	[ $# -eq 0 ] || usage
	uninstall
	;;
*) usage ;;
esac
