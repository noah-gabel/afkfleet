#!/bin/sh
# afkfleet's setup script (Plan.md P12.2; ADR-0022, S18, U8), run on the host
# from the folder it lives in, beside compose.yaml and the host's .env:
#
#   sh setup.sh check          what's there and which proxy path fits; changes nothing
#   sh setup.sh init           a first install's one-time steps, then afkfleet started
#   sh setup.sh fetch X.Y.Z    a newer release's files, checked, into this folder
#   sh setup.sh update         a backup with the running server, then the new images;
#                              in a remote agent's folder, the agent's new image alone
#   sh setup.sh restore NAME   the backup NAME as the database, the old one aside
#   sh setup.sh forget-aside [STAMP]
#                              what restore and renew-cert set aside: listed, or
#                              STAMP's deleted
#   sh setup.sh renew-cert     the server's certificate renewed from the same CA
#   sudo sh setup.sh keys-backup [DEST]
#                              the keys' copy: a root-only folder, names printed
#   sudo sh setup.sh keys-restore DIR
#                              a new host's keys and CA from that copy
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
# A remote agent's folder, on another host than the server's.
AGENT_PATH=compose.agent.yaml

# The files compose mounts into the containers, which the images' user reads:
# on the server's host, and in a remote agent's folder.
MOUNTED="Caddyfile files.sh prod/server.toml prod/agent.toml"
AGENT_MOUNTED="prod/agent.remote.toml"
# Where fetch downloads a release's files from: the repository's own
# releases (scripts/release.mjs's SOURCE; a test holds them equal).
RELEASES=https://github.com/noah-gabel/afkfleet/releases/download
# fetch's own folder: made by it, never there before it, removed after a
# success and left for a look after a failure.
FETCH=.afkfleet-fetch
# `afkfleet-server agent list`'s header (fleet-server's cli::agent::LIST_HEADER;
# a test holds them equal), its tab and a newline.
TAB=$(printf '\t')
NL='
'
AGENTS_HEADER="id${TAB}name${TAB}state${TAB}bots${TAB}capacity${TAB}version${TAB}last_seen_at${TAB}not_after${TAB}created_at"

# What this run created, for the message after a failure.
CREATED=""
# What this run left for a look, for the message after a failure.
LEFT=""
# keys-restore's staging folder while it exists, removed by refuse too.
STAGE=""

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
	if [ -n "$STAGE" ]; then
		rm -rf "$STAGE"
	fi
	exit 1
}

usage() {
	printf 'usage: sh setup.sh check|init|update|fetch X.Y.Z|restore NAME|forget-aside [STAMP]|renew-cert|keys-backup [DEST]|keys-restore DIR\n' >&2
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
	# keys-restore's `docker cp -a` keeps the server's user (a uid:gid) only
	# from Engine 28.0 on: an older one is found here, at the install, never
	# in the middle of a restore (ADR-0022, U40).
	engine=$(docker version --format '{{.Server.Version}}' 2>/dev/null) || engine=""
	case ${engine%%.*} in
	"" | *[!0-9]*) refuse "Docker Engine's version can't be read ($engine): afkfleet needs 28.0 or newer" ;;
	esac
	[ "${engine%%.*}" -ge 28 ] || refuse "Docker Engine $engine is too old: afkfleet needs 28.0 or newer"
	options=$(docker info --format '{{.SecurityOptions}}' 2>/dev/null || true)
	case $options in
	*rootless* | *userns*)
		refuse "Docker runs rootless or with user namespaces here: the backups' folder can't belong to the server's user (65532) that way"
		;;
	esac
	ok "Docker $engine and Compose $version"
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
	# A remote agent's folder: the agent alone, which check and update take
	# (the runbook's Agents on other hosts); everything else is the server's.
	if [ "$path" = "$AGENT_PATH" ]; then
		[ "${1:-}" = agent-ok ] ||
			refuse "this folder runs a remote agent (COMPOSE_FILE=compose.agent.yaml): only check, fetch and update apply here; the rest runs on the server's host"
		MODE=agent
		PROXY=none
		MOUNTS=$AGENT_MOUNTED
		image AFKFLEET_AGENT_IMAGE agent_image "${AFKFLEET_AGENT_IMAGE:-}"
		AGENT_IMAGE=$IMAGE
		SERVER_IMAGE=""
		ok ".env: a remote agent's folder$extra"
		return 0
	fi
	MODE=server
	MOUNTS=$MOUNTED
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
	for file in $MOUNTS; do
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
	[ -z "$SERVER_IMAGE" ] || check_image "$SERVER_IMAGE"
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

# Nothing of the project may exist yet; with $1 = restored, init also takes
# the state keys-restore and restore leave on a new host (S39): no container,
# and exactly the keys', the CA's, the certificate's and the database's
# volumes, no agent identity. RESTORED says which it found.
check_clean() {
	name=$(project)
	[ -n "$name" ] || refuse "docker compose can't read compose.yaml with this .env"
	containers=$(docker ps -a -q --filter "label=com.docker.compose.project=$name")
	volumes=$(docker volume ls -q --filter "label=com.docker.compose.project=$name")
	RESTORED=no
	if [ -z "$containers" ] && [ -z "$volumes" ]; then
		ok "nothing of the project $name exists yet"
		return 0
	fi
	if [ "${1:-}" = restored ] && [ -z "$containers" ]; then
		found=$(printf '%s\n' "$volumes" | sort | tr '\n' ' ')
		if [ "$found" = "${name}_ca ${name}_data ${name}_keys ${name}_tls " ]; then
			RESTORED=yes
			ok "the project $name holds restored keys, a CA and a database: init makes no key and no PKI"
			return 0
		fi
	fi
	refuse "the project $name has containers or volumes already: init is for a first install only, or a new host keys-restore and restore made ready (an existing install is updated with: sh setup.sh update)"
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
	check_clean restored
	check_backups
	if [ "$RESTORED" = no ]; then
		CREATED="$CREATED the project's volumes"
		docker compose run --rm -T setup vault create-key --config "$SERVER_CONFIG" || refuse "the vault key couldn't be created"
		ok "the vault key, in the keys volume (copy it off this host separately: see the runbook)"
		# shellcheck disable=SC2046 # one name per line, each its own argument
		set -- $(pki_names)
		docker compose run --rm -T setup pki init --config "$SERVER_CONFIG" "$@" || refuse "the PKI couldn't be created"
		ok "the CA and the server's certificate"
	fi
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
	check_env agent-ok
	check_mounted
	check_images
	if [ "$MODE" = agent ]; then
		agents=$(docker compose ps -a -q agent) || refuse "docker compose can't list the agent's container"
		if [ -n "$agents" ]; then
			say "a remote agent is installed here: update it with sh setup.sh update"
		else
			say "no remote agent is installed here yet: enroll it as the runbook's Agents on other hosts says"
		fi
		return 0
	fi
	check_ports
	name=$(project)
	if [ -n "$(docker ps -a -q --filter "label=com.docker.compose.project=$name")" ]; then
		say "the project $name is installed here: update it with sh setup.sh update"
	else
		say "the project $name isn't installed yet: sh setup.sh init installs it"
	fi
}

# A running $2 (server or agent; its container $1) newer than this folder's
# release would be replaced by an older one: refused. RELEASE's own version
# must be one; an image without a release version (a local build) isn't
# compared.
check_running_version() {
	[ -f RELEASE ] || return 0
	release=$(release_value version)
	semver "$release" || refuse "RELEASE's version can't be read ($release): fetch the release again (sh setup.sh fetch <version>); nothing was changed"
	running=$(docker inspect --format '{{index .Config.Labels "org.opencontainers.image.version"}}' "$1") ||
		refuse "the running $2's version label can't be read: nothing was changed"
	if [ -z "$running" ]; then
		say "the running $2 has no release version (a local build): not compared"
		return 0
	fi
	semver "$running" || refuse "the running $2's version label isn't a version ($running): nothing was changed"
	vercmp "$running" "$release"
	[ "$CMP" -le 0 ] ||
		refuse "the running $2 is $running, newer than this folder's release $release: fetch it (sh setup.sh fetch $running), or restore a backup; nothing was changed"
}

# update in a remote agent's folder: no backup (the server's host keeps the
# database), the same refusals for a fetch that didn't finish and a newer
# running agent, then its new image and the agent started.
update_agent() {
	installed=$(docker compose ps -a -q agent) || refuse "docker compose can't list the agent's container: nothing was changed"
	[ -n "$installed" ] || refuse "the agent isn't installed here: enroll it first, as the runbook's Agents on other hosts says"
	id=$(docker compose ps -q agent) || refuse "docker compose can't list the agent's container: nothing was changed"
	[ -z "$id" ] || check_running_version "$id" agent
	docker compose pull || refuse "the new image couldn't be pulled; nothing changed"
	docker compose up --detach --wait || refuse "the agent didn't come up healthy: docker compose logs agent says why"
	ok "the agent is updated"
}

update() {
	[ ! -e "$FETCH" ] || refuse "$FETCH is here: a fetch didn't finish; look inside, remove it (rm -r $FETCH) and fetch again before updating"
	check_docker
	check_env agent-ok
	if [ "$MODE" = agent ]; then
		update_agent
		return 0
	fi
	id=$(docker compose ps -q server)
	[ -n "$id" ] || refuse "the server isn't running: an update starts from a running, healthy afkfleet"
	health=$(docker inspect --format '{{.State.Health.Status}}' "$id")
	[ "$health" = healthy ] || refuse "the server isn't healthy ($health): fix that before updating"
	check_running_version "$id" server
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

# Whether $1 is a stamp, YYYYMMDDTHHMMSSZ, as files.sh takes one.
stamp() {
	case $1 in
	[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]T[0-9][0-9][0-9][0-9][0-9][0-9]Z) return 0 ;;
	esac
	return 1
}

# Now in UTC, as a stamp.
stamp_now() {
	date -u +%Y%m%dT%H%M%SZ
}

# The project's volume $1 must exist: files and maintenance never make one.
need_volume() {
	name=$(project)
	[ -n "$name" ] || refuse "docker compose can't read compose.yaml with this .env"
	docker volume inspect "${name}_$1" >/dev/null 2>&1 ||
		refuse "the volume ${name}_$1 isn't here: on a new host, keys-restore comes first (the runbook's Restoring a backup)"
}

# Whether the server runs and is healthy.
server_healthy() {
	id=$(docker compose ps -q server)
	[ -n "$id" ] || return 1
	[ "$(docker inspect --format '{{.State.Health.Status}}' "$id")" = healthy ]
}

# The state the agent list $1 (`agent list`'s lines) gives the agent $2.
agent_state() {
	while IFS=$TAB read -r id _ state _ || [ -n "$id" ]; do
		if [ "$id" = "$2" ]; then
			printf '%s' "$state"
			return 0
		fi
	done <<EOF
$1
EOF
}

# The agents $1 (an `agent list` from before the restore) has disabled and
# $2 (the restored one) has enabled, one ID per line.
disabled_since() {
	while IFS=$TAB read -r id _ state _ || [ -n "$id" ]; do
		[ "$state" = disabled ] || continue
		case $id in
		"" | *[!0-9a-f-]*) continue ;;
		esac
		[ "$(agent_state "$2" "$id")" != enabled ] || printf '%s\n' "$id"
	done <<EOF
$1
EOF
}

# restore NAME (ADR-0022, S8, S40): the backup NAME, from the backups'
# folder, as the database. The old database, its -wal and -shm move aside,
# never deleted; then migrate, vault status and the agents, each through the
# maintenance service with the server stopped. With a healthy server first,
# every agent disabled since the backup is disabled again and afkfleet
# starts; after a crash or on a new host it ends stopped and says what's next.
restore() {
	backup=${1#/backups/}
	case $backup in
	afkfleet-backup-*.db) inner=${backup#afkfleet-backup-} ;;
	*) inner="" ;;
	esac
	stamp "${inner%.db}" || refuse "$1 isn't a backup's name: afkfleet-backup-YYYYMMDDTHHMMSSZ.db, as the backups' folder lists it"
	[ ! -e "$FETCH" ] || refuse "$FETCH is here: a fetch didn't finish; look inside, remove it (rm -r $FETCH), then restore"
	check_docker
	check_env
	need_volume keys
	need_volume tls
	saved=""
	if server_healthy; then
		saved=$(docker compose exec -T server /usr/local/bin/afkfleet-server agent list --config "$SERVER_CONFIG") ||
			refuse "the server's agents can't be listed: nothing was changed"
		[ "${saved%%"$NL"*}" = "$AGENTS_HEADER" ] || refuse "the server's agent list can't be read: nothing was changed"
	fi
	docker compose stop agent server || refuse "the agent and the server couldn't be stopped: nothing was restored"
	docker compose run --rm -T maintenance --version >/dev/null || refuse "the server's image can't run here: nothing was restored"
	when=$(stamp_now)
	docker compose run --rm -T files restore-in "$when" "$backup" ||
		refuse "$backup couldn't be put in place: the database is as it was, or aside under $when (sh setup.sh forget-aside lists it)"
	ok "$backup is the database; the old one is aside under $when"
	docker compose run --rm -T maintenance migrate --config "$SERVER_CONFIG" ||
		refuse "the backup is from a newer server than this one, or can't be read: the server stays stopped; put the old database back with: docker compose run --rm files put-back $when"
	status=0
	docker compose run --rm -T maintenance vault status --config "$SERVER_CONFIG" || status=$?
	case $status in
	0) ok "every secret opens with this host's vault keys" ;;
	3) refuse "the backup's secrets need a vault key this host doesn't configure (vault status above says which): the server stays stopped; put that key back (the runbook's Rotating the vault key), or the old database with: docker compose run --rm files put-back $when" ;;
	*) refuse "vault status failed: the server stays stopped" ;;
	esac
	restored=$(docker compose run --rm -T maintenance agent list --config "$SERVER_CONFIG") ||
		refuse "the restored agents can't be listed: the server stays stopped"
	printf '%s\n' "$restored"
	if [ -z "$saved" ]; then
		say ""
		say "The server is stopped. Before you start it, disable every agent you disabled since this backup was taken (it has them enabled):"
		say "  docker compose run --rm maintenance agent disable --config $SERVER_CONFIG --id <its id>"
		say "Then start afkfleet: docker compose up --detach --wait"
	else
		for id in $(disabled_since "$saved" "$restored"); do
			docker compose run --rm -T maintenance agent disable --config "$SERVER_CONFIG" --id "$id" ||
				refuse "the agent $id couldn't be disabled again: the server stays stopped; disable it, then start afkfleet (docker compose up --detach --wait)"
			ok "the agent $id is disabled again, as it was before the restore"
		done
		docker compose up --detach --wait || refuse "afkfleet didn't come up healthy: docker compose ps and docker compose logs say why"
		ok "afkfleet runs on $backup"
	fi
	say ""
	say "In the app, redo what was done after the backup was taken: disable users again, end sessions and revoke grants again; whoever changed their password or 2FA since must change it once more."
	say "Once all is well: sh setup.sh forget-aside $when"
}

# forget-aside [STAMP] (S40): without a stamp, the aside files listed; with
# one, exactly that stamp's deleted, only while the server is healthy.
forget_aside() {
	if [ $# -eq 1 ]; then
		stamp "$1" || refuse "$1 isn't a stamp: YYYYMMDDTHHMMSSZ, as restore and renew-cert printed it"
	fi
	check_docker
	check_env
	if [ $# -eq 0 ]; then
		docker compose run --rm -T files list || refuse "the aside files can't be listed"
		return 0
	fi
	server_healthy || refuse "the server isn't running and healthy: check the restore or the renewal first, then forget what's aside"
	docker compose run --rm -T files forget "$1" || refuse "what's aside under $1 couldn't be deleted"
	ok "what was aside under $1 is gone"
}

# renew-cert (S41): the server's certificate renewed from the same CA, for
# `server` and AFKFLEET_GRPC_NAMES' names, so no agent enrolls again; the old
# pair moved aside first, and back when the new one fails.
renew_cert() {
	check_docker
	check_env
	[ -n "$(docker compose ps -a -q server)" ] ||
		refuse "the server has no container here: renew-cert renews an installed afkfleet's certificate"
	need_volume tls
	docker compose stop server || refuse "the server couldn't be stopped: nothing changed"
	when=$(stamp_now)
	if ! docker compose run --rm -T files tls-aside "$when"; then
		docker compose up --detach --wait server || true
		refuse "the old certificate couldn't be moved aside: nothing changed"
	fi
	# shellcheck disable=SC2046 # one name per line, each its own argument
	set -- $(pki_names)
	if ! docker compose run --rm -T setup pki server-cert --config "$SERVER_CONFIG" "$@"; then
		docker compose run --rm -T files tls-back "$when" ||
			refuse "a new certificate couldn't be made, and the old one is still aside under $when: docker compose run --rm files tls-back $when"
		docker compose up --detach --wait server || true
		refuse "a new certificate couldn't be made; the old certificate is back and the server started"
	fi
	docker compose up --detach --wait || refuse "afkfleet didn't come up healthy: docker compose logs server says why"
	ok "the server's certificate is new, from the same CA: no agent enrolls again"
	say "Once the agents are back online: sh setup.sh forget-aside $when"
}

# Whether every folder above $1 that exists belongs to root and lets no
# group or other write, up to /.
root_path() {
	path=$1
	while [ "$path" != / ]; do
		path=$(dirname "$path")
		[ -d "$path" ] || continue
		info=$(stat -c '%u:%g %a' "$path")
		[ "${info%%:*}" = 0 ] || return 1
		! others_write "${info##* }" || return 1
	done
	return 0
}

# The keys' copy $1 (R22): the folder, its keys/ and its ca/ root's with mode
# 700; in them only ca.key and vault-key-<n>, and ca.crt, each a regular
# file of root's with mode 600; and the CA's pair and the active vault key
# there.
check_copy() {
	for folder in "$1" "$1/keys" "$1/ca"; do
		if [ -L "$folder" ] || [ ! -d "$folder" ]; then
			refuse "$folder isn't a folder: a keys' copy holds keys/ and ca/"
		fi
		info=$(stat -c '%u:%g %a' "$folder")
		[ "$info" = "0:0 700" ] || refuse "${folder#"$1"/} must belong to root with mode 700, not $info"
	done
	for entry in "$1"/* "$1"/.[!.]* "$1"/keys/* "$1"/keys/.[!.]* "$1"/ca/* "$1"/ca/.[!.]*; do
		if [ ! -e "$entry" ] && [ ! -L "$entry" ]; then
			continue
		fi
		relative=${entry#"$1"/}
		case $relative in
		keys | ca) continue ;;
		keys/ca.key | ca/ca.crt) ;;
		keys/vault-key-*)
			case ${relative#keys/vault-key-} in
			"" | *[!0-9]*) refuse "$relative isn't a file of the keys' copy" ;;
			esac
			;;
		*) refuse "$relative isn't a file of the keys' copy: it holds keys/ca.key, keys/vault-key-<n> and ca/ca.crt alone" ;;
		esac
		if [ -L "$entry" ] || [ ! -f "$entry" ]; then
			refuse "$relative isn't a regular file"
		fi
		info=$(stat -c '%u:%g %a' "$entry")
		[ "$info" = "0:0 600" ] || refuse "$relative must belong to root with mode 600, not $info"
	done
	active=$(env_value AFKFLEET_SERVER__VAULT__ACTIVE_KEY_ID)
	[ -n "$active" ] || active=1
	for need in keys/ca.key "keys/vault-key-$active" ca/ca.crt; do
		[ -f "$1/$need" ] || refuse "the keys' copy $1 has no $need"
	done
}

# The names in the keys' copy $1, on one line.
copy_names() {
	names=""
	for entry in "$1"/keys/* "$1"/ca/*; do
		[ -f "$entry" ] && names="$names ${entry#"$1"/}"
	done
	printf '%s' "${names# }"
}

# keys-backup [DEST] (ADR-0022, U22, S39, R22): the keys volume's vault keys
# and CA key, and the CA's certificate, copied from the server's container
# into DEST (/root/afkfleet-keys-<stamp> by default), a root-only folder of
# root-only files that must not exist yet, in folders only root can change,
# never in the backups' SFTP folder. It prints names, never a key.
keys_backup() {
	[ "$(id -u)" = 0 ] || refuse "keys-backup runs as root (sudo sh setup.sh keys-backup): the copy is root's alone"
	check_docker
	check_env
	dest=${1:-/root/afkfleet-keys-$(stamp_now)}
	backups_path "$dest" || refuse "$dest must be an absolute path of letters, digits and ._-, without //, . or .. parts or a trailing /"
	if [ -e "$dest" ] || [ -L "$dest" ]; then
		refuse "$dest exists already: keys-backup never writes over anything"
	fi
	case $dest/ in
	"$(dirname "$BACKUPS")"/*) refuse "$dest is inside the backups' SFTP folder, which the backups' account reads: the keys never go there" ;;
	esac
	root_path "$dest" || refuse "every folder above $dest must belong to root and let no one else write: only root may reach the copy"
	server=$(docker compose ps -a -q server)
	[ -n "$server" ] || refuse "the server has no container here: keys-backup copies an installed afkfleet's keys"
	mkdir -m 700 "$dest" "$dest/keys" "$dest/ca" || refuse "$dest couldn't be made"
	LEFT="the copy so far is in $dest: remove it (rm -r $dest) before you try again"
	docker compose cp server:/keys/. "$dest/keys" || refuse "the keys couldn't be copied"
	docker compose cp server:/ca/. "$dest/ca" || refuse "the CA's certificate couldn't be copied"
	check_copy "$dest"
	LEFT=""
	ok "the keys' copy is in $dest: $(copy_names "$dest")"
	say "Copy it off this host as the runbook's The keys' copy says, and keep it offline, apart from the backups; never print its files."
}

# keys-restore DIR (S39, R30): on a new host, before init, the keys' copy
# DIR into new keys and ca volumes as the server's user, owner-only, then a
# server certificate from that CA for this host's names. The copy is staged
# in a root-only folder of setup.sh's own, owned by the server's user, so
# `docker cp -a` gives it that user however the Engine reads -a; the stage is
# removed after the copy, and on a failure.
keys_restore() {
	[ "$(id -u)" = 0 ] || refuse "keys-restore runs as root (sudo sh setup.sh keys-restore <folder>): the copy is root's alone"
	backups_path "$1" || refuse "$1 must be an absolute path of letters, digits and ._-, without //, . or .. parts or a trailing /"
	check_docker
	check_env
	check_mounted
	check_images
	if [ "$PROXY" = caddy ]; then
		check_ports caddy
	fi
	check_clean
	check_backups
	check_copy "$1"
	stage=.afkfleet-keys-restore
	if [ -e "$stage" ] || [ -L "$stage" ]; then
		refuse "$stage exists: an earlier keys-restore stopped there; remove it (rm -r $stage) and run again"
	fi
	mkdir -m 700 "$stage" || refuse "$stage couldn't be made"
	STAGE=$stage
	mkdir -m 700 "$stage/keys" "$stage/ca" || refuse "$stage couldn't be made"
	for entry in "$1"/keys/* "$1"/ca/*; do
		[ -f "$entry" ] || continue
		relative=${entry#"$1"/}
		cp "$entry" "$stage/$relative" || refuse "$relative couldn't be staged"
		chown 65532:65532 "$stage/$relative" || refuse "$relative couldn't be given to the server's user"
		chmod 600 "$stage/$relative" || refuse "$relative couldn't be made owner-only"
	done
	CREATED="$CREATED the project's volumes"
	docker compose --profile setup create setup || refuse "the setup service couldn't be created"
	container=$(docker compose ps -a -q setup)
	[ -n "$container" ] || refuse "the setup service's container isn't there"
	docker cp -a "$stage/keys/." "$container:/keys" || refuse "the keys couldn't be copied into their volume"
	docker cp -a "$stage/ca/." "$container:/ca" || refuse "the CA's certificate couldn't be copied into its volume"
	docker rm -f "$container" >/dev/null || refuse "the setup service's container couldn't be removed"
	rm -rf "$stage"
	STAGE=""
	ok "the keys and the CA are in this host's volumes, as the server's user"
	# shellcheck disable=SC2046 # one name per line, each its own argument
	set -- $(pki_names)
	docker compose run --rm -T setup pki server-cert --config "$SERVER_CONFIG" "$@" ||
		refuse "a server certificate couldn't be made from the restored CA"
	ok "a server certificate from the same CA: agents keep their identities"
	say ""
	say "Next, as the runbook's Restoring a backup says:"
	say "  1. Put the backup in the backups' folder as the server's user: sudo install -o 65532 -g 65532 -m 600 <the backup> <the backups' folder>/"
	say "  2. sh setup.sh restore <its name>"
	say "  3. Disable the old host's local agent: docker compose run --rm maintenance agent disable --config $SERVER_CONFIG --id <its id>"
	say "  4. sudo sh setup.sh init"
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
check | init | update | renew-cert) [ $# -eq 1 ] || usage ;;
fetch | restore | keys-restore) [ $# -eq 2 ] || usage ;;
forget-aside | keys-backup) [ $# -le 2 ] || usage ;;
*) usage ;;
esac
cd "$(dirname "$0")" || exit 1
case $1 in
check) check ;;
init) init ;;
update) update ;;
fetch) fetch "$2" ;;
restore) restore "$2" ;;
forget-aside)
	shift
	forget_aside "$@"
	;;
renew-cert) renew_cert ;;
keys-backup)
	shift
	keys_backup "$@"
	;;
keys-restore) keys_restore "$2" ;;
esac
