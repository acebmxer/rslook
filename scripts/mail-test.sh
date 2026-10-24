#!/bin/sh
# End-to-end Send/Receive tests (crates/sync/tests/mail_servers.rs) against throwaway servers:
#   GreenMail  IMAP and SMTP, plain and implicit TLS, with test users
#   Dovecot    IMAP with STARTTLS
#   Mailpit    SMTP with STARTTLS and login
# Synthetic mail and test users only; nothing leaves this machine. Needs podman.
#   scripts/mail-test.sh
# Optional: RSLOOK_DEV_IMAGE (Rust toolchain image, default docker.io/library/rust:latest),
#           RSLOOK_CARGO_VOLUME / RSLOOK_TARGET_VOLUME (named volumes to reuse build caches).
set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
DEV_IMAGE=${RSLOOK_DEV_IMAGE:-docker.io/library/rust:latest}
GM_IMAGE=docker.io/greenmail/standalone:2.1.8
DC_IMAGE=docker.io/dovecot/dovecot:2.4.5
MP_IMAGE=docker.io/axllent/mailpit:latest
RUN=rslook-mailtest-$$
WORK=$(mktemp -d)
cleanup() {
    podman rm -f "$RUN-greenmail" "$RUN-dovecot" "$RUN-mailpit" >/dev/null 2>&1 || true
    podman network rm -f "$RUN" >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

# A private test CA and a server certificate for the test host names, made fresh for each run.
podman run --rm --security-opt label=disable -v "$WORK":/w -w /w "$DEV_IMAGE" sh -ec '
    openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=Rslook test CA" \
        -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign \
        -keyout ca.key -out ca.crt 2>/dev/null
    openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout server.key -out server.csr 2>/dev/null
    printf "subjectAltName=DNS:localhost,DNS:greenmail,DNS:dovecot,DNS:mailpit,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n" > ext.cnf
    openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 2 -extfile ext.cnf -out server.crt 2>/dev/null
    openssl pkcs12 -export -inkey server.key -in server.crt -certfile ca.crt -name greenmail \
        -passout pass:changeit -out greenmail.p12
    printf "start:pw-start\n" > mailpit-auth
    chmod 644 greenmail.p12 ca.crt server.crt server.key mailpit-auth'

podman network create "$RUN" >/dev/null
USERS=flow:pw-flow@example.test,offline:pw-offline@example.test,tls:pw-tls@example.test,bob:pw-bob@example.test
podman run -d --name "$RUN-greenmail" --network "$RUN" --network-alias greenmail --security-opt label=disable \
    -v "$WORK/greenmail.p12":/home/greenmail/test.p12:ro \
    -e GREENMAIL_OPTS="-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.users=$USERS -Dgreenmail.tls.keystore.file=/home/greenmail/test.p12 -Dgreenmail.tls.keystore.password=changeit" \
    "$GM_IMAGE" >/dev/null
podman run -d --name "$RUN-dovecot" --network "$RUN" --network-alias dovecot --security-opt label=disable \
    -v "$WORK/server.crt":/etc/dovecot/ssl/tls.crt:ro -v "$WORK/server.key":/etc/dovecot/ssl/tls.key:ro \
    -e USER_PASSWORD=pw-start "$DC_IMAGE" >/dev/null
podman run -d --name "$RUN-mailpit" --network "$RUN" --network-alias mailpit --security-opt label=disable \
    -v "$WORK":/certs:ro "$MP_IMAGE" --smtp-tls-cert /certs/server.crt --smtp-tls-key /certs/server.key \
    --smtp-require-starttls --smtp-auth-file /certs/mailpit-auth >/dev/null

wait_for() { # container, text its log shows once it is ready
    i=0
    until podman logs "$1" 2>&1 | grep -q "$2"; do
        i=$((i + 1))
        [ "$i" -gt 60 ] && { podman logs "$1"; echo "$1 did not start" >&2; exit 1; }
        sleep 1
    done
}
wait_for "$RUN-greenmail" "Starting GreenMail standalone"
wait_for "$RUN-dovecot" "starting up"
wait_for "$RUN-mailpit" "accessible via"
sleep 3

podman run --rm --network "$RUN" --security-opt label=disable \
    -v "$REPO":/src:ro -v "$WORK/ca.crt":/ca.crt:ro \
    ${RSLOOK_CARGO_VOLUME:+-v "$RSLOOK_CARGO_VOLUME":/usr/local/cargo/registry} \
    ${RSLOOK_TARGET_VOLUME:+-v "$RSLOOK_TARGET_VOLUME":/target} \
    -e CARGO_TARGET_DIR=/target -e RSLOOK_TEST_CA=/ca.crt \
    -e RSLOOK_GM_HOST=greenmail -e RSLOOK_DC_HOST=dovecot -e RSLOOK_MP_HOST=mailpit \
    -w /src "$DEV_IMAGE" \
    cargo test -p rslook-sync --test mail_servers -- --ignored --test-threads=1
