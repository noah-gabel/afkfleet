#!/bin/sh
# afkfleet's setup script (Plan.md P12.2; ADR-0022, S18, U8), run on the host
# from the folder it lives in, beside compose.yaml and the host's .env:
#
#   sh setup.sh check     what's there and which proxy path fits; changes nothing
#   sh setup.sh init      a first install's one-time steps, then afkfleet started
#   sh setup.sh update    a backup with the running server, then the new images
#
# Exit codes: 0 done, 1 refused or failed (it says why), 2 usage error.
#
# It needs only Docker with Compose v2.24 or newer. It reads .env line by line
# and never sources it. Every key is made inside the server's image, as the
# image's own user (65532), into the project's volumes, and never printed:
# the vault key, the CA and the server's certificate. Root is needed only to
# create the backups' folder, which must belong to that same user so the
# server can write it and the SFTP account can read it (U3). It never
# overwrites anything, and on a failure it names what this run created,
# deleting nothing itself.
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

# What this run created, for the message after a failure.
CREATED=""

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
	exit 1
}

usage() {
	printf 'usage: sh setup.sh check|init|update\n' >&2
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
	SERVER_IMAGE=$(env_value AFKFLEET_SERVER_IMAGE)
	AGENT_IMAGE=$(env_value AFKFLEET_AGENT_IMAGE)
	plain "$SERVER_IMAGE" || refuse "AFKFLEET_SERVER_IMAGE in .env is missing or holds a character it can't"
	plain "$AGENT_IMAGE" || refuse "AFKFLEET_AGENT_IMAGE in .env is missing or holds a character it can't"
	if [ "$PROXY" = caddy ]; then
		domain=$(env_value AFKFLEET_DOMAIN)
		plain "$domain" || refuse "AFKFLEET_DOMAIN in .env is missing or isn't a plain host name"
		case $domain in
		*example.com | *example.org | *example.net) refuse "AFKFLEET_DOMAIN in .env is still env.example's placeholder" ;;
		esac
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
	fi
	BACKUPS=$(env_value AFKFLEET_BACKUPS_DIR)
	[ -n "$BACKUPS" ] || BACKUPS=$DEFAULT_BACKUPS
	case $BACKUPS in
	/*) ;;
	*) refuse "AFKFLEET_BACKUPS_DIR in .env must be an absolute path" ;;
	esac
	case $BACKUPS in
	*[!A-Za-z0-9./_-]*) refuse "AFKFLEET_BACKUPS_DIR in .env holds a character it can't" ;;
	esac
	ok ".env: the $PROXY proxy path$extra"
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
# only root can change (ChrootDirectory's rule for the SFTP account).
check_backups() {
	parent=$(dirname "$BACKUPS")
	if [ -d "$BACKUPS" ]; then
		owner=$(stat -c '%u:%g %a' "$BACKUPS")
		[ "$owner" = "$NONROOT:$NONROOT 700" ] || refuse "$BACKUPS must belong to $NONROOT:$NONROOT with mode 700, not $owner"
		ok "the backups' folder $BACKUPS"
		return 0
	fi
	if [ "$(id -u)" != 0 ]; then
		refuse "$BACKUPS doesn't exist, and only root can create it for the server's user: run sh setup.sh init with sudo, or create it with: sudo install -d -o root -g root -m 755 $parent && sudo install -d -o $NONROOT -g $NONROOT -m 700 $BACKUPS"
	fi
	say "creating $BACKUPS as root: the server's user ($NONROOT) must own it to write the backups"
	install -d -o root -g root -m 755 "$parent"
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
	check_images
	check_ports
	name=$(project)
	if [ -n "$(docker ps -a -q --filter "label=com.docker.compose.project=$name")" ]; then
		say "the project $name is installed here: update it with sh setup.sh update"
	else
		say "the project $name isn't installed yet: sh setup.sh init installs it"
	fi
}

update() {
	check_docker
	check_env
	id=$(docker compose ps -q server)
	[ -n "$id" ] || refuse "the server isn't running: an update starts from a running, healthy afkfleet"
	health=$(docker inspect --format '{{.State.Health.Status}}' "$id")
	[ "$health" = healthy ] || refuse "the server isn't healthy ($health): fix that before updating"
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

[ $# -eq 1 ] || usage
cd "$(dirname "$0")" || exit 1
case $1 in
check) check ;;
init) init ;;
update) update ;;
*) usage ;;
esac
