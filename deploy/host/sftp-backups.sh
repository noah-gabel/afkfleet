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
		refuse "sshd isn't installed here; nothing was changed"
	fi
}

# Refuses, before anything is written, when one of the tools $@ is missing:
# a step that fails halfway (exit 127) must never be what tells.
need() {
	for tool in "$@"; do
		command -v "$tool" >/dev/null 2>&1 ||
			refuse "$tool isn't installed here, or isn't on root's PATH; nothing was changed"
	done
}

# The command that reloads sshd here, in RELOAD: systemctl or rc-service.
find_reload() {
	if command -v systemctl >/dev/null 2>&1; then
		RELOAD=systemctl
	elif command -v rc-service >/dev/null 2>&1; then
		RELOAD=rc-service
	else
		refuse "neither systemctl nor rc-service is here to reload sshd with; nothing was changed"
	fi
}

# Reloads sshd through RELOAD; open sessions stay.
reload_sshd() {
	case $RELOAD in
	systemctl) systemctl reload ssh 2>/dev/null || systemctl reload sshd ;;
	rc-service) rc-service sshd reload ;;
	*) return 1 ;;
	esac
}

# What this run added, in order: keys, dropin, group, account. A failure
# removes exactly these again, so it leaves the host as it found it.
ADDED=""

# Removes what this run added, newest first, and names in LEFTOVER what it
# couldn't remove.
cleanup() {
	LEFTOVER=""
	case " $ADDED " in
	*" account "*) userdel "$ACCOUNT" >/dev/null 2>&1 || LEFTOVER="$LEFTOVER the account $ACCOUNT" ;;
	esac
	case " $ADDED " in
	*" group "*) groupdel "$ACCOUNT" >/dev/null 2>&1 || LEFTOVER="$LEFTOVER the group $ACCOUNT" ;;
	esac
	case " $ADDED " in
	*" dropin "*) rm -f "$DROPIN" || LEFTOVER="$LEFTOVER $DROPIN" ;;
	esac
	case " $ADDED " in
	*" keys "*) rm -f "$KEYS" || LEFTOVER="$LEFTOVER $KEYS" ;;
	esac
	ADDED=""
}

# Refuses after something was written: removes it again first.
fail() {
	cleanup
	if [ -n "$LEFTOVER" ]; then
		refuse "$*; what this run added was removed again, except$LEFTOVER: remove those by hand"
	fi
	refuse "$*; what this run added was removed again, nothing changed"
}

# A step that fails where none was expected (set -e) still removes what this
# run added, and exits 1 saying so.
on_exit() {
	status=$?
	if [ -n "$ADDED" ]; then
		cleanup
		printf 'afkfleet sftp: a step failed (exit %s); what this run added was removed again%s\n' \
			"$status" "${LEFTOVER:+, except$LEFTOVER}" >&2
		exit 1
	fi
}
trap on_exit EXIT

# Whether $1 is owned by root and writable by root alone, as
# ChrootDirectory demands of the folder and every one above it. stat's %a
# prints the mode without leading zeros (2 for 0002), so it's padded first.
root_only() {
	set -- "$1" "$(stat -c '%u %a' "$1")"
	owner=${2%% *}
	mode=$(printf '%04d' "${2#* }")
	[ "$owner" = 0 ] || return 1
	case $mode in
	??[2367]? | ???[2367]) return 1 ;;
	esac
	return 0
}

# Whether $1 is a folder path the backups can live in: absolute, without
# //, . or .. parts or a trailing /, of plain characters, and inside a folder
# of its own other than /, which becomes the account's chroot. setup.sh
# holds AFKFLEET_BACKUPS_DIR to the same rules.
backups_path() {
	case $1 in
	/*) ;;
	*) return 1 ;;
	esac
	case $1 in
	*//* | */./* | */. | */../* | */.. | */ | *[!A-Za-z0-9./_-]*) return 1 ;;
	esac
	[ "$(dirname "$1")" != / ] || return 1
	return 0
}

# Whether the account's name, uid or gid is taken, in any of the host's
# account databases.
taken() {
	getent passwd "$ACCOUNT" >/dev/null && return 0
	getent passwd "$ACCOUNT_ID" >/dev/null && return 0
	getent group "$ACCOUNT" >/dev/null && return 0
	getent group "$ACCOUNT_ID" >/dev/null && return 0
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
	need getent groupadd useradd usermod userdel groupdel ssh-keygen stat awk grep
	sshd=$(sshd_bin)
	find_reload
	backups_path "$backups" ||
		refuse "the backups' folder must be an absolute path of letters, digits and ._-, without //, . or .. parts or a trailing /, inside a folder of its own other than /: $backups"
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
	line=$(grep . "$key")
	# The line starts with the key's type: an authorized_keys option before
	# it (cert-authority, say) would come through behind `restrict`.
	type=${line%% *}
	case $type in
	"" | *[!a-z0-9@.-]*) type="" ;;
	esac
	case $type in
	ssh-ed25519 | ssh-rsa | ecdsa-sha2-* | sk-*) ;;
	*) refuse "$key must hold one public key line that starts with its type (ssh-ed25519, ecdsa-sha2-…, sk-…, ssh-rsa), with nothing before it" ;;
	esac
	ssh-keygen -l -f "$key" >/dev/null 2>&1 || refuse "$key isn't a public key ssh-keygen can read"
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

	# Neither file existed (checked above), so what a failure removes is ours.
	ADDED="$ADDED keys"
	{ printf 'restrict %s\n' "$line" >"$KEYS" && chmod 644 "$KEYS"; } || fail "$KEYS couldn't be written"
	ADDED="$ADDED dropin"
	write_dropin || fail "$DROPIN couldn't be written"
	"$sshd" -t || fail "the drop-in fails sshd -t"
	got=$("$sshd" -T -C "user=$ACCOUNT,host=example.com,addr=192.0.2.10") || fail "sshd -T for the account failed"
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
	after=$("$sshd" -T) || fail "sshd -T failed"
	[ "$after" = "$before" ] || missing="$missing [it changed sshd's settings for everyone else]"
	[ -z "$missing" ] || fail "an earlier setting wins over the drop-in:$missing"

	# The name, uid and gid were free (checked above), so a group or an
	# account of that name now is this run's, even after a failed step.
	if ! groupadd --gid "$ACCOUNT_ID" "$ACCOUNT"; then
		! getent group "$ACCOUNT" >/dev/null || ADDED="$ADDED group"
		fail "groupadd couldn't add the group $ACCOUNT"
	fi
	ADDED="$ADDED group"
	if ! useradd --uid "$ACCOUNT_ID" --gid "$ACCOUNT_ID" --home-dir /nonexistent --no-create-home \
		--shell /usr/sbin/nologin "$ACCOUNT"; then
		! getent passwd "$ACCOUNT" >/dev/null || ADDED="$ADDED account"
		fail "useradd couldn't add the account $ACCOUNT"
	fi
	ADDED="$ADDED account"
	# `*`, not `!`: a key login works, and no password ever does.
	usermod --password '*' "$ACCOUNT" || fail "usermod couldn't set the account's password field to *"
	reload_sshd || fail "sshd couldn't be reloaded with $RELOAD"
	ADDED=""
	say "ok: the account $ACCOUNT reads $backups over SFTP, read-only"
	for hostkey in /etc/ssh/ssh_host_ed25519_key.pub /etc/ssh/ssh_host_ecdsa_key.pub; do
		if [ -f "$hostkey" ]; then
			say "this host's key, to compare at your PC's first connection: $(ssh-keygen -l -f "$hostkey")"
			break
		fi
	done
	say "pull with: sftp -i <your key> $ACCOUNT@<this host> (start a new login before closing your own)"
}

# Writes the drop-in for the account: chroot $chroot, start folder $start.
write_dropin() {
	cat >"$DROPIN" <<EOF || return 1
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
}

uninstall() {
	[ "$(id -u)" = 0 ] || refuse "run it as root: it removes an account and an sshd drop-in"
	need getent userdel groupdel
	sshd=$(sshd_bin)
	find_reload
	# The account goes first: once it's gone, nothing logs in with the key,
	# and when it can't go (it's logged in, say) everything stays as it was.
	if getent passwd "$ACCOUNT" >/dev/null; then
		userdel "$ACCOUNT" ||
			refuse "userdel couldn't remove the account $ACCOUNT (is it logged in? end its sessions and run uninstall again); nothing was removed"
	fi
	if getent group "$ACCOUNT" >/dev/null; then
		groupdel "$ACCOUNT" ||
			refuse "groupdel couldn't remove the group $ACCOUNT; the account is gone, the drop-in and the key file stay: run uninstall again"
	fi
	if [ -e "$DROPIN" ]; then
		rm -f "$DROPIN"
		"$sshd" -t || refuse "sshd's config fails sshd -t without the drop-in: fix it, then reload sshd"
		reload_sshd || refuse "sshd couldn't be reloaded with $RELOAD: reload it yourself; the account and the drop-in are gone"
	fi
	rm -f "$KEYS"
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
