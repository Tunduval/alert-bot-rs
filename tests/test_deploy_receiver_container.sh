#!/bin/bash
# Exercise the privileged deploy receiver inside a disposable container.

set -euo pipefail

: "${DEPLOY_TEST_IMAGE:?set DEPLOY_TEST_IMAGE to a pinned Linux image with bash and coreutils}"
container_runtime=${CONTAINER_RUNTIME:-podman}
repo=$(git rev-parse --show-toplevel)

"$container_runtime" run --rm --interactive --user 0 \
    --volume "$repo:/repo:ro" \
    "$DEPLOY_TEST_IMAGE" /bin/bash -s <<'CONTAINER'
set -euo pipefail

install -d -o root -g root -m 0755 \
    /usr/local/bin /usr/local/sbin /etc/systemd /etc/systemd/system
install -o root -g root -m 0755 \
    /repo/scripts/alertbot-deploy-receiver.sh \
    /usr/local/sbin/alertbot-deploy-receiver.sh
install -o root -g root -m 0644 \
    /repo/scripts/systemd/alert-bot.service \
    /etc/systemd/system/alert-bot.service

cat > /usr/sbin/systemctl <<'MOCK'
#!/bin/bash
set -euo pipefail
case "$*" in
    daemon-reload|"restart alert-bot"|"is-active --quiet alert-bot") ;;
    "show --property=FragmentPath --value alert-bot")
        echo /etc/systemd/system/alert-bot.service
        ;;
    "show --property=DropInPaths --value alert-bot") ;;
    "show --property=User --value alert-bot") echo alertbot ;;
    "show --property=Group --value alert-bot") echo alertbot ;;
    "show --property=NoNewPrivileges --value alert-bot") echo yes ;;
    *)
        echo "unexpected systemctl arguments: $*" >&2
        exit 90
        ;;
esac
MOCK
chmod 0755 /usr/sbin/systemctl

receiver=/usr/local/sbin/alertbot-deploy-receiver.sh

# A successful install must report the exact bytes received.
touch /usr/local/bin/.alert-bot.deploy.ABC123
expected=$(sha256sum /bin/true)
expected=${expected%% *}
actual=$($receiver < /bin/true)
[[ "$actual" == "alert-bot deployed sha256=$expected" ]]
[[ $(sha256sum /usr/local/bin/alert-bot) == "$expected  /usr/local/bin/alert-bot" ]]
[[ ! -e /usr/local/bin/.alert-bot.deploy.ABC123 ]]
[[ $(stat -c '%U:%G:%a' /run/alertbot-deploy) == root:root:755 ]]
[[ $(stat -c '%U:%G:%a' /run/alertbot-deploy/deploy.lock) == root:root:600 ]]

# A pre-created or redirected lock must fail closed without touching its target.
rm -f /run/alertbot-deploy/deploy.lock
printf 'preserve\n' >/tmp/lock-victim
ln -s /tmp/lock-victim /run/alertbot-deploy/deploy.lock
if $receiver < /bin/true 2>/tmp/unsafe-lock.err; then
    echo "unsafe deploy lock was accepted" >&2
    exit 1
fi
grep -Fq "unsafe deploy lock file" /tmp/unsafe-lock.err
grep -Fxq "preserve" /tmp/lock-victim
rm -f /run/alertbot-deploy/deploy.lock

# Invalid bytes must not replace the installed binary.
if printf nope | $receiver 2>/tmp/invalid.err; then
    echo "invalid input was accepted" >&2
    exit 1
fi
grep -Fq "input is not an ELF binary" /tmp/invalid.err
[[ $(sha256sum /usr/local/bin/alert-bot) == "$expected  /usr/local/bin/alert-bot" ]]

# The exact unit and root-owned path invariants fail closed.
printf '\n# tampered\n' >> /etc/systemd/system/alert-bot.service
if $receiver < /bin/true 2>/tmp/unit.err; then
    echo "tampered unit was accepted" >&2
    exit 1
fi
grep -Fq "systemd unit does not match" /tmp/unit.err
install -o root -g root -m 0644 /repo/scripts/systemd/alert-bot.service \
    /etc/systemd/system/alert-bot.service

chmod 0777 /usr/local/bin
if $receiver < /bin/true 2>/tmp/mode.err; then
    echo "writable target directory was accepted" >&2
    exit 1
fi
grep -Fq "unsafe directory ownership or mode" /tmp/mode.err
chmod 0755 /usr/local/bin

# A second authenticated session cannot race an active deployment.
flock /run/alertbot-deploy/deploy.lock sleep 5 &
locker=$!
for _ in 1 2 3 4 5; do
    if ! flock -n /run/alertbot-deploy/deploy.lock true; then
        break
    fi
    sleep 0.1
done
if $receiver < /bin/true 2>/tmp/lock.err; then
    echo "concurrent deployment was accepted" >&2
    exit 1
fi
grep -Fq "another deployment is already running" /tmp/lock.err
kill "$locker"
wait "$locker" 2>/dev/null || true

echo "deploy receiver container tests passed"
CONTAINER
