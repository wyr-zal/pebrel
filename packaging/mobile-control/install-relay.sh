#!/bin/sh
set -eu

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

usage() {
    printf 'Usage: sh install-relay.sh https://relay.example.com\n' >&2
    exit 2
}

[ "$#" -eq 1 ] || usage
public_url=${1%/}
case "$public_url" in
    https://*) ;;
    *) fail "Relay URL must use https://" ;;
esac
origin=${public_url#https://}
[ -n "$origin" ] || fail "Relay URL has no host"
case "$origin" in
    */*|*\?*|*#*|*" "*) fail "Relay URL must be an origin without a path, query, fragment or spaces" ;;
    *:*) fail "Custom ports are not supported; publish HTTPS on port 443" ;;
    *[!A-Za-z0-9.-]*) fail "Relay host must be an ASCII DNS name or IPv4 address" ;;
    -*|.*|*-|*.|*..*) fail "Relay host has an invalid DNS label" ;;
esac

for command_name in docker curl awk head base64 tr; do
    command -v "$command_name" >/dev/null 2>&1 || fail "missing command: $command_name"
done
docker compose version >/dev/null 2>&1 || fail "Docker Compose v2 is required"
[ -f compose.pebrel.yml ] || fail "compose.pebrel.yml is missing; run this script from the extracted Relay directory"
[ -f .env.example ] || fail ".env.example is missing; the Relay package is incomplete"
[ ! -e .env ] || fail ".env already exists; refusing to replace its credentials"

umask 077
secret=$(head -c 48 /dev/urandom | base64 | tr -d '\n')
[ "${#secret}" -ge 43 ] || fail "could not generate JWT_SECRET"

awk -v secret="$secret" -v public_url="$public_url" '
    /^JWT_SECRET=/ {
        print "JWT_SECRET=" secret
        next
    }
    /^DATABASE_PATH=/ {
        print "DATABASE_PATH=/data/relay.sqlite"
        next
    }
    /^PUBLIC_RELAY_URL=/ {
        print "PUBLIC_RELAY_URL=" public_url
        next
    }
    /^ALLOW_LEGACY_WEB_PROXY=/ {
        print "ALLOW_LEGACY_WEB_PROXY=0"
        next
    }
    /^SECURE_HANDSHAKE_TIMEOUT_MS=/ {
        print "SECURE_HANDSHAKE_TIMEOUT_MS=130000"
        next
    }
    { print }
' .env.example > .env
chmod 600 .env

cat > Caddyfile.generated <<EOF
$origin {
    reverse_proxy 127.0.0.1:8787
}
EOF

docker compose --env-file .env -f compose.pebrel.yml up -d --build

attempt=0
while [ "$attempt" -lt 30 ]; do
    if curl --noproxy '*' --fail --silent --show-error --max-time 3 \
        http://127.0.0.1:8787/health >/dev/null 2>&1; then
        printf 'Relay is healthy at %s\n' "$public_url"
        printf 'Phone entry: %s/app/\n' "$public_url"
        printf 'Reverse proxy template: %s/Caddyfile.generated\n' "$PWD"
        printf 'Keep port 8787 on loopback; publish only HTTPS through your proxy.\n'
        exit 0
    fi
    attempt=$((attempt + 1))
    sleep 2
done

docker compose --env-file .env -f compose.pebrel.yml logs --tail=80 relay >&2 || true
fail "Relay did not become healthy; inspect the container logs above"
