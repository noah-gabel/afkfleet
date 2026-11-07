#!/bin/sh
# afkfleet's setup script (Plan.md P12.2; ADR-0022, S18, U8), run on the host
# from the folder it lives in, beside compose.yaml and the host's .env:
#
#   sh setup.sh check          what's there and which proxy path fits; changes nothing
#   sh setup.sh init           a first install's one-time steps, then afkfleet started
#   sh setup.sh fetch X.Y.Z    a newer release's files, checked, into this folder
#   sh setup.sh update         a backup with the running server, then the new images
#
# Exit codes: 0 done, 1 refused or failed (it says why), 2 usage error.
#
# It needs only Docker with Compose v2.24 or newer, and for fetch curl,
# sha256sum and tar. It reads .env and RELEASE line by line and never sources
# them. A release's RELEASE names its images by digest (ADR-0022, S23, S33);
# a line in .env runs another image instead. Every key is made inside the
# server's image, as the image's own user (65532), into the project's
# volumes, and never printed: the vault key, the CA and the server's
# certificate. Root is needed only to create the backups' folder, which must
# belong to that same user so the server can write it and the SFTP account
# can read it (U3). It never overwrites an operator's file, and on a failure
# it names what this run created or left, deleting nothing of the
# operator's.
#
# POSIX sh only (dash, busybox): no arrays, [[, local, pipefail, echo -e,
# read -s, mktemp, readlink -f or sed -i. scripts/deploy-setup.test.mjs
# checks that and runs it against a stub docker.
set -eu
umask 077

# The server's user inside the images (distroless's nonroot).
NONROOT=65532
# The config paths inside the containers.
SERVER_CONFIG=/etc/afkfleet/server.toml
AGENT_CONFIG=/etc/afkfleet/agent.toml
# Where the backups' folder is unless .env says otherwise.
DEFAULT_BACKUPS=/srv/afkfleet-backups/backups
# The proxy paths, as .env's COMPOSE_FILE names them.
CADDY_PATH=compose.yaml:compose.edge.yaml:compose.caddy.yaml
CONTAINER_PATH=compose.yaml:compose.edge.yaml
HOST_PATH=compose.yaml:compose.loopback.yaml

# The files compose mounts into the containers, which the images' user reads.
MOUNTED="Caddyfile files.sh prod/server.toml prod/agent.toml"
# Where fetch downloads a release's files from: the repository's own
# releases (scripts/release.mjs's SOURCE; a test holds them equal).
RELEASES=https://github.com/noah-gabel/afkfleet/releases/download
# fetch's own folder: made by it, never there before it, removed after a
# success and left for a look after a failure.
FETCH=.afkfleet-fetch

# What this run created, for the message after a failure.
CREATED=""
# What this run left for a look, for the message after a failure.
LEFT=""

say() {
	printf '%s\n' "$*"
}

ok() {
	printf 'ok: %s\n' "$*"
}

refuse() {
	printf 'afkfleet setup: %s\n' "$*" >&2
	if [ -n "$CREATED" ]; then
		printf 'afkfleet setup: this run created:%s\n' "$CREATED" >&2
		printf 'afkfleet setup: to start over, remove them with: docker compose down --volumes\n' >&2
	fi
	if [ -n "$LEFT" ]; then
		printf 'afkfleet setup: %s\n' "$LEFT" >&2
	fi
	exit 1
}

usage() {
	printf 'usage: sh setup.sh check|init|update|fetch X.Y.Z\n' >&2
	exit 2
}

# The value of KEY in .env, the last line that sets it, or nothing.
env_value() {
	found=""
	while IFS= read -r line || [ -n "$line" ]; do
		case $line in
		"$1="*) found=${line#*=} ;;
		esac
	done <.env
	printf '%s' "$found"
}

# Whether $1 holds only the characters of a host name, an address or a list
# of them, so nothing in it can be read as more than one value.
plain() {
	case $1 in
	"" | *[!A-Za-z0-9.,:@_-]*) return 1 ;;
	esac
	return 0
}

# Whether $1 is an image reference: a registry path, a tag and a digest
# allowed (ghcr.io/owner/afkfleet-server:1.0), but no whitespace, no glob
# character and no leading -, so it's one word and never an option.
image_ref() {
	case $1 in
	"" | -* | *[!A-Za-z0-9./:@_-]*) return 1 ;;
	esac
	return 0
}

# Whether $1 is an image pinned by its digest, as RELEASE names one:
# a reference ending in @sha256: and 64 small hex digits.
digest_ref() {
	image_ref "$1" || return 1
	case $1 in
	*@sha256:*) ;;
	*) return 1 ;;
	esac
	digest=${1##*@sha256:}
	[ ${#digest} -eq 64 ] || return 1
	case $digest in
	*[!0-9a-f]*) return 1 ;;
	esac
	return 0
}

# The value of KEY in the RELEASE file $2 (./RELEASE by default), the last
# line that sets it, or nothing; # lines are comments.
release_value() {
	found=""
	while IFS= read -r line || [ -n "$line" ]; do
		case $line in
		"#"*) ;;
		"$1="*) found=${line#*=} ;;
		esac
	done <"${2:-RELEASE}"
	printf '%s' "$found"
}

# The names the RELEASE file $1 lists (its file= lines), one per line.
release_names() {
	while IFS= read -r line || [ -n "$line" ]; do
		case $line in
		file=*) printf '%s\n' "${line#file=*  }" ;;
		esac
	done <"$1"
}

# The SHA-256 the RELEASE file $1 lists for the name $2, or nothing.
release_hash() {
	found=""
	while IFS= read -r line || [ -n "$line" ]; do
		case $line in
		"file="*"  $2")
			found=${line#file=}
			found=${found%%  *}
			;;
		esac
	done <"$1"
	printf '%s' "$found"
}

# The SHA-256 of the file $1.
hash_of() {
	sum=$(sha256sum -- "$1") || return 1
	printf '%s' "${sum%% *}"
}

# Whether $1 is a version, X.Y.Z: three numbers without a leading zero, of
# at most 9 digits each.
semver() {
	case $1 in
	"" | *[!0-9.]* | .* | *. | *..*) return 1 ;;
	esac
	rest=$1
	parts=0
	while :; do
		part=${rest%%.*}
		case $part in
		0?*) return 1 ;;
		esac
		[ ${#part} -le 9 ] || return 1
		parts=$((parts + 1))
		case $rest in
		*.*) rest=${rest#*.} ;;
		*) break ;;
		esac
	done
	[ "$parts" -eq 3 ]
}

# Compares the versions $1 and $2, part by part: CMP becomes -1, 0 or 1.
vercmp() {
	left=$1
	right=$2
	CMP=0
	while [ -n "$left" ]; do
		x=${left%%.*}
		y=${right%%.*}
		if [ "$x" -lt "$y" ]; then
			CMP=-1
			return 0
		fi
		if [ "$x" -gt "$y" ]; then
			CMP=1
			return 0
		fi
		case $left in
		*.*)
			left=${left#*.}
			right=${right#*.}
			;;
		*) left="" ;;
		esac
	done
}

# Whether $1 is a plain name for a release file: letters, digits and ._-,
# not hidden.
plain_name() {
	case $1 in
	"" | .* | *[!A-Za-z0-9._-]*) return 1 ;;
	esac
	return 0
}

# Whether $1 is a name env.example uses as a placeholder.
placeholder_name() {
	case $1 in
	example.com | *.example.com | example.org | *.example.org | example.net | *.example.net) return 0 ;;
	esac
	return 1
}

# Whether $1 is a folder path the backups can live in: absolute, without
# //, . or .. parts or a trailing /, of plain characters, and inside a folder
# of its own other than / (sftp-backups.sh's chroot). The same rules as
# host/sftp-backups.sh's.
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

# Whether stat's mode $1 (`%a`: octal, without leading zeros, so 2 for 0002)
# lets the group or others write.
others_write() {
	padded=$(printf '%04d' "$1")
	case $padded in
	??[2367]? | ???[2367]) return 0 ;;
	esac
	return 1
}

check_docker() {
	command -v docker >/dev/null 2>&1 || refuse "docker isn't installed, or isn't on PATH"
	docker info >/dev/null 2>&1 || refuse "the Docker daemon doesn't answer: is it running, and may this user use it?"
	version=$(docker compose version --short 2>/dev/null) || refuse "Docker Compose v2 is missing: install the docker compose plugin (the old docker-compose won't do)"
	major=${version%%.*}
	rest=${version#*.}
	minor=${rest%%.*}
	case $major$minor in
	*[!0-9]*) refuse "Docker Compose's version can't be read: $version" ;;
	esac
	if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 24 ]; }; then
		refuse "Docker Compose $version is too old: afkfleet needs 2.24 or newer"
	fi
	options=$(docker info --format '{{.SecurityOptions}}' 2>/dev/null || true)
	case $options in
	*rootless* | *userns*)
		refuse "Docker runs rootless or with user namespaces here: the backups' folder can't belong to the server's user (65532) that way"
		;;
	esac
	ok "Docker and Compose $version"
}

# Sets IMAGE to the image the variable $1 runs: RELEASE's $2, pinned by its
# digest, unless .env names another, which it says; in a folder without
# RELEASE (a checkout of the repository), .env's alone. $3 is the variable's
# value in this shell's own environment, which is refused: compose would
# prefer it over both.
image() {
	[ -z "$3" ] || refuse "$1 is set in this shell's environment, which compose would prefer over .env and RELEASE: unset it"
	named=$(env_value "$1")
	if [ -n "$named" ] || [ ! -f RELEASE ]; then
		image_ref "$named" || refuse "$1 in .env is missing, starts with - or holds a character an image reference can't"
	fi
	IMAGE=$named
	if [ -f RELEASE ]; then
		released=$(release_value "$2")
		digest_ref "$released" || refuse "RELEASE's $2 isn't an image pinned by its digest: fetch the release again (sh setup.sh fetch <version>)"
		if [ -z "$named" ]; then
			IMAGE=$released
		else
			say "warning: .env's $1 runs $named instead of this release's $released"
		fi
	fi
}

check_env() {
	[ -f .env ] || refuse "there's no .env here: copy env.example to .env and fill it in first"
	path=$(env_value COMPOSE_FILE)
	extra=""
	# A host-only override of the operator's own comes last, remote agents' file before it.
	case $path in
	*:compose.override.yaml) path=${path%:compose.override.yaml}; extra=" and compose.override.yaml" ;;
	esac
	case $path in
	*:compose.grpc.yaml) path=${path%:compose.grpc.yaml}; extra=" with remote agents$extra" ;;
	esac
	case $path in
	"$CADDY_PATH") PROXY=caddy ;;
	"$CONTAINER_PATH") PROXY=container ;;
	"$HOST_PATH") PROXY=host ;;
	*) refuse "COMPOSE_FILE in .env isn't one of the paths env.example lists" ;;
	esac
	image AFKFLEET_SERVER_IMAGE server_image "${AFKFLEET_SERVER_IMAGE:-}"
	SERVER_IMAGE=$IMAGE
	image AFKFLEET_AGENT_IMAGE agent_image "${AFKFLEET_AGENT_IMAGE:-}"
	AGENT_IMAGE=$IMAGE
	if [ "$PROXY" = caddy ]; then
		domain=$(env_value AFKFLEET_DOMAIN)
		plain "$domain" || refuse "AFKFLEET_DOMAIN in .env is missing or isn't a plain host name"
		! placeholder_name "$domain" || refuse "AFKFLEET_DOMAIN in .env is still env.example's placeholder"
		tls=$(env_value AFKFLEET_TLS)
		case $tls in
		letsencrypt)
			email=$(env_value AFKFLEET_ACME_EMAIL)
			plain "$email" || refuse "AFKFLEET_ACME_EMAIL in .env is missing or holds a character it can't"
			case $email in
			*@example.com | *@example.org | *@example.net) refuse "AFKFLEET_ACME_EMAIL in .env is still env.example's placeholder" ;;
			esac
			;;
		internal) ;;
		*) refuse "AFKFLEET_TLS in .env must be letsencrypt (or internal, for tests)" ;;
		esac
	fi
	NAMES=$(env_value AFKFLEET_GRPC_NAMES)
	if [ -n "$NAMES" ]; then
		plain "$NAMES" || refuse "AFKFLEET_GRPC_NAMES in .env must be names and addresses, separated by commas"
		rest=$NAMES
		while [ -n "$rest" ]; do
			name=${rest%%,*}
			case $rest in
			*,*) rest=${rest#*,} ;;
			*) rest="" ;;
			esac
			! placeholder_name "$name" || refuse "AFKFLEET_GRPC_NAMES in .env is still env.example's placeholder ($name)"
		done
	fi
	BACKUPS=$(env_value AFKFLEET_BACKUPS_DIR)
	[ -n "$BACKUPS" ] || BACKUPS=$DEFAULT_BACKUPS
	backups_path "$BACKUPS" ||
		refuse "AFKFLEET_BACKUPS_DIR in .env must be an absolute path of letters, digits and ._-, without //, . or .. parts or a trailing /, inside a folder of its own other than /"
	ok ".env: the $PROXY proxy path$extra"
}

# The files compose mounts must be readable by everyone: the containers read
# them as their own user (65532), and a bundle unpacked under umask 077 would
# leave them owner-only (fetch sets the modes itself).
check_mounted() {
	for file in $MOUNTED; do
		[ -f "$file" ] || refuse "$file is missing here: this folder must hold a release's files (the runbook's Installing)"
		info=$(stat -c '%u:%g %a' "$file")
		padded=$(printf '%04d' "${info##* }")
		case $padded in
		???[4567]) ;;
		*) refuse "$file must be readable by everyone (mode 644): the containers read it as their own user, and it's $info (chmod 644 $file)" ;;
		esac
	done
	ok "the release's files are readable by the containers"
}

# Pulls an image the host doesn't have.
check_image() {
	if ! docker image inspect "$1" >/dev/null 2>&1; then
		docker pull "$1" >/dev/null 2>&1 || refuse "the image $1 can't be pulled"
	fi
}

check_images() {
	check_image "$SERVER_IMAGE"
	check_image "$AGENT_IMAGE"
	ok "the images are here"
}

# Whether host port $1 is free, by publishing it for a moment with the
# server's image: Docker answers the way it will when afkfleet starts.
port_free() {
	if answer=$(docker run --rm -p "$1:8080" "$SERVER_IMAGE" --version 2>&1); then
		return 0
	fi
	case $answer in
	*"already allocated"*) say "port $1 is taken by another container" ;;
	*"address already in use"*) say "port $1 is taken by a program on the host" ;;
	*"permission denied"*) say "port $1 needs a Docker that may bind ports below 1024" ;;
	*) say "port $1 couldn't be checked: $answer" ;;
	esac
	return 1
}

check_ports() {
	free=yes
	port_free 80 || free=no
	port_free 443 || free=no
	if [ "$free" = yes ]; then
		say "ports 80 and 443 are free here: the Caddy path fits"
	else
		say "ports 80 and 443 aren't both free: run afkfleet behind your existing proxy"
	fi
	say "free here isn't reachable from outside: open 80 and 443 in your provider's firewall and the host's"
	if [ "${1:-}" = caddy ] && [ "$free" = no ]; then
		refuse "COMPOSE_FILE picks the Caddy path, but 80 and 443 aren't both free"
	fi
}

# The project's name, as compose reads it.
project() {
	docker compose config --format json 2>/dev/null | sed -n 's/^ *"name": "\([^"]*\)".*/\1/p' | head -n 1
}

check_clean() {
	name=$(project)
	[ -n "$name" ] || refuse "docker compose can't read compose.yaml with this .env"
	containers=$(docker ps -a -q --filter "label=com.docker.compose.project=$name")
	volumes=$(docker volume ls -q --filter "label=com.docker.compose.project=$name")
	if [ -n "$containers" ] || [ -n "$volumes" ]; then
		refuse "the project $name has containers or volumes already: init is for a first install only (an existing install is updated with: sh setup.sh update)"
	fi
	ok "nothing of the project $name exists yet"
}

# The backups' folder: owned by the server's user, owner-only, in a folder
# only root can change (ChrootDirectory's rule for the SFTP account). An
# existing folder is checked and never changed; a missing one is made only
# as root, and named among what this run created.
check_backups() {
	parent=$(dirname "$BACKUPS")
	root=no
	[ "$(id -u)" != 0 ] || root=yes
	if [ -d "$parent" ]; then
		info=$(stat -c '%u:%g %a' "$parent")
		owner=${info%%:*}
		mode=${info##* }
		if [ "$owner" != 0 ] || others_write "$mode"; then
			refuse "$parent must belong to root and be writable by root alone (sshd's ChrootDirectory demands it), not $info; it's left as it is"
		fi
	elif [ ! -d "$(dirname "$parent")" ]; then
		refuse "$(dirname "$parent") doesn't exist: create it first, or pick another AFKFLEET_BACKUPS_DIR"
	elif [ "$root" = no ]; then
		refuse "$parent and $BACKUPS don't exist, and only root can create them for the server's user: run sh setup.sh init with sudo, or create them with: sudo install -d -o root -g root -m 755 $parent && sudo install -d -o $NONROOT -g $NONROOT -m 700 $BACKUPS"
	else
		say "creating $parent as root's, mode 755: the backups' folder's chroot for the SFTP account"
		install -d -o root -g root -m 755 "$parent"
		CREATED="$CREATED $parent"
	fi
	if [ -d "$BACKUPS" ]; then
		owner=$(stat -c '%u:%g %a' "$BACKUPS")
		[ "$owner" = "$NONROOT:$NONROOT 700" ] || refuse "$BACKUPS must belong to $NONROOT:$NONROOT with mode 700, not $owner"
		ok "the backups' folder $BACKUPS"
		return 0
	fi
	if [ "$root" = no ]; then
		refuse "$BACKUPS doesn't exist, and only root can create it for the server's user: run sh setup.sh init with sudo, or create it with: sudo install -d -o $NONROOT -g $NONROOT -m 700 $BACKUPS"
	fi
	say "creating $BACKUPS as root: the server's user ($NONROOT) must own it to write the backups"
	install -d -o "$NONROOT" -g "$NONROOT" -m 700 "$BACKUPS"
	CREATED="$CREATED $BACKUPS"
	ok "the backups' folder $BACKUPS"
}

# The server certificate's names: `server`, which the local agent dials,
# and every name and address in AFKFLEET_GRPC_NAMES, for remote agents.
pki_names() {
	printf '%s\n' "--dns"
	printf '%s\n' "server"
	rest=$NAMES
	while [ -n "$rest" ]; do
		name=${rest%%,*}
		case $rest in
		*,*) rest=${rest#*,} ;;
		*) rest="" ;;
		esac
		[ -n "$name" ] || continue
		# An IPv6 address holds a colon; an IPv4 one only digits and dots.
		case $name in
		*:*) printf '%s\n' "--ip" ;;
		*[!0-9.]*) printf '%s\n' "--dns" ;;
		*) printf '%s\n' "--ip" ;;
		esac
		printf '%s\n' "$name"
	done
}

init() {
	check_docker
	check_env
	check_mounted
	check_images
	if [ "$PROXY" = caddy ]; then
		check_ports caddy
	fi
	check_clean
	check_backups
	CREATED="$CREATED the project's volumes"
	docker compose run --rm -T setup vault create-key --config "$SERVER_CONFIG" || refuse "the vault key couldn't be created"
	ok "the vault key, in the keys volume (copy it off this host separately: see the runbook)"
	# shellcheck disable=SC2046 # one name per line, each its own argument
	set -- $(pki_names)
	docker compose run --rm -T setup pki init --config "$SERVER_CONFIG" "$@" || refuse "the PKI couldn't be created"
	ok "the CA and the server's certificate"
	docker compose up --detach --wait server || refuse "the server didn't become healthy: docker compose logs server says why"
	ok "the server is healthy"
	status=0
	docker compose exec -T server /usr/local/bin/afkfleet-server agent create-enrollment-token --config "$SERVER_CONFIG" |
		docker compose run --rm -T --no-deps agent enroll --config "$AGENT_CONFIG" || status=$?
	case $status in
	0) ok "the local agent is enrolled" ;;
	3) refuse "the server enrolled the local agent, but its identity wasn't stored: disable it (docker compose exec server /usr/local/bin/afkfleet-server agent list, then agent disable --id <its id>) and run init's last steps again from the runbook" ;;
	*) refuse "the local agent couldn't be enrolled" ;;
	esac
	docker compose up --detach --wait || refuse "afkfleet didn't come up healthy: docker compose ps and docker compose logs say why"
	ok "afkfleet is running"
	say ""
	say "Next:"
	say "  1. Create the Owner: docker compose exec server /usr/local/bin/afkfleet-server user create-owner --config $SERVER_CONFIG --username <name>"
	say "  2. Copy the vault key and the CA key off this host, apart from the backups (the runbook says how)."
	say "  3. Let your PC pull the backups: sudo sh host/sftp-backups.sh install --key <your key.pub>"
	say "  4. In the app, connect to this server, log in as the Owner and set up 2FA."
}

check() {
	check_docker
	check_env
	check_mounted
	check_images
	check_ports
	name=$(project)
	if [ -n "$(docker ps -a -q --filter "label=com.docker.compose.project=$name")" ]; then
		say "the project $name is installed here: update it with sh setup.sh update"
	else
		say "the project $name isn't installed yet: sh setup.sh init installs it"
	fi
}

# A running server newer than this folder's release would be replaced by an
# older one: refused. An image without a release version (a local build) isn't
# compared.
check_running_version() {
	[ -f RELEASE ] || return 0
	release=$(release_value version)
	running=$(docker inspect --format '{{index .Config.Labels "org.opencontainers.image.version"}}' "$1")
	if [ -z "$running" ]; then
		say "the running server has no release version (a local build): not compared"
		return 0
	fi
	semver "$running" || refuse "the running server's version label isn't a version ($running): nothing was changed"
	vercmp "$running" "$release"
	[ "$CMP" -le 0 ] ||
		refuse "the running server is $running, newer than this folder's release $release: fetch it (sh setup.sh fetch $running), or restore a backup; nothing was changed"
}

update() {
	[ ! -e "$FETCH" ] || refuse "$FETCH is here: a fetch didn't finish; look inside, remove it (rm -r $FETCH) and fetch again before updating"
	check_docker
	check_env
	id=$(docker compose ps -q server)
	[ -n "$id" ] || refuse "the server isn't running: an update starts from a running, healthy afkfleet"
	health=$(docker inspect --format '{{.State.Health.Status}}' "$id")
	[ "$health" = healthy ] || refuse "the server isn't healthy ($health): fix that before updating"
	check_running_version "$id"
	status=0
	docker compose exec -T server /usr/local/bin/afkfleet-server backup --config "$SERVER_CONFIG" || status=$?
	case $status in
	0) ok "a backup with the old server" ;;
	3) say "warning: the backup is written, but older ones couldn't be pruned" ;;
	*) refuse "the backup failed, so nothing was updated" ;;
	esac
	docker compose pull || refuse "the new images couldn't be pulled; nothing changed"
	docker compose up --detach --wait server || refuse "the new server didn't become healthy: docker compose logs server says why (the backup above restores the old state)"
	ok "the server is updated"
	docker compose up --detach --wait || refuse "afkfleet didn't come up healthy: docker compose ps says which service"
	ok "afkfleet is updated"
}

# Downloads $1 into $2 over HTTPS alone, at most 10 MiB and 5 minutes.
download() {
	curl --proto '=https' --proto-redir '=https' --tlsv1.2 --fail --location --max-redirs 5 \
		--silent --show-error --connect-timeout 20 --max-time 300 --max-filesize 10485760 \
		--output "$2" "$1" ||
		refuse "$1 couldn't be downloaded (curl's exit $?): is the version right, and does this host reach GitHub?"
}

# fetch X.Y.Z (ADR-0022, S23, S35): release X.Y.Z's files, checked, into this
# folder. Every check runs before anything here changes; then each file is
# renamed into place, RELEASE last, so an interrupted fetch can run again.
# It never touches .env, compose.override.yaml or ca.crt, refuses a file
# someone changed here, and leaves a file the new release no longer ships.
fetch() {
	version=$1
	semver "$version" || refuse "$version isn't a version: X.Y.Z, such as 1.2.0"
	[ -f RELEASE ] || refuse "this folder isn't a release's files (it has no RELEASE): install from a release's bundle, as the runbook's Installing says"
	installed=$(release_value version)
	semver "$installed" || refuse "RELEASE's version can't be read ($installed)"
	vercmp "$version" "$installed"
	[ "$CMP" -gt 0 ] || refuse "$version isn't newer than this folder's release $installed: fetch only moves forward (going back is a restore, as the runbook's Restoring a backup says)"
	for tool in curl sha256sum tar cmp sort; do
		command -v "$tool" >/dev/null 2>&1 || refuse "$tool isn't installed, or isn't on PATH"
	done
	if [ -e "$FETCH" ] || [ -L "$FETCH" ]; then
		refuse "$FETCH exists: an earlier fetch stopped there; look inside, then remove it (rm -r $FETCH) and fetch again"
	fi
	mkdir -m 700 "$FETCH" || refuse "$FETCH can't be made here"
	LEFT="the downloads are left in $FETCH for a look: remove it (rm -r $FETCH) before you fetch again"
	bundle=afkfleet-deploy-$version.tar.gz
	download "$RELEASES/v$version/SHA256SUMS" "$FETCH/SHA256SUMS"
	download "$RELEASES/v$version/$bundle" "$FETCH/$bundle"
	ok "downloaded release $version's files"

	lines=0
	while IFS= read -r line || [ -n "$line" ]; do
		case $line in
		*"  $bundle")
			lines=$((lines + 1))
			printf '%s\n' "$line" >"$FETCH/bundle.sum"
			;;
		esac
	done <"$FETCH/SHA256SUMS"
	[ "$lines" -eq 1 ] || refuse "SHA256SUMS names $bundle $lines times, not once: nothing was changed"
	(cd "$FETCH" && sha256sum -c bundle.sum >/dev/null 2>&1) ||
		refuse "$bundle's SHA-256 isn't the one SHA256SUMS lists: nothing was changed"
	ok "its SHA-256 is the one SHA256SUMS lists"

	# What the archive holds, before anything is unpacked: files and folders
	# alone, named plainly, at the top or in host/ or prod/ (S35).
	tar -tvzf "$FETCH/$bundle" >"$FETCH/types" 2>/dev/null || refuse "$bundle can't be read as a tar.gz"
	while IFS= read -r line || [ -n "$line" ]; do
		case $line in
		-* | d*) ;;
		*) refuse "the bundle holds something that isn't a file or a folder: $line" ;;
		esac
	done <"$FETCH/types"
	tar -tzf "$FETCH/$bundle" >"$FETCH/names" 2>/dev/null || refuse "$bundle can't be read as a tar.gz"
	: >"$FETCH/archived"
	: >"$FETCH/folders"
	while IFS= read -r name || [ -n "$name" ]; do
		case $name in
		host/ | prod/) ;;
		host/* | prod/*) plain_name "${name#*/}" || refuse "the bundle holds a name it can't: $name" ;;
		*/*) refuse "the bundle holds a name it can't: $name" ;;
		*) plain_name "$name" || refuse "the bundle holds a name it can't: $name" ;;
		esac
		case $name in
		*/) printf '%s\n' "${name%/}" >>"$FETCH/folders" ;;
		RELEASE) ;;
		*) printf '%s\n' "$name" >>"$FETCH/archived" ;;
		esac
	done <"$FETCH/names"
	mkdir "$FETCH/new" || refuse "$FETCH/new can't be made"
	tar -xzf "$FETCH/$bundle" -C "$FETCH/new" || refuse "$bundle couldn't be unpacked"

	# The new RELEASE: this version, an update from this folder's, listing
	# exactly the archive's files, each with its SHA-256, and never a file of
	# the operator's.
	new=$FETCH/new
	[ -f "$new/RELEASE" ] || refuse "the bundle holds no RELEASE"
	said=$(release_value version "$new/RELEASE")
	[ "$said" = "$version" ] || refuse "the bundle's RELEASE says version $said, not $version"
	from=$(release_value updates_from "$new/RELEASE")
	semver "$from" || refuse "the bundle's RELEASE has no updates_from it can read"
	vercmp "$installed" "$from"
	[ "$CMP" -ge 0 ] ||
		refuse "$version updates only from $from or newer, and this folder's release is $installed: fetch $from first (sh setup.sh fetch $from), then $version"
	release_names "$new/RELEASE" | sort >"$FETCH/listed"
	sort "$FETCH/archived" >"$FETCH/archived.sorted"
	cmp -s "$FETCH/listed" "$FETCH/archived.sorted" || refuse "the bundle's RELEASE doesn't list exactly its files"
	while IFS= read -r name || [ -n "$name" ]; do
		case $name in
		.env | compose.override.yaml | ca.crt) refuse "the bundle would write $name, and fetch never replaces $name" ;;
		esac
	done <"$FETCH/listed"
	: >"$FETCH/new.sums"
	while IFS= read -r line || [ -n "$line" ]; do
		case $line in
		file=*) printf '%s\n' "${line#file=}" >>"$FETCH/new.sums" ;;
		esac
	done <"$new/RELEASE"
	(cd "$new" && sha256sum -c ../new.sums >/dev/null 2>&1) || refuse "a file in the bundle isn't the one its RELEASE lists"
	ok "the bundle holds release $version's files, each as its RELEASE lists it"

	# This folder, before anything moves: each folder a folder, each file the
	# release puts here either missing or this release's or the last one's.
	while IFS= read -r folder || [ -n "$folder" ]; do
		if [ -L "$folder" ] || { [ -e "$folder" ] && [ ! -d "$folder" ]; }; then
			refuse "$folder here isn't a folder, where release $version puts one: move it away, then fetch again"
		fi
	done <"$FETCH/folders"
	while IFS= read -r name || [ -n "$name" ]; do
		[ ! -L "$name" ] || refuse "$name is a link here, where release $version puts a file: move it away, then fetch again"
		[ -e "$name" ] || continue
		[ -f "$name" ] || refuse "$name isn't a regular file here, where release $version puts one: move it away, then fetch again"
		have=$(hash_of "$name") || refuse "$name can't be read"
		[ "$have" != "$(release_hash "$new/RELEASE" "$name")" ] || continue
		[ "$have" != "$(release_hash RELEASE "$name")" ] || continue
		refuse "$name differs from release $installed's and $version's: someone changed it here (settings belong in .env, as the runbook's Settings says); put it back or move it away, then fetch again"
	done <"$FETCH/listed"
	release_names RELEASE >"$FETCH/was"

	# Into place: modes the containers can read whatever the umask, each file
	# renamed over the old one (the running setup.sh keeps its own), RELEASE
	# last.
	while IFS= read -r folder || [ -n "$folder" ]; do
		[ -d "$folder" ] || mkdir -m 755 "$folder" || refuse "$folder couldn't be made"
	done <"$FETCH/folders"
	while IFS= read -r name || [ -n "$name" ]; do
		{ chmod 644 "$new/$name" && mv -f "$new/$name" "$name"; } ||
			refuse "$name couldn't be moved into place; RELEASE still says $installed, so fetch again once that's fixed"
	done <"$FETCH/listed"
	{ chmod 644 "$new/RELEASE" && mv -f "$new/RELEASE" RELEASE; } ||
		refuse "RELEASE couldn't be moved into place; it still says $installed, so fetch again once that's fixed"
	while IFS= read -r name || [ -n "$name" ]; do
		grep -Fqx -- "$name" "$FETCH/listed" ||
			say "left: $name, which release $version no longer ships; remove it once nothing needs it"
	done <"$FETCH/was"
	rm -rf "$FETCH"
	LEFT=""
	ok "this folder holds release $version's files"
	say ""
	say "Next: sh setup.sh update"
}

[ $# -ge 1 ] || usage
case $1 in
check | init | update) [ $# -eq 1 ] || usage ;;
fetch) [ $# -eq 2 ] || usage ;;
*) usage ;;
esac
cd "$(dirname "$0")" || exit 1
case $1 in
check) check ;;
init) init ;;
update) update ;;
fetch) fetch "$2" ;;
esac
