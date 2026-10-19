# alert-bot-rs

[![CI](https://github.com/duramson/alert-bot-rs/actions/workflows/deploy.yml/badge.svg)](https://github.com/duramson/alert-bot-rs/actions/workflows/deploy.yml)
[![Rust](https://img.shields.io/badge/rust-1.86%2B-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)
[![Try it on Telegram](https://img.shields.io/badge/Telegram-%40rust__alertbot-26A5E4?logo=telegram&logoColor=white)](https://t.me/rust_alertbot)

A Telegram reminder bot in Rust. German and English natural language, fuzzy
matching against typos, group reminders with per-user permissions, no LLM
required — runs deterministically on your own hardware.

The live instance runs on my Proxmox homelab as
[**@rust_alertbot**](https://t.me/rust_alertbot) — message it `/start` to try it.

```
You:   /alert 5m Kaffee fertig
Bot:   ✓ #1 · Fr 8.5.2026 14:03
       [✗ Löschen]

You:   /alert do 14:00 Standup
Bot:   ✓ #2 · Do 14.5.2026 14:00
       [✗ Löschen]

You:   /alert 30.4.27 ssl-zertifikat erneuern
Bot:   ✓ #3 · Fr 30.4.2027 09:00
       [✗ Löschen]
```

## Features

- **Compact slash syntax** — `/alert 5m text`, `/alert 7h30m text`,
  `/alert 1Y2M15d text`, `/alert 22:00 text` (next 22:00),
  `/alert 30.4.26 text`, `/alert 2026-04-30 text`, `/alert 2pm text`,
  `/alert morgen 9 Uhr text`, `/alert do 14:00 text`. Case-sensitive `M`/`Y`
  for months / years (lowercase `m` = minute). Full input contract in
  [RULES.md](RULES.md). Known deviations are tracked in the local, unversioned `BACKLOG.md`.
- **Recurring** — `*` prefix or `every`/`alle`/`jeden`. `*30m water`,
  `*1d vitamin`, `*do 14:00 standup`, `*mo,mi,fr 9 yoga`, `*1. rent`,
  `*24.12 christmas`. `*31.` and `*29.2` fall back to the last day of the
  month / Feb 28 in non-leap years and announce that once at creation.
  Minimum interval 30 minutes.
- **DE + EN parser** — keywords (`morgen`/`tomorrow`, weekdays, `Uhr`, time
  units) accept both languages and tolerate typos via Levenshtein distance
  with adaptive thresholds. `heute`/`today` is intentionally rejected with
  a hint to use bare clock-time instead.
- **Group reminders with two scopes**
  - `/alert` in a group fires in the group, but only the creator can cancel it.
  - `/galert` is a shared group reminder; anyone in the chat can cancel.
  - Commands sent as an anonymous administrator or on behalf of a chat are
    ignored because Telegram does not provide a trustworthy user identity.
- **Delivery controls** — snooze by 5 minutes, 15 minutes or 1 hour; stop a
  recurring series directly from its delivered message.
- **Per-user language and timezone** — auto-detected from Telegram's
  `language_code`, overridable via `/lang` and `/tz`. The slash-menu also
  switches to the right language when `/lang` is used in a DM.
- **Webhook or polling** — long-polling for local development, webhook + TLS
  via Cloudflare Tunnel (or any reverse proxy) for production.
- **Postgres-backed delivery** — workers claim due alerts via
  `FOR UPDATE SKIP LOCKED`, listen on `NOTIFY` so they don't poll, and
  redrive stuck claims after a worker crash.
- **No LLM, no API keys beyond Telegram** — fully self-hosted.

## Quick start (local development)

```bash
git clone https://github.com/duramson/alert-bot-rs.git
cd alert-bot-rs
cp .env.example .env
# edit .env: BOT_TOKEN (from @BotFather) and POSTGRES_PASSWORD

docker compose up -d postgres       # local-dev DB only — bound to 127.0.0.1:5432
cargo run -p alert-bot
```

The bot falls back to long-polling when `WEBHOOK_URL` is unset, so no public
endpoint is needed for development. Open Telegram, message your bot, send
`/start`.

> **Use a separate dev bot.** Telegram delivers every update to a single
> webhook/poll target, so running this against your production bot token would
> fight the deployed instance for ownership. Create a second bot at
> [@BotFather](https://t.me/BotFather) and put its token in `.env`.

## Configuration

| Variable             | Required | Default                  | Notes                                                    |
|----------------------|----------|--------------------------|----------------------------------------------------------|
| `BOT_TOKEN`          | yes      | —                        | from [@BotFather](https://t.me/BotFather)                |
| `DATABASE_URL`       | yes      | —                        | `postgres://user:pw@host:port/db` (TCP) or `postgres:///alertbot?host=/var/run/postgresql` (Unix socket on the LXC) |
| `POSTGRES_PASSWORD`  | dev      | —                        | only consumed by `docker-compose.yml`; ignored in prod   |
| `POSTGRES_USER`      | dev      | `alertbot`               | same                                                     |
| `POSTGRES_DB`        | dev      | `alertbot`               | same                                                     |
| `ADMIN_CHAT_ID`      | no       | empty                    | Telegram chat id that gets ops notifications             |
| `WEBHOOK_URL`        | no       | empty (long-polling)     | public HTTPS URL Telegram POSTs to                       |
| `WEBHOOK_LISTEN`     | no       | `0.0.0.0:8080`           | bind address inside the process                          |
| `WEBHOOK_SECRET`     | no       | generated by teloxide   | optional fixed token; verified via `X-Telegram-Bot-Api-Secret-Token` |
| `STATS_LISTEN`       | no       | disabled                 | read-only `/stats`; use an internal address, e.g. `127.0.0.1:8081`; no endpoint authentication |
| `RUST_LOG`           | no       | `info,alert_bot=debug`   |                                                          |

## Production deployment (Proxmox LXC + Cloudflare Tunnel)

This is how the live instance runs. cloudflared lives in its own LXC and
fronts multiple services on the same Proxmox host; the alert-bot LXC just
exposes `:8080` on the internal Proxmox bridge so cloudflared can reach it.

```mermaid
flowchart LR
    tg[Telegram] -- HTTPS --> cf[Cloudflare edge]
    cf -- tunnel --> cfd[cloudflared LXC]
    cfd -- "HTTP :8080" --> bot

    subgraph lxc["alert-bot LXC · native systemd units, no Docker"]
        bot[alert-bot] -- "local Unix socket" --> pg[(Postgres)]
    end

    pg -- "pg_dump nightly · SFTP" --> netcup[(Netcup · off-site dumps)]
```

No Docker. Postgres and the bot binary both run as native systemd units on
the same LXC.

### One-time setup

#### 1. Create the LXC

Create a dedicated, unprivileged Debian LXC from a trusted template. Pin or
record the exact template digest before creating the container so a rebuild
uses the same reviewed input. Do not pipe a downloaded community script into a
root shell.

Install the required system packages from Debian's signed repositories. If the
exact PostgreSQL 16 release is not in that Debian release, use PostgreSQL's
official signed APT repository. Keep the container minimal (2 CPU / 2 GB /
8 GB disk is enough for a small instance), enable automatic security updates,
and record the LXC's internal IP. Confirm that local PostgreSQL connections use
peer authentication before continuing.

#### 2. Postgres role + database + system user

```bash
ssh root@<lxc-ip>

# System user the bot will run as. No shell, no home, no password —
# only systemd ever activates it.
useradd --system --no-create-home --shell /usr/sbin/nologin alertbot

# Postgres role that matches the system user, so peer auth on the
# local Unix socket Just Works.
sudo -u postgres createuser alertbot
sudo -u postgres createdb -O alertbot alertbot

# Sanity check — should print a row count of 0.
sudo -u alertbot psql -d alertbot -c "SELECT count(*) FROM pg_tables WHERE schemaname='public';"
```

No password is stored anywhere — peer auth via the Unix socket binds
identity to the calling system user.

#### 3. Install the systemd unit + config file

The repo lives only on your dev machine; the LXC only needs the unit
files and a config file. Copy them over the first time:

```bash
# from your dev machine
ssh root@<lxc-ip> install -d -o root -g root -m 0700 /root/alertbot-install
scp scripts/systemd/alert-bot.service       root@<lxc-ip>:/root/alertbot-install/
scp scripts/systemd/alertbot-backup.service root@<lxc-ip>:/root/alertbot-install/
scp scripts/systemd/alertbot-backup.timer   root@<lxc-ip>:/root/alertbot-install/
scp scripts/backup-postgres.sh              root@<lxc-ip>:/root/alertbot-install/
scp scripts/alertbot-deploy-receiver.sh     root@<lxc-ip>:/root/alertbot-install/
```

```bash
# on the LXC
install -o root -g root -m 0644 /root/alertbot-install/alert-bot.service \
    /etc/systemd/system/alert-bot.service
install -o root -g root -m 0644 /root/alertbot-install/alertbot-backup.service \
    /etc/systemd/system/alertbot-backup.service
install -o root -g root -m 0644 /root/alertbot-install/alertbot-backup.timer \
    /etc/systemd/system/alertbot-backup.timer
install -o root -g root -m 0755 /root/alertbot-install/backup-postgres.sh \
    /usr/local/bin/alertbot-backup.sh
install -o root -g root -m 0755 /root/alertbot-install/alertbot-deploy-receiver.sh \
    /usr/local/sbin/alertbot-deploy-receiver.sh

install -d -o root -g alertbot -m 0750 /etc/alert-bot
install -o root -g alertbot -m 0640 /dev/null /etc/alert-bot/config.env
webhook_secret=$(openssl rand -hex 32)
cat > /etc/alert-bot/config.env <<'EOF'
BOT_TOKEN=<from BotFather>
DATABASE_URL=postgres:///alertbot?host=/var/run/postgresql
ADMIN_CHAT_ID=<your numeric Telegram user id>
RUST_LOG=info,alert_bot=debug
# WEBHOOK_URL=         # added in step 5
EOF
printf 'WEBHOOK_SECRET=%s\n' "$webhook_secret" >> /etc/alert-bot/config.env
unset webhook_secret

systemctl daemon-reload
```

The bot isn't installed yet — that's step 4.

#### 4. Install the first binary

Until CI is wired up (step 7), build on a Linux machine compatible with the
LXC architecture and glibc, then stream the binary through the restricted
deploy receiver. CI later uses the same path (step 7).

```bash
# on the compatible Linux build machine
cargo build --release --bin alert-bot
strip target/release/alert-bot
ssh root@<lxc-ip> env -i /usr/local/sbin/alertbot-deploy-receiver.sh \
    < target/release/alert-bot

# on the LXC
systemctl enable --now alert-bot
systemctl status alert-bot --no-pager -l
journalctl -u alert-bot -n 30
```

You should see `transport=polling` in the startup line. If you set
`ADMIN_CHAT_ID`, your Telegram DM gets a `✅ alert-bot started`
notification.

Migrations run automatically on every start (`PgStore::migrate`), so the
schema is materialised on first boot.

> **glibc note.** Building the release locally on macOS produces a
> Mach-O binary that won't run on Linux. If you're on macOS, either use
> `cargo zigbuild --target x86_64-unknown-linux-gnu --release` (needs
> `brew install zig && cargo install cargo-zigbuild`), or just push the
> commit and let the CI workflow do the build for you — the workflow
> streams the artifact to the restricted LXC receiver after build.

#### 5. Cloudflare Tunnel route → webhook switch

In the Cloudflare Zero Trust dashboard for the existing cloudflared LXC:

- Tunnel → Public Hostnames → Add
- Subdomain: `alert` (or whatever), Domain: yours
- Service: HTTP, URL: `<bot-lxc-ip>:8080`

Then on the LXC:

```bash
nano /etc/alert-bot/config.env
# WEBHOOK_URL=https://alert.yourdomain.de/webhook

systemctl restart alert-bot
systemctl is-active --quiet alert-bot
```

The environment file contains secrets. Keep it owned by `root:alertbot` with
mode `0640`, and never source it into an interactive shell. The webhook secret
above is generated before the file is written; systemd environment files do
not evaluate shell substitutions such as `$(openssl ...)`.

#### 6. Migrate data from the old host (skip if starting fresh)

On the source machine (old docker-on-LXC setup):

```bash
docker compose stop bot
docker compose exec -T postgres \
    pg_dump -U alertbot --format=custom --no-owner --no-privileges alertbot \
    > alertbot-migration.dump
scp alertbot-migration.dump root@<new-lxc-ip>:/root/alertbot-migration.dump
```

On the new LXC:

```bash
set -euo pipefail
migration=/root/alertbot-migration.dump
test -f "$migration"
test ! -L "$migration"
chmod 0600 "$migration"
systemctl stop alert-bot
cleanup_migration() {
    rm -f -- "$migration"
    systemctl start alert-bot
}
trap cleanup_migration EXIT
sudo -u alertbot pg_restore --exit-on-error --single-transaction \
    --clean --if-exists --no-owner --no-privileges \
    --dbname=alertbot "$migration"
sudo -u alertbot psql -d alertbot \
    -c "SELECT count(*), max(created_at) FROM alerts;"
cleanup_migration
trap - EXIT
```

#### 7. Cloudflare Access SSH for CI deploys

The GitHub Actions workflow connects to the LXC over Cloudflare Access
SSH — no SSH port needs to be exposed on the LXC.

Create an unprivileged account that exists only to enter the root-owned deploy
receiver. The clean `env -i` boundary is inside the exact `sudoers` command, so
SSH environment variables can never reach a root shell or interpreter.

```bash
# on the LXC
useradd --system --home-dir /var/lib/alertbot-deploy --no-create-home \
    --shell /bin/sh alertbot-deploy
install -d -o root -g root -m 0755 /var/lib/alertbot-deploy
install -d -o root -g root -m 0755 /var/lib/alertbot-deploy/.ssh
install -o root -g root -m 0600 /dev/null \
    /var/lib/alertbot-deploy/.ssh/authorized_keys

cat > /etc/sudoers.d/alertbot-deploy <<'EOF'
Defaults:alertbot-deploy env_reset,secure_path=/usr/sbin:/usr/bin:/sbin:/bin
alertbot-deploy ALL=(root:root) NOPASSWD:NOSETENV: /usr/bin/env -i /usr/local/sbin/alertbot-deploy-receiver.sh
EOF
chmod 0440 /etc/sudoers.d/alertbot-deploy
visudo -cf /etc/sudoers.d/alertbot-deploy
```

Limit this account at the SSH daemon as a second layer. Validate the complete
server configuration before reloading it.

```bash
cat > /etc/ssh/sshd_config.d/00-alertbot-deploy.conf <<'EOF'
# Global directive. Keep it before the Match block.
PermitUserEnvironment no

Match User alertbot-deploy
    AuthenticationMethods publickey
    PasswordAuthentication no
    KbdInteractiveAuthentication no
    PermitTTY no
    DisableForwarding yes
    PermitTunnel no
    PermitUserRC no
    ForceCommand /usr/bin/env -i /usr/bin/sudo -n -- /usr/bin/env -i /usr/local/sbin/alertbot-deploy-receiver.sh

Match all
EOF
sshd -t
sshd -T -C user=alertbot-deploy,host=localhost,addr=127.0.0.1 | \
    grep -Ei '^(authenticationmethods|passwordauthentication|kbdinteractiveauthentication|permittty|disableforwarding|permituserrc|permittunnel|permituserenvironment|forcecommand) '
sshd -T -C user=alertbot-deploy,host=localhost,addr=127.0.0.1 | \
    grep -Ei '^acceptenv '
systemctl reload ssh
```

The effective `AcceptEnv` list may contain locale variables such as `LANG` and
`LC_*`. It must not contain a name or wildcard that can match `BASH_ENV`, `ENV`,
or `LD_*`.

**Generate a dedicated key pair** (don't reuse your personal one):

```bash
ssh-keygen -t ed25519 -f ~/.ssh/alertbot-deploy -C "github-actions-deploy"
deploy_key=$(cat ~/.ssh/alertbot-deploy.pub)
ssh root@<lxc-ip> "printf '%s %s\\n' \
  'restrict,command=\"/usr/bin/env -i /usr/bin/sudo -n -- /usr/bin/env -i /usr/local/sbin/alertbot-deploy-receiver.sh\"' \
  '$deploy_key' >> /var/lib/alertbot-deploy/.ssh/authorized_keys"
unset deploy_key
```

The forced command accepts one ELF binary of at most 64 MiB, installs it
atomically, and restarts only `alert-bot`. It refuses concurrent deploys and
refuses to run if the reviewed service unit or its non-root sandbox changed.
The key cannot open a shell, forward ports, run `scp`, or invoke another root
command. If its forced shell were ever bypassed, the attacker would still be
the unprivileged `alertbot-deploy` user and `sudo` would still allow only the
clean-environment receiver. Test the restriction before adding the private key
to GitHub:

```bash
ssh -i ~/.ssh/alertbot-deploy alertbot-deploy@<lxc-ip> id </dev/null
# Expected: "deploy rejected", never uid output.
```

**Cloudflare Tunnel → SSH route + Access policy.** Same cloudflared
instance as the webhook route:

- Public Hostnames → Add
  - Subdomain: `ssh-alert` (or anything)
  - Service: **SSH**, URL: `<bot-lxc-ip>:22`
- Zero Trust → Access → Applications → Add Application
  - **Self-hosted**, App URL: `ssh-alert.yourdomain.de`
  - Add a policy: action **Allow**, **Service Auth** rule → create a
    fresh Service Token, copy the Client ID and Client Secret (shown
    only once)

**Protected GitHub environment.** Create an environment named `production`.
Allow deployments only from `master`, require a reviewer, and enable
**Prevent self-review**. Store the following values in that environment, not as
repository secrets:

| Type     | Name                          | Value                                      |
|----------|-------------------------------|--------------------------------------------|
| Secret   | `LXC_SSH_KEY`                 | contents of `~/.ssh/alertbot-deploy`       |
| Secret   | `LXC_SSH_KNOWN_HOSTS`         | verified `known_hosts` line for `SSH_HOST` |
| Secret   | `CF_ACCESS_CLIENT_ID`         | from the Service Token                     |
| Secret   | `CF_ACCESS_CLIENT_SECRET`     | from the Service Token                     |
| Variable | `SSH_HOST`                    | `ssh-alert.yourdomain.de`                  |
| Variable | `SSH_USER`                    | `alertbot-deploy`                          |

Collect the LXC SSH host key over a trusted path and verify its fingerprint
out of band before saving the complete `known_hosts` line. The workflow uses
strict host-key checking and fails closed when the key changes.

Protect `master` with a branch ruleset that requires a pull request, at least
one approval from a code owner, and the `Test` status check. Block force pushes
and deletion. Do not exempt Renovate or another automation account from these
rules, and disable auto-merge for changes to GitHub Actions workflows.

Complete the following migration **before merging the hardened workflow**:

1. Create and protect the `production` environment as described above.
2. Rotate all four production secrets and store the new values only in that
   environment.
3. Delete every repository- and organization-level copy of those production
   secrets that could still reach this repository.
4. Install the checked-in service unit and deploy receiver. Create the
   `alertbot-deploy` account, SSH policy, and exact `sudoers` rule from step 7.
   Remove the old deploy key from root's `authorized_keys`, then verify the new
   key cannot run `id` or open a shell.
5. Create the branch ruleset and verify that `.github/CODEOWNERS` requires the
   owner to review workflow changes.
6. Cancel any still-running legacy deploy. Do not re-run an old deploy; delete
   old runs if their continued availability is not required for audit.
7. Set the repository variable `PRODUCTION_DEPLOY_ENABLED=true` last.

The final variable is an explicit fail-closed gate. Without it the workflow
still tests and builds `master`, but the deploy job remains skipped. Environment
protection applies only to environment secrets; it does not protect
repository-level secrets.

The next code push to `master` triggers tests → build → restricted SSH-stream
deployment via Cloudflare Access. There is no GitHub Release step.

#### 8. Nightly off-site backup → Netcup SFTP

```bash
apt install -y curl openssl sshpass postgresql-client

# Keep backup credentials outside the bot's Unix identity.
useradd --system --no-create-home --shell /usr/sbin/nologin alertbot-backup
sudo -u postgres createuser --no-createdb --no-createrole --no-superuser alertbot-backup
sudo -u postgres psql -v ON_ERROR_STOP=1 --dbname alertbot <<'SQL'
GRANT CONNECT ON DATABASE alertbot TO "alertbot-backup";
GRANT USAGE ON SCHEMA public TO "alertbot-backup";
GRANT SELECT ON ALL TABLES IN SCHEMA public TO "alertbot-backup";
GRANT SELECT ON ALL SEQUENCES IN SCHEMA public TO "alertbot-backup";
SQL
sudo -u alertbot psql -v ON_ERROR_STOP=1 --dbname alertbot <<'SQL'
ALTER DEFAULT PRIVILEGES IN SCHEMA public
  GRANT SELECT ON TABLES TO "alertbot-backup";
ALTER DEFAULT PRIVILEGES IN SCHEMA public
  GRANT SELECT ON SEQUENCES TO "alertbot-backup";
SQL

# Confirm peer authentication and the narrowly scoped read access.
sudo -u alertbot-backup psql -v ON_ERROR_STOP=1 --dbname alertbot \
  --command 'SELECT count(*) FROM alerts;'

ssh-keyscan -t ed25519 yourhost.netcup.net \
  > /etc/alertbot-backup-known-hosts
ssh-keygen -lf /etc/alertbot-backup-known-hosts
# Compare this fingerprint with the provider's value over a separate,
# trusted channel before continuing.
chown root:root /etc/alertbot-backup-known-hosts
chmod 0644 /etc/alertbot-backup-known-hosts
host_key_sha256=$(ssh-keygen -lf /etc/alertbot-backup-known-hosts \
  | awk 'NR == 1 {print $2}')

install -o root -g root -m 0600 /dev/null /etc/alertbot-backup.env
cat > /etc/alertbot-backup.env <<EOF
BACKUP_NETCUP_HOST=yourhost.netcup.net
BACKUP_NETCUP_USER=<sftp user>
BACKUP_NETCUP_PASS=<sftp password>
BACKUP_NETCUP_PATH=backups/alertbot
BACKUP_KNOWN_HOSTS=/etc/alertbot-backup-known-hosts
BACKUP_SFTP_HOSTKEY_SHA256=${host_key_sha256#SHA256:}
RETENTION_DAYS=30
EOF
unset host_key_sha256

# Keep the private signing key root-only. systemd exposes it only to the
# dedicated backup service through LoadCredential.
umask 077
install -o root -g root -m 0700 -d /etc/alertbot-backup
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 \
  -out /etc/alertbot-backup/signing-private.pem
openssl pkey -in /etc/alertbot-backup/signing-private.pem -pubout \
  -out /etc/alertbot-backup/signing-public.pem
chown root:root /etc/alertbot-backup/signing-*.pem
chmod 0600 /etc/alertbot-backup/signing-private.pem
chmod 0644 /etc/alertbot-backup/signing-public.pem

systemctl daemon-reload
systemctl enable --now alertbot-backup.timer
systemctl start alertbot-backup.service        # test now
journalctl -u alertbot-backup -n 30
```

Daily from 03:30 UTC, with up to 15 minutes randomized delay, the timer creates
a PostgreSQL custom-format dump. The script signs its SHA-256 hash with RSA. It
uploads the dump and detached signature as `.part` files. Separate renames
publish the signature first and the dump last. The script prunes matching
`.dump` and `.dump.sig` files after `RETENTION_DAYS`.

The unit runs as `alertbot-backup`. PostgreSQL peer authentication maps that
Unix account to the matching database role. That role has only `CONNECT`,
schema `USAGE`, and `SELECT` grants; it cannot assume the application owner.
The `postgres` superuser is not required. The bot's Unix account cannot read
the SFTP environment file or private signing key.

### Restoring from backup

```bash
set -euo pipefail
restore_dir=$(mktemp -d /root/alertbot-restore.XXXXXX)
bot_stopped=0
cleanup_restore() {
    rm -rf -- "$restore_dir"
    if (( bot_stopped )); then
        systemctl start alert-bot
    fi
}
trap cleanup_restore EXIT
backup="$restore_dir/alertbot.dump"
signature="$backup.sig"
host_key_sha256=$(sed -n \
  's/^BACKUP_SFTP_HOSTKEY_SHA256=//p' /etc/alertbot-backup.env)
[[ "$host_key_sha256" =~ ^[A-Za-z0-9+/]{43}$ ]]

read -r -p 'SFTP user: ' BACKUP_NETCUP_USER
curl --fail --show-error --silent \
    --connect-timeout 15 --max-time 1800 --max-filesize 1073741824 \
    --user "$BACKUP_NETCUP_USER" \
    --hostpubsha256 "$host_key_sha256" \
    --output "$backup" \
    "sftp://your-netcup-host/backups/alertbot/alertbot-2026-05-09T033000Z.dump"
curl --fail --show-error --silent \
    --connect-timeout 15 --max-time 60 --max-filesize 16384 \
    --user "$BACKUP_NETCUP_USER" \
    --hostpubsha256 "$host_key_sha256" \
    --output "$signature" \
    "sftp://your-netcup-host/backups/alertbot/alertbot-2026-05-09T033000Z.dump.sig"

# Abort before stopping the bot if either file is missing or manipulated.
openssl dgst -sha256 -verify /etc/alertbot-backup/signing-public.pem \
  -signature "$signature" "$backup"

systemctl stop alert-bot
bot_stopped=1
sudo -u alertbot pg_restore \
  --exit-on-error --single-transaction --clean --if-exists \
  --no-owner --no-privileges --dbname=alertbot "$backup"
systemctl start alert-bot
bot_stopped=0
cleanup_restore
trap - EXIT
```

Never restore an unsigned dump. Never use a public key downloaded beside the
dump. Only the pinned local public key proves that the backup host did not
replace the archive. `pg_restore --clean --if-exists` drops and recreates the
backed-up objects. It runs as the narrowly privileged application owner, not as
the PostgreSQL superuser. First preserve any current data you still need. A
failed restore leaves the bot stopped so it cannot use a partially restored
database.

## Update workflow (CI/CD)

**Why build in CI and not on the box?** The production LXC is deliberately
small, with no Rust toolchain or build dependencies. Compiling ~250
crates there is a non-starter and would force the container to carry a whole
build chain it never needs at runtime. So the ephemeral GitHub runner does the
entire build, and only the stripped release binary (a few MB) is shipped to the
LXC. The container stays minimal: it runs the binary and Postgres, nothing else
— smaller footprint, smaller attack surface, faster restores.

Pull requests run the complete check suite. A push to protected `master`
adds a release build and deploy. The full pipeline is in
[`.github/workflows/deploy.yml`](.github/workflows/deploy.yml):

```mermaid
flowchart TD
    pr["pull request to master"] --> test
    push["push to protected master"] --> test

    subgraph test["Job: test · ubuntu-22.04"]
        t1[checkout + Rust toolchain] --> t2[restore crate cache]
        t2 --> t3["./check, including PostgreSQL integration tests"]
    end

    test -->|"push only"| build

    subgraph build["Job: build · no production secrets"]
        b1["cargo build --release + strip"] --> b2["upload short-lived artifact"]
    end

    build --> deploy

    subgraph deploy["Job: deploy · protected production environment"]
        d1["download artifact"] --> d2["verify pinned cloudflared"]
        d2 --> d3["strict SSH host-key check"]
        d3 --> d4["stream into forced command"]
        d4 --> d5["bounded atomic install + restart"]
    end

    d5 -.->|deploys onto| host[("alert-bot LXC<br/>no public SSH port")]
```

1. **Test** — workspace tests, isolated PostgreSQL integration tests, and offline
   backup failure tests.
2. **Build** — `cargo build --release --locked --bin alert-bot` on pinned
   `ubuntu-22.04`, then upload a one-day artifact. This job has no production
   secrets, so project build code cannot read them.
3. **Deploy** — after the protected `production` environment approves the job,
   download the artifact, verify the pinned `cloudflared` binary, verify the
   LXC host key, and stream the binary into the forced deploy command. That
   command enforces the size and ELF format, installs atomically, restarts only
   `alert-bot`, and requires an active unit. The deploy does not print
   service-token details or production journals.

No GitHub Release roundtrip — keeps the repo free to stay private.

### Manual redeploy

GitHub → Actions → "Build and deploy to production" → open the successful,
post-hardening `master` run for the currently deployed commit → **Re-run all
jobs**. This keeps the original commit and ref. Do not rerun historical deploy
runs from before this hardening; cancel them and rotate their secrets. Use a
revert pull request for an older version instead of rerunning an old workflow.

### Tail logs from your dev machine

```bash
./scripts/logs.sh                       # default: alert-bot, last 100 + follow
./scripts/logs.sh postgresql            # postgres logs
./scripts/logs.sh alertbot-backup 50    # last backup run
```

Requires `LXC_HOST` exported in your shell, or set it once in
`scripts/local.env` (gitignored). Defaults to a placeholder otherwise.

### Rollback

Revert the faulty change through a pull request and merge the revert into
protected `master`. The normal pipeline then builds and deploys the reviewed
rollback.

An older binary is safe only when every applied database migration is backward
compatible with it. Never edit an applied migration: add a new migration for
every schema change. If a rollback crosses an incompatible migration, restore a
matching database backup as part of a planned maintenance window.

## Architecture

```
crates/
├── parser/    deterministic time-expression parser, DE + EN, fuzzy keywords
├── core/      domain types — User, Alert, AlertScope, Schedule
├── storage/   sqlx-backed Postgres repository
└── bot/       teloxide command handlers, callback queries, delivery worker
```

- **Webhook** is served by `teloxide`'s `webhooks::axum` listener with the
  Telegram secret-token header verified on every request.
- **Worker** sleeps until the earliest pending alert's `fire_at` or until
  Postgres `NOTIFY alerts_changed` wakes it. Claimed alerts cycle
  `pending → claimed → sent`. Each worker claims one alert at a time; every
  claim increments `claim_generation` (migration 0005). Completion/retry writes
  require the same generation and claimed state, fencing out old workers.
  Transient failures are rescheduled with
  exponential backoff, permanent ones (`BotBlocked`, `ChatNotFound`,
  `UserDeactivated`) are marked `failed`.
- **Reaper** unsticks claims left behind by crashed workers (default 60s
  timeout) and purges old `processed_updates` rows.
- **Task lifecycle** — cancellation is retained while tasks are busy. A normal
  shutdown lets the worker finish its current delivery; unexpected worker exit
  fails the process so `Restart=on-failure` can restart it.
- **Update deduplication** — `processed_updates` records IDs before handlers run.
  This suppresses repeated input, but currently also suppresses retries after
  handler errors (F07 in `BACKLOG.md`). Delivery can still duplicate when
  Telegram accepts a send but its response or the database finalisation is lost;
  claim fencing does not provide exactly-once delivery.

## Development

Requires Rust ≥ 1.86.

```bash
cargo test --workspace --locked
python3 -m unittest discover -s tests -v
# Dedicated test Postgres: DATABASE_URL must use a role with CREATEDB.
# sqlx creates and cleans a separate database per integration test.
DATABASE_URL=postgres://test_user:password@127.0.0.1:5432/postgres cargo test --workspace --locked -- --ignored
cargo run -p alert-bot      # local bot, needs DATABASE_URL + BOT_TOKEN in env
cargo clippy --workspace --all-targets
cargo fmt
```

The parser is isolated from async and I/O, with deterministic tests and a
Criterion benchmark (`cargo bench -p parser`). No fuzz target is checked in yet.
PostgreSQL integration tests are ignored in the default run and run explicitly
in CI; the worker test uses a local mock Telegram endpoint. No real bot is needed.

## Remaining work

Open review findings and feature ideas are maintained locally in `BACKLOG.md`
(not versioned).

## License

MIT OR Apache-2.0
