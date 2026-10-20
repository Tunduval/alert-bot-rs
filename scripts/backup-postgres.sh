#!/usr/bin/env bash
#
# Daily encrypted and signed Postgres backup -> Netcup SFTP. Native (no docker).
#
# Runs as the dedicated `alertbot-backup` system user. Its matching PostgreSQL
# role has read-only grants for pg_dump and cannot assume the application
# owner role or read the bot configuration.
#
# Required env (provided by root-owned /etc/alertbot-backup.env):
#   BACKUP_NETCUP_HOST    e.g. yourhost.netcup.net
#   BACKUP_NETCUP_USER    SFTP username
#   BACKUP_NETCUP_PASS    SFTP password
#   BACKUP_AGE_RECIPIENT  native age X25519 public recipient (age1...)
#
# Required systemd credential:
#   backup-signing-key    RSA private key
#
# Optional env:
#   BACKUP_NETCUP_PATH    remote directory (default: backups/alertbot)
#   BACKUP_KNOWN_HOSTS    pinned SFTP host keys for curl and sftp (default below)
#   DB_NAME               postgres database to dump (default: alertbot)
#   RETENTION_DAYS        prune remote dumps older than N days (default: 30)
#
# Required packages on the host: age, postgresql-client, curl, openssl, sshpass

set -euo pipefail

: "${BACKUP_NETCUP_HOST:?must be set}"
: "${BACKUP_NETCUP_USER:?must be set}"
: "${BACKUP_NETCUP_PASS:?must be set}"
: "${BACKUP_AGE_RECIPIENT:?must be set}"
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
[[ "$BACKUP_AGE_RECIPIENT" =~ ^age1[023456789acdefghjklmnpqrstuvwxyz]{58}$ ]] \
    || { echo "invalid BACKUP_AGE_RECIPIENT" >&2; exit 2; }
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

# Ask age to parse the recipient before reading the database. This rejects
# strings that match the public-key shape but have an invalid Bech32 checksum.
age --encrypt --recipient "$BACKUP_AGE_RECIPIENT" \
    </dev/null >/dev/null 2>/dev/null \
    || { echo "invalid BACKUP_AGE_RECIPIENT" >&2; exit 2; }

PG_DUMP_VERSION_OUTPUT=$(pg_dump --version 2>/dev/null) \
    || { echo "cannot inspect pg_dump version" >&2; exit 2; }
[[ "$PG_DUMP_VERSION_OUTPUT" =~ ^pg_dump\ \(PostgreSQL\)\ ([0-9]+)\.([0-9]+)([^0-9].*)?$ ]] \
    || { echo "unrecognized pg_dump version" >&2; exit 2; }
PG_DUMP_MAJOR=${BASH_REMATCH[1]}
PG_DUMP_MINOR=${BASH_REMATCH[2]}
case "$PG_DUMP_MAJOR" in
    16) PG_DUMP_FIXED_MINOR=15 ;;
    17) PG_DUMP_FIXED_MINOR=11 ;;
    18) PG_DUMP_FIXED_MINOR=6 ;;
    *)
        echo "unsupported pg_dump major ${PG_DUMP_MAJOR}; refusing backup" >&2
        exit 2
        ;;
esac
if (( 10#$PG_DUMP_MINOR < PG_DUMP_FIXED_MINOR )); then
    echo "pg_dump ${PG_DUMP_MAJOR}.${PG_DUMP_MINOR} is vulnerable to CVE-2026-19385" >&2
    exit 2
fi
unset PG_DUMP_VERSION_OUTPUT PG_DUMP_MAJOR PG_DUMP_MINOR PG_DUMP_FIXED_MINOR

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

# curl reads SFTP host keys only from $HOME/.ssh/known_hosts. With an entry
# for the host, libssh2 negotiates that key type and rejects any other key.
CURL_HOME="${WORK_DIR}/curl-home"
install -d -m 0700 "${CURL_HOME}" "${CURL_HOME}/.ssh"
install -m 0600 "$BACKUP_KNOWN_HOSTS" "${CURL_HOME}/.ssh/known_hosts"

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
FILE_NAME="alertbot-${DATE}.dump.age"
REMOTE_FILE="${BACKUP_NETCUP_PATH}/${FILE_NAME}"
REMOTE_MANIFEST="${REMOTE_FILE}.manifest"
REMOTE_SIGNATURE="${REMOTE_MANIFEST}.sig"
DUMP_FILE="${WORK_DIR}/${FILE_NAME%.age}"
ENCRYPTED_FILE="${WORK_DIR}/${FILE_NAME}"
MANIFEST_FILE="${ENCRYPTED_FILE}.manifest"
SIGNATURE_FILE="${MANIFEST_FILE}.sig"

echo "[backup] encrypting ${DB_NAME} backup -> ${REMOTE_FILE}"

pg_dump --format=custom --no-owner --no-privileges --no-blobs \
    --file="$DUMP_FILE" "$DB_NAME"
age --encrypt --recipient "$BACKUP_AGE_RECIPIENT" \
    --output "$ENCRYPTED_FILE" "$DUMP_FILE"
rm -f -- "$DUMP_FILE"
ENCRYPTED_SIZE=$(wc -c <"$ENCRYPTED_FILE" | tr -d '[:space:]')
ENCRYPTED_SHA256=$(openssl dgst -sha256 -r "$ENCRYPTED_FILE")
ENCRYPTED_SHA256=${ENCRYPTED_SHA256%% *}
[[ "$ENCRYPTED_SIZE" =~ ^[0-9]+$ ]] \
    && [[ "$ENCRYPTED_SHA256" =~ ^[0-9a-f]{64}$ ]] \
    || { echo "cannot describe encrypted backup" >&2; exit 2; }
printf '%s\n' \
    'format_version=2' \
    "database=${DB_NAME}" \
    "filename=${FILE_NAME}" \
    "created_at=${DATE}" \
    "size_bytes=${ENCRYPTED_SIZE}" \
    "sha256=${ENCRYPTED_SHA256}" >"$MANIFEST_FILE"
openssl dgst -sha256 -sign "$SIGNING_KEY" \
    -out "$SIGNATURE_FILE" "$MANIFEST_FILE"

# Publish the signature and manifest first and the dump last. Therefore a final
# dump name is visible only after all three complete files reached the server.
for local_file in "$ENCRYPTED_FILE" "$MANIFEST_FILE" "$SIGNATURE_FILE"; do
    remote_file="$REMOTE_FILE"
    if [[ "$local_file" == "$MANIFEST_FILE" ]]; then
        remote_file="$REMOTE_MANIFEST"
    elif [[ "$local_file" == "$SIGNATURE_FILE" ]]; then
        remote_file="$REMOTE_SIGNATURE"
    fi
    HOME="$CURL_HOME" curl "${CURL_COMMON[@]}" \
         --upload-file "$local_file" \
         --ftp-create-dirs \
         "sftp://${BACKUP_NETCUP_HOST}/${remote_file}.part"
done

for remote_file in "$REMOTE_SIGNATURE" "$REMOTE_MANIFEST" "$REMOTE_FILE"; do
    HOME="$CURL_HOME" curl "${CURL_COMMON[@]}" \
         --quote "rename /${remote_file}.part /${remote_file}" \
         --output /dev/null \
         "sftp://${BACKUP_NETCUP_HOST}/${BACKUP_NETCUP_PATH}/"
done

echo "[backup] uploaded encrypted dump with signed manifest"

CUTOFF_EPOCH=$(date -u -d "${RETENTION_DAYS} days ago" +%s 2>/dev/null \
              || date -u -v-"${RETENTION_DAYS}d" +%s)

LIST_BATCH="${WORK_DIR}/list.batch"
LIST_OUTPUT="${WORK_DIR}/list.output"
printf 'cd /%s\nls -1\nbye\n' "$BACKUP_NETCUP_PATH" >"$LIST_BATCH"
RETENTION_LIST_MAX_BYTES=1048576
set +e
SSHPASS="$SFTP_PASSWORD" timeout --signal=TERM --kill-after=5s 60s \
    sshpass -e sftp "${SFTP_COMMON[@]}" -b "$LIST_BATCH" \
    "${BACKUP_NETCUP_USER}@${BACKUP_NETCUP_HOST}" \
    | head -c "$((RETENTION_LIST_MAX_BYTES + 1))" >"$LIST_OUTPUT"
LIST_PIPE_STATUS=("${PIPESTATUS[@]}")
set -e
LIST_OUTPUT_SIZE=$(wc -c <"$LIST_OUTPUT" | tr -d '[:space:]')
if (( LIST_OUTPUT_SIZE > RETENTION_LIST_MAX_BYTES )); then
    echo "retention listing exceeds ${RETENTION_LIST_MAX_BYTES} bytes" >&2
    exit 2
fi
if (( LIST_PIPE_STATUS[0] != 0 || LIST_PIPE_STATUS[1] != 0 )); then
    echo "retention listing failed or exceeded 60 seconds" >&2
    exit 2
fi

DELETE_BATCH="${WORK_DIR}/delete.batch"
printf 'cd /%s\n' "$BACKUP_NETCUP_PATH" >"$DELETE_BATCH"
DELETE_COUNT=0

while IFS= read -r file_name; do
    timestamp=${file_name#alertbot-}
    timestamp=${timestamp%%.*}
    file_time="${timestamp:0:10} ${timestamp:11:2}:${timestamp:13:2}:${timestamp:15:2}"
    file_epoch=$(date -u -d "$file_time" +%s 2>/dev/null \
                 || date -u -j -f '%Y-%m-%d %H:%M:%S' "$file_time" +%s 2>/dev/null \
                 || true)
    if [[ -n "$file_epoch" && "$file_epoch" -lt "$CUTOFF_EPOCH" ]]; then
        echo "[backup] deleting ${file_name} (older than ${RETENTION_DAYS} days)"
        printf 'rm %s\n' "$file_name" >>"$DELETE_BATCH"
        ((DELETE_COUNT += 1))
    fi
done < <(grep -E '^alertbot-[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{6}Z\.(dump\.age(\.part|\.manifest(\.part|\.sig(\.part)?)?)?|sql\.gz(\.part)?)$' "$LIST_OUTPUT" || true)

if (( DELETE_COUNT > 0 )); then
    printf 'bye\n' >>"$DELETE_BATCH"
    SSHPASS="$SFTP_PASSWORD" sshpass -e sftp "${SFTP_COMMON[@]}" -b "$DELETE_BATCH" \
        "${BACKUP_NETCUP_USER}@${BACKUP_NETCUP_HOST}" >/dev/null
fi

echo "[backup] done"
