#!/usr/bin/env bash
#
# Daily signed Postgres backup -> Netcup SFTP. Native (no docker).
#
# Runs as the dedicated `alertbot-backup` system user. Its matching PostgreSQL
# role has read-only grants for pg_dump and cannot assume the application
# owner role or read the bot configuration.
#
# Required env (provided by root-owned /etc/alertbot-backup.env):
#   BACKUP_NETCUP_HOST    e.g. yourhost.netcup.net
#   BACKUP_NETCUP_USER    SFTP username
#   BACKUP_NETCUP_PASS    SFTP password
#   BACKUP_SFTP_HOSTKEY_SHA256  base64 SHA-256 host-key hash, without SHA256:
#
# Required systemd credential:
#   backup-signing-key    RSA private key
#
# Optional env:
#   BACKUP_NETCUP_PATH    remote directory (default: backups/alertbot)
#   BACKUP_KNOWN_HOSTS    pinned SFTP host keys (default below)
#   DB_NAME               postgres database to dump (default: alertbot)
#   RETENTION_DAYS        prune remote dumps older than N days (default: 30)
#
# Required packages on the host: postgresql-client, curl, openssl, sshpass

set -euo pipefail

: "${BACKUP_NETCUP_HOST:?must be set}"
: "${BACKUP_NETCUP_USER:?must be set}"
: "${BACKUP_NETCUP_PASS:?must be set}"
: "${BACKUP_SFTP_HOSTKEY_SHA256:?must be set}"
: "${BACKUP_NETCUP_PATH:=backups/alertbot}"
: "${BACKUP_KNOWN_HOSTS:=/etc/alertbot-backup-known-hosts}"
: "${DB_NAME:=alertbot}"
: "${RETENTION_DAYS:=30}"
: "${CREDENTIALS_DIRECTORY:?must be run by the systemd backup unit}"

SIGNING_KEY="${CREDENTIALS_DIRECTORY}/backup-signing-key"

[[ "$BACKUP_NETCUP_HOST" =~ ^[A-Za-z0-9][A-Za-z0-9.-]*$ ]] \
    || { echo "invalid BACKUP_NETCUP_HOST" >&2; exit 2; }
[[ "$BACKUP_NETCUP_USER" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] \
    || { echo "invalid BACKUP_NETCUP_USER" >&2; exit 2; }
[[ "$BACKUP_SFTP_HOSTKEY_SHA256" =~ ^[A-Za-z0-9+/]{43}$ ]] \
    || { echo "invalid BACKUP_SFTP_HOSTKEY_SHA256" >&2; exit 2; }
[[ "$BACKUP_NETCUP_PATH" =~ ^[A-Za-z0-9._/-]+$ ]] \
    && [[ "/$BACKUP_NETCUP_PATH/" != *"/../"* ]] \
    && [[ "$BACKUP_NETCUP_PATH" != /* ]] \
    || { echo "invalid BACKUP_NETCUP_PATH" >&2; exit 2; }
[[ "$DB_NAME" =~ ^[A-Za-z_][A-Za-z0-9_-]*$ ]] \
    || { echo "invalid DB_NAME" >&2; exit 2; }
[[ "$RETENTION_DAYS" =~ ^[1-9][0-9]*$ ]] && (( RETENTION_DAYS <= 3650 )) \
    || { echo "RETENTION_DAYS must be between 1 and 3650" >&2; exit 2; }
[[ -r "$BACKUP_KNOWN_HOSTS" ]] \
    || { echo "BACKUP_KNOWN_HOSTS is not readable" >&2; exit 2; }
[[ -r "$SIGNING_KEY" ]] \
    || { echo "backup-signing-key credential is not readable" >&2; exit 2; }
CURL_HELP=$(curl --help all 2>/dev/null) \
    || { echo "cannot inspect curl features" >&2; exit 2; }
[[ "$CURL_HELP" == *"--hostpubsha256"* ]] \
    || { echo "curl lacks --hostpubsha256 support" >&2; exit 2; }
unset CURL_HELP

umask 077
WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/alertbot-backup.XXXXXX")
trap 'rm -rf -- "$WORK_DIR"' EXIT

escape_curl_config() {
    local value=$1
    value=${value//\\/\\\\}
    value=${value//\"/\\\"}
    value=${value//$'\n'/\\n}
    value=${value//$'\r'/\\r}
    printf '%s' "$value"
}

CURL_CONFIG="${WORK_DIR}/curl.conf"
printf 'user = "%s:%s"\n' \
    "$(escape_curl_config "$BACKUP_NETCUP_USER")" \
    "$(escape_curl_config "$BACKUP_NETCUP_PASS")" >"$CURL_CONFIG"

SFTP_PASSWORD=$BACKUP_NETCUP_PASS
unset BACKUP_NETCUP_PASS

CURL_COMMON=(
    --config "$CURL_CONFIG"
    --silent --show-error --fail
    --connect-timeout 15 --max-time 1800
    --hostpubsha256 "$BACKUP_SFTP_HOSTKEY_SHA256"
)

SFTP_COMMON=(
    -q
    -o "UserKnownHostsFile=${BACKUP_KNOWN_HOSTS}"
    -o StrictHostKeyChecking=yes
    -o BatchMode=no
    -o ConnectTimeout=15
    -o ConnectionAttempts=1
    -o ServerAliveInterval=15
    -o ServerAliveCountMax=3
)

DATE=$(date -u +%Y-%m-%dT%H%M%SZ)
FILE_NAME="alertbot-${DATE}.dump"
REMOTE_FILE="${BACKUP_NETCUP_PATH}/${FILE_NAME}"
REMOTE_SIGNATURE="${REMOTE_FILE}.sig"
DUMP_FILE="${WORK_DIR}/${FILE_NAME}"
SIGNATURE_FILE="${DUMP_FILE}.sig"

echo "[backup] dumping ${DB_NAME} -> ${REMOTE_FILE}"

pg_dump --format=custom --no-owner --no-privileges \
    --file="$DUMP_FILE" "$DB_NAME"
openssl dgst -sha256 -sign "$SIGNING_KEY" \
    -out "$SIGNATURE_FILE" "$DUMP_FILE"

# Publish the signature first and the dump last. Therefore a final dump name
# is visible only after both complete files have reached the SFTP server.
for local_file in "$DUMP_FILE" "$SIGNATURE_FILE"; do
    remote_file="$REMOTE_FILE"
    [[ "$local_file" == "$SIGNATURE_FILE" ]] && remote_file="$REMOTE_SIGNATURE"
    curl "${CURL_COMMON[@]}" \
         --upload-file "$local_file" \
         --ftp-create-dirs \
         "sftp://${BACKUP_NETCUP_HOST}/${remote_file}.part"
done

for remote_file in "$REMOTE_SIGNATURE" "$REMOTE_FILE"; do
    curl "${CURL_COMMON[@]}" \
         --quote "rename ${remote_file}.part ${remote_file}" \
         --output /dev/null \
         "sftp://${BACKUP_NETCUP_HOST}/${BACKUP_NETCUP_PATH}/"
done

echo "[backup] uploaded signed dump"

CUTOFF_EPOCH=$(date -u -d "${RETENTION_DAYS} days ago" +%s 2>/dev/null \
              || date -u -v-"${RETENTION_DAYS}d" +%s)

LIST_BATCH="${WORK_DIR}/list.batch"
LIST_OUTPUT="${WORK_DIR}/list.output"
printf 'cd %s\nls -1\nbye\n' "$BACKUP_NETCUP_PATH" >"$LIST_BATCH"
SSHPASS="$SFTP_PASSWORD" sshpass -e sftp "${SFTP_COMMON[@]}" -b "$LIST_BATCH" \
    "${BACKUP_NETCUP_USER}@${BACKUP_NETCUP_HOST}" >"$LIST_OUTPUT"

DELETE_BATCH="${WORK_DIR}/delete.batch"
printf 'cd %s\n' "$BACKUP_NETCUP_PATH" >"$DELETE_BATCH"
DELETE_COUNT=0

while IFS= read -r file_name; do
    timestamp=${file_name#alertbot-}
    timestamp=${timestamp%%.dump*}
    file_time="${timestamp:0:10} ${timestamp:11:2}:${timestamp:13:2}:${timestamp:15:2}"
    file_epoch=$(date -u -d "$file_time" +%s 2>/dev/null \
                 || date -u -j -f '%Y-%m-%d %H:%M:%S' "$file_time" +%s 2>/dev/null \
                 || true)
    if [[ -n "$file_epoch" && "$file_epoch" -lt "$CUTOFF_EPOCH" ]]; then
        echo "[backup] deleting ${file_name} (older than ${RETENTION_DAYS} days)"
        printf 'rm %s\n' "$file_name" >>"$DELETE_BATCH"
        ((DELETE_COUNT += 1))
    fi
done < <(grep -E '^alertbot-[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{6}Z\.dump(\.sig)?(\.part)?$' "$LIST_OUTPUT" || true)

if (( DELETE_COUNT > 0 )); then
    printf 'bye\n' >>"$DELETE_BATCH"
    SSHPASS="$SFTP_PASSWORD" sshpass -e sftp "${SFTP_COMMON[@]}" -b "$DELETE_BATCH" \
        "${BACKUP_NETCUP_USER}@${BACKUP_NETCUP_HOST}" >/dev/null
fi

echo "[backup] done"
