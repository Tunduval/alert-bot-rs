#!/usr/bin/env bash
# Restricted SSH forced-command receiver for production deployments.

set -euo pipefail

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
IFS=$' \t\n'
umask 077

readonly MAX_BINARY_BYTES=67108864
target=/usr/local/bin/alert-bot
staging=$(mktemp /usr/local/bin/.alert-bot.deploy.XXXXXX)
trap 'rm -f -- "$staging"' EXIT

# Read at most one byte beyond the limit. The outer timeout also prevents a
# client from holding a privileged forced command open indefinitely.
timeout 120 dd iflag=fullblock bs=1048576 count=65 of="$staging" status=none
size=$(stat -c '%s' "$staging")
(( size > 0 && size <= MAX_BINARY_BYTES )) || {
    echo "deploy rejected: binary must be between 1 byte and 64 MiB" >&2
    exit 2
}

magic=$(od -An -tx1 -N4 "$staging" | tr -d ' \n')
[[ "$magic" == 7f454c46 ]] || {
    echo "deploy rejected: input is not an ELF binary" >&2
    exit 2
}

chown root:root "$staging"
chmod 0755 "$staging"
mv -f -- "$staging" "$target"
trap - EXIT

systemctl restart alert-bot
systemctl is-active --quiet alert-bot
echo "alert-bot deployed"
