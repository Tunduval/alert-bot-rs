#!/bin/bash
# Restricted SSH forced-command receiver for production deployments.

set -euo pipefail

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
IFS=$' \t\n'
umask 077

readonly MAX_BINARY_BYTES=67108864
readonly SERVICE=alert-bot
readonly SERVICE_UNIT=/etc/systemd/system/alert-bot.service
readonly SERVICE_UNIT_SHA256=0b1e1eaf92b1ab63e035f0a0a4985400fee21a9696abe0f5d42ec72c2fc7e3b1
readonly TARGET=/usr/local/bin/alert-bot

reject() {
    echo "deploy rejected: $*" >&2
    exit 2
}

(( EUID == 0 )) || reject "receiver must run as root"

# Serialize deployments before allocating a staging file. This bounds disk use
# even when several authenticated clients connect at once.
exec 9>/run/lock/alertbot-deploy.lock
flock -n 9 || reject "another deployment is already running"

# A deploy key may choose the program bytes, but those bytes must only ever run
# as the sandboxed alertbot account. Refuse stale, overridden, or weakened units.
systemctl daemon-reload
[[ $(systemctl show --property=FragmentPath --value "$SERVICE") == "$SERVICE_UNIT" ]] ||
    reject "unexpected systemd unit path"
[[ -z $(systemctl show --property=DropInPaths --value "$SERVICE") ]] ||
    reject "systemd drop-ins are not allowed"
printf '%s  %s\n' "$SERVICE_UNIT_SHA256" "$SERVICE_UNIT" |
    sha256sum --check --status - || reject "systemd unit does not match the reviewed version"
[[ $(systemctl show --property=User --value "$SERVICE") == alertbot ]] ||
    reject "systemd service must run as alertbot"
[[ $(systemctl show --property=Group --value "$SERVICE") == alertbot ]] ||
    reject "systemd service must run with group alertbot"
[[ $(systemctl show --property=NoNewPrivileges --value "$SERVICE") == yes ]] ||
    reject "systemd service must set NoNewPrivileges=yes"

find /usr/local/bin -xdev -maxdepth 1 -type f \
    -name '.alert-bot.deploy.??????' -user root -mmin +10 -delete

staging=$(mktemp /usr/local/bin/.alert-bot.deploy.XXXXXX)
trap 'rm -f -- "$staging"' EXIT

# Read at most one byte beyond the limit. The outer timeout also prevents a
# client from holding a privileged forced command open indefinitely.
timeout 120 dd iflag=fullblock bs=1048576 count=65 of="$staging" status=none
size=$(stat -c '%s' "$staging")
(( size > 0 && size <= MAX_BINARY_BYTES )) || {
    reject "binary must be between 1 byte and 64 MiB"
}

magic=$(od -An -tx1 -N4 "$staging" | tr -d ' \n')
[[ "$magic" == 7f454c46 ]] || {
    reject "input is not an ELF binary"
}

digest=$(sha256sum "$staging")
digest=${digest%% *}
chown root:root "$staging"
chmod 0755 "$staging"
mv -f -- "$staging" "$TARGET"
trap - EXIT
installed_digest=$(sha256sum "$TARGET")
installed_digest=${installed_digest%% *}
[[ "$installed_digest" == "$digest" ]] || reject "installed binary hash changed"

systemctl restart "$SERVICE"
sleep 3
systemctl is-active --quiet "$SERVICE" || reject "service did not stay active"
printf 'alert-bot deployed sha256=%s\n' "$installed_digest"
