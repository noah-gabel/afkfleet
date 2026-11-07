#!/bin/sh
# afkfleet's file operations on its volumes (Plan.md P12.6; ADR-0022, S25,
# S38, S40, S41). deploy/setup.sh runs it in the `files` service alone
# (`docker compose run --rm files <verb> …`): Caddy's pinned image for its
# busybox, the images' nonroot user (65532), no network, the database's and
# the server certificate's volumes at /vol/data and /vol/tls, the backups
# read-only at /vol/backups, and never the keys or the CA.
#
#   restore-in STAMP NAME   the database, its -wal and -shm moved aside as
#                           <name>.aside-STAMP (the -wal first, so it never
#                           sits beside another database), then the backup
#                           NAME copied in as afkfleet.db, owner-only
#   put-back STAMP          undoes restore-in STAMP: the files there now
#                           moved to <name>.undone-STAMP, the aside ones back
#   tls-aside STAMP         server.crt and server.key moved aside
#   tls-back STAMP          undoes tls-aside STAMP, as put-back does
#   finished STAMP          marks STAMP's restore or renewal finished
#   forget STAMP            deletes exactly STAMP's aside and undone files,
#                           only once STAMP is marked finished
#   list                    names every aside and undone file
#
# STAMP is a UTC time, YYYYMMDDTHHMMSSZ; NAME a backup's file name,
# afkfleet-backup-<STAMP>.db. Nothing is ever moved over a file, and only
# forget deletes, by exact names built from its stamp, and only a set whose
# restore or renewal finished (setup.sh marks it after migrate, or after the
# new certificate) or was put back: its marker .afkfleet-finished-STAMP in
# the database's volume. A restore that stopped halfway leaves the only copy
# of the database aside, which nothing deletes (PR #99's review). Each
# volume's root must be 65532's, owner-only, as the server's image made it.
#
# Exit codes: 0 done, 1 refused (it says why), 2 usage error.
#
# POSIX sh only (busybox, dash): no arrays, [[, local, pipefail, echo -e,
# read -s, mktemp, readlink -f or sed -i. scripts/deploy-files.test.mjs runs
# it against a stub stat; FILES_ROOT points it at a test's folder instead of
# /vol, and compose never sets it.
set -eu
umask 077

ROOT=${FILES_ROOT:-/vol}
DATA=$ROOT/data
TLS=$ROOT/tls
BACKUPS=$ROOT/backups
# The database's files, the -wal first.
DATABASE="afkfleet.db-wal afkfleet.db-shm afkfleet.db"
# The server's pair.
PAIR="server.crt server.key"

say() {
	printf '%s\n' "$*"
}

refuse() {
	printf 'afkfleet files: %s\n' "$*" >&2
	exit 1
}

usage() {
	printf 'usage: files.sh restore-in STAMP NAME | put-back STAMP | tls-aside STAMP | tls-back STAMP | finished STAMP | forget STAMP | list\n' >&2
	exit 2
}

# The marker of STAMP ($1): its restore or renewal finished, or was put back.
marker() {
	printf '%s' "$DATA/.afkfleet-finished-$1"
}

# Marks STAMP ($1) finished: an empty owner-only file, never a link.
finish() {
	if [ -L "$(marker "$1")" ] || { [ -e "$(marker "$1")" ] && [ ! -f "$(marker "$1")" ]; }; then
		refuse ".afkfleet-finished-$1 isn't a regular file: it's left as it is"
	fi
	: >"$(marker "$1")" || refuse "$1 couldn't be marked finished"
	say "marked finished: $1"
}

# Whether $1 is a stamp: YYYYMMDDTHHMMSSZ.
stamp() {
	case $1 in
	[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]T[0-9][0-9][0-9][0-9][0-9][0-9]Z) return 0 ;;
	esac
	return 1
}

# Whether $1 is a backup's name: afkfleet-backup-<stamp>.db.
backup_name() {
	case $1 in
	afkfleet-backup-*.db) ;;
	*) return 1 ;;
	esac
	inner=${1#afkfleet-backup-}
	stamp "${inner%.db}"
}

# The volume root $1 must belong to the server's user, owner-only.
check_root() {
	info=$(stat -c '%u:%g %a' "$1") || refuse "$1 can't be read"
	[ "$info" = "65532:65532 700" ] ||
		refuse "$1 must belong to 65532:65532 with mode 700, as the server's image makes it, not $info"
}

# Moves $1 to $2, never over a file: refuses when $2 exists, or the move
# didn't happen.
move() {
	if [ -e "$2" ] || [ -L "$2" ]; then
		refuse "$2 exists already: nothing is moved over a file"
	fi
	mv -n "$1" "$2" || refuse "$1 couldn't be moved to $2"
	if [ -e "$1" ] || [ ! -e "$2" ]; then
		refuse "$1 couldn't be moved to $2"
	fi
}

# Moves aside, as <name>.aside-STAMP ($3), the files $2 in the folder $1 that
# are there; checks every name first.
aside() {
	for file in $2; do
		if [ -e "$1/$file.aside-$3" ] || [ -L "$1/$file.aside-$3" ]; then
			refuse "$file.aside-$3 exists already: nothing is moved over a file"
		fi
	done
	for file in $2; do
		if [ -e "$1/$file" ] || [ -L "$1/$file" ]; then
			move "$1/$file" "$1/$file.aside-$3"
			say "moved aside: $file as $file.aside-$3"
		fi
	done
}

# Puts back the files $2 in the folder $1 that are aside under STAMP ($3):
# what's there now goes to <name>.undone-STAMP first.
put_back() {
	found=no
	for file in $2; do
		[ ! -e "$1/$file.aside-$3" ] || found=yes
		if [ -e "$1/$file.undone-$3" ] || [ -L "$1/$file.undone-$3" ]; then
			refuse "$file.undone-$3 exists already: nothing is moved over a file"
		fi
	done
	[ "$found" = yes ] || refuse "nothing is aside under $3"
	for file in $2; do
		if [ -e "$1/$file" ] || [ -L "$1/$file" ]; then
			move "$1/$file" "$1/$file.undone-$3"
			say "moved: $file as $file.undone-$3"
		fi
	done
	for file in $2; do
		if [ -e "$1/$file.aside-$3" ]; then
			move "$1/$file.aside-$3" "$1/$file"
			say "put back: $file"
		fi
	done
	# What's left under the stamp is the undone copy: forget may delete it.
	finish "$3"
}

restore_in() {
	check_root "$DATA"
	check_root "$BACKUPS"
	if [ -L "$BACKUPS/$2" ] || [ ! -f "$BACKUPS/$2" ]; then
		refuse "there's no backup $2 in the backups' folder"
	fi
	partial=$DATA/.afkfleet-restore-$1.partial
	if [ -e "$partial" ] || [ -L "$partial" ]; then
		refuse "$partial exists already: an earlier restore under $1 stopped there"
	fi
	aside "$DATA" "$DATABASE" "$1"
	cp "$BACKUPS/$2" "$partial" || refuse "$2 couldn't be copied in"
	chmod 600 "$partial"
	cmp -s "$BACKUPS/$2" "$partial" || refuse "the copy of $2 differs from it"
	move "$partial" "$DATA/afkfleet.db"
	say "restored: afkfleet.db from $2"
}

# Deletes exactly STAMP's ($1) aside and undone files, each only a regular
# file, once STAMP is marked finished; checks every one first.
forget() {
	check_root "$DATA"
	check_root "$TLS"
	if [ -L "$(marker "$1")" ] || [ ! -f "$(marker "$1")" ]; then
		refuse "$1 isn't marked finished: its restore or renewal stopped halfway, so what's aside may be the only copy of the database or the certificate; nothing was deleted. Put it back first (docker compose run --rm files put-back $1, or tls-back $1), then forget it"
	fi
	for kind in aside undone; do
		for file in $DATABASE; do
			name=$DATA/$file.$kind-$1
			if [ -L "$name" ] || { [ -e "$name" ] && [ ! -f "$name" ]; }; then
				refuse "$file.$kind-$1 isn't a regular file: it's left as it is, and nothing was deleted"
			fi
		done
		for file in $PAIR; do
			name=$TLS/$file.$kind-$1
			if [ -L "$name" ] || { [ -e "$name" ] && [ ! -f "$name" ]; }; then
				refuse "$file.$kind-$1 isn't a regular file: it's left as it is, and nothing was deleted"
			fi
		done
	done
	for kind in aside undone; do
		for file in $DATABASE; do
			if [ -f "$DATA/$file.$kind-$1" ]; then
				rm -f "$DATA/$file.$kind-$1"
				say "deleted: $file.$kind-$1"
			fi
		done
		for file in $PAIR; do
			if [ -f "$TLS/$file.$kind-$1" ]; then
				rm -f "$TLS/$file.$kind-$1"
				say "deleted: $file.$kind-$1"
			fi
		done
	done
	partial=$DATA/.afkfleet-restore-$1.partial
	if [ -f "$partial" ] && [ ! -L "$partial" ]; then
		rm -f "$partial"
		say "deleted: .afkfleet-restore-$1.partial"
	fi
	rm -f "$(marker "$1")"
}

# Names every aside and undone file, by its volume.
list() {
	check_root "$DATA"
	check_root "$TLS"
	for volume in data tls; do
		for name in "$ROOT/$volume"/*.aside-* "$ROOT/$volume"/*.undone-*; do
			[ -e "$name" ] || [ -L "$name" ] || continue
			say "$volume: ${name##*/}"
		done
	done
}

[ $# -ge 1 ] || usage
case $1 in
restore-in)
	if [ $# -ne 3 ] || ! stamp "$2" || ! backup_name "$3"; then usage; fi
	restore_in "$2" "$3"
	;;
put-back)
	if [ $# -ne 2 ] || ! stamp "$2"; then usage; fi
	check_root "$DATA"
	put_back "$DATA" "$DATABASE" "$2"
	;;
tls-aside)
	if [ $# -ne 2 ] || ! stamp "$2"; then usage; fi
	check_root "$TLS"
	aside "$TLS" "$PAIR" "$2"
	;;
tls-back)
	if [ $# -ne 2 ] || ! stamp "$2"; then usage; fi
	check_root "$TLS"
	check_root "$DATA"
	put_back "$TLS" "$PAIR" "$2"
	;;
finished)
	if [ $# -ne 2 ] || ! stamp "$2"; then usage; fi
	check_root "$DATA"
	finish "$2"
	;;
forget)
	if [ $# -ne 2 ] || ! stamp "$2"; then usage; fi
	forget "$2"
	;;
list)
	[ $# -eq 1 ] || usage
	list
	;;
*) usage ;;
esac
