#!/bin/bash
# Restricted SSH forced-command receiver for production deployments.

set -euo pipefail

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
IFS=$' \t\n'
umask 077

readonly MAX_BINARY_BYTES=67108864
readonly SERVICE=alert-bot
readonly RECEIVER=/usr/local/sbin/alertbot-deploy-receiver.sh
readonly SERVICE_UNIT=/etc/systemd/system/alert-bot.service
readonly SERVICE_UNIT_SHA256=e2ee0e52b5d8a0758468059433ddbd9c700993ce1d083011eacc551680895a4a
readonly TARGET=/usr/local/bin/alert-bot
readonly LOCK_DIR=/run/alertbot-deploy
readonly LOCK_FILE=$LOCK_DIR/deploy.lock

reject() {
    echo "deploy rejected: $*" >&2
    exit 2
}

(( EUID == 0 )) || reject "receiver must run as root"
[[ "$0" == "$RECEIVER" && ! -L "$RECEIVER" ]] || reject "unexpected receiver path"
[[ $(stat -c '%U:%G:%a' "$RECEIVER") == root:root:755 ]] ||
    reject "receiver ownership or mode is unsafe"
[[ ! -L "$SERVICE_UNIT" && $(stat -c '%U:%G:%a' "$SERVICE_UNIT") == root:root:644 ]] ||
    reject "systemd unit ownership or mode is unsafe"
for trusted_dir in /usr /usr/local /usr/local/bin /usr/local/sbin \
    /etc /etc/systemd /etc/systemd/system; do
    [[ -d "$trusted_dir" && ! -L "$trusted_dir" ]] || reject "unsafe path: $trusted_dir"
    [[ $(stat -c '%U:%G:%a' "$trusted_dir") == root:root:755 ]] ||
        reject "unsafe directory ownership or mode: $trusted_dir"
done

# Keep the lock outside the world-writable /run/lock directory. Otherwise the
# deployed service account could pre-create it after a reboot and block root's
# open through fs.protected_regular.
if [[ ! -e "$LOCK_DIR" && ! -L "$LOCK_DIR" ]]; then
    install -d -o root -g root -m 0755 "$LOCK_DIR"
fi
[[ -d "$LOCK_DIR" && ! -L "$LOCK_DIR" ]] || reject "unsafe deploy lock directory"
[[ $(stat -c '%U:%G:%a' "$LOCK_DIR") == root:root:755 ]] ||
    reject "unsafe deploy lock directory"
if [[ -e "$LOCK_FILE" || -L "$LOCK_FILE" ]]; then
    [[ -f "$LOCK_FILE" && ! -L "$LOCK_FILE" ]] || reject "unsafe deploy lock file"
    [[ $(stat -c '%U:%G:%a' "$LOCK_FILE") == root:root:600 ]] ||
        reject "unsafe deploy lock file"
fi

# Serialize deployments before allocating a staging file. This bounds disk use
# even when several authenticated clients connect at once.
exec 9>"$LOCK_FILE"
[[ $(stat -Lc '%u:%g:%a' /proc/self/fd/9) == 0:0:600 ]] ||
    reject "unsafe deploy lock file"
flock -n 9 || reject "another deployment is already running"

# A deploy key may choose the program bytes, but those bytes must only ever run
# as the sandboxed alertbot account. Refuse stale, overridden, or weakened units.
systemctl daemon-reload
[[ $(systemctl show --property=FragmentPath --value "$SERVICE") == "$SERVICE_UNIT" ]] ||
    reject "unexpected systemd unit path"
[[ -z $(systemctl show --property=DropInPaths --value "$SERVICE") ]] ||
    reject "systemd drop-ins are not allowed"
printf '%s  %s\n' "$SERVICE_UNIT_SHA256" "$SERVICE_UNIT" |
    sha256sum --check --status - || reject "systemd unit does not match the expected version"
[[ $(systemctl show --property=User --value "$SERVICE") == alertbot ]] ||
    reject "systemd service must run as alertbot"
[[ $(systemctl show --property=Group --value "$SERVICE") == alertbot ]] ||
    reject "systemd service must run with group alertbot"
[[ $(systemctl show --property=NoNewPrivileges --value "$SERVICE") == yes ]] ||
    reject "systemd service must set NoNewPrivileges=yes"

# No legitimate staging file can exist while this exclusive lock is held.
find /usr/local/bin -xdev -maxdepth 1 -type f \
    -name '.alert-bot.deploy.??????' -user root -delete

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
