# alert-bot-rs

[![CI](https://github.com/duramson/alert-bot-rs/actions/workflows/deploy.yml/badge.svg)](https://github.com/duramson/alert-bot-rs/actions/workflows/deploy.yml)
[![Rust](https://img.shields.io/badge/rust-1.96%2B-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
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
  - `/galert` is a shared group reminder; any current member can cancel it,
    snooze it, or stop its series. The bot must be a group administrator so it
    can check membership with Telegram. Failed checks leave the reminder
    unchanged. When upgrading, make the bot an administrator in every group
    that already has `/galert` reminders; otherwise members may be unable to
    cancel, snooze, or stop them.
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

On the Proxmox host, use the community-scripts PostgreSQL helper — it
gives you a Debian-12 LXC with Postgres 16 pre-installed and the standard
`local all all peer` line in `pg_hba.conf`:

```bash
bash -c "$(wget -qLO - https://community-scripts.org/scripts/postgresql)"
```

Pick the non-Alpine variant. The size difference is irrelevant on a
dedicated DB LXC and Debian's glibc matches what GitHub Actions produces
for the bot binary. Defaults are fine (2 CPU / 2 GB / 8 GB disk).

Use PostgreSQL 16.15, 17.11, 18.6, or a later minor release of those major
versions. Older releases lack the security fixes of 2026-08-13. Update the
LXC's packages if the installed minor version is older. The same floors apply
to the client package that provides `pg_dump`.

Note the LXC's IP — the script prints it at the end.

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
identity to the calling system user. To keep it that way, put these rules
before every generic `local` or `host` rule in
`/etc/postgresql/<major>/main/pg_hba.conf` and reload PostgreSQL. They let the
`alertbot` and `alertbot-backup` roles reach only their own database over the
socket and reject them on TCP, so a later password rule can't become a second
way in:

```text
local   alertbot   alertbot,alertbot-backup   peer
local   all        alertbot,alertbot-backup   reject
host    all        alertbot,alertbot-backup   0.0.0.0/0   reject
host    all        alertbot,alertbot-backup   ::0/0       reject
```

The bot sets `lock_timeout = 5s` and `statement_timeout = 30s` on every
connection, so a stuck lock can't hold the pool forever. The backup role
receives only the separate read grants in step 8.

#### 3. Install the systemd unit + config file

The repo lives only on your dev machine; the LXC only needs the unit
files and a config file. Copy them over the first time:

```bash
# from your dev machine
scp scripts/systemd/alert-bot.service       root@<lxc-ip>:/etc/systemd/system/
scp scripts/systemd/alertbot-backup.service root@<lxc-ip>:/etc/systemd/system/
scp scripts/systemd/alertbot-backup.timer   root@<lxc-ip>:/etc/systemd/system/
scp scripts/backup-postgres.sh              root@<lxc-ip>:/usr/local/bin/alertbot-backup.sh
```

```bash
# on the LXC
mkdir -p /etc/alert-bot
cat > /etc/alert-bot/config.env <<EOF
BOT_TOKEN=<from BotFather>
DATABASE_URL=postgres:///alertbot?host=/var/run/postgresql
ADMIN_CHAT_ID=<your numeric Telegram user id>
RUST_LOG=info,alert_bot=debug
# WEBHOOK_URL=         # added in step 5
# WEBHOOK_SECRET=      # added in step 5
EOF
chmod 600 /etc/alert-bot/config.env
chown -R alertbot:alertbot /etc/alert-bot

chmod +x /usr/local/bin/alertbot-backup.sh
systemctl daemon-reload
```

The bot isn't installed yet — that's step 4.

#### 4. Install the first binary

Until CI is wired up (step 7), build on a Linux machine compatible with the
LXC architecture and glibc, then scp the binary across. CI later builds on
Ubuntu and copies directly from the runner (step 7).

```bash
# on the compatible Linux build machine
cargo build --release --bin alert-bot
strip target/release/alert-bot
scp target/release/alert-bot root@<lxc-ip>:/tmp/alert-bot
```

```bash
# on the LXC
install -m 0755 -o root -g root /tmp/alert-bot /usr/local/bin/alert-bot
rm /tmp/alert-bot

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
> scp's straight to the LXC after build.

#### 5. Cloudflare Tunnel route → webhook switch

In the Cloudflare Zero Trust dashboard for the existing cloudflared LXC:

- Tunnel → Public Hostnames → Add
- Subdomain: `alert` (or whatever), Domain: yours
- Service: HTTP, URL: `<bot-lxc-ip>:8080`

Then on the LXC:

```bash
nano /etc/alert-bot/config.env
# WEBHOOK_URL=https://alert.yourdomain.de/webhook
# WEBHOOK_SECRET=$(openssl rand -hex 32)

systemctl restart alert-bot

# verify
source /etc/alert-bot/config.env
curl -s "https://api.telegram.org/bot${BOT_TOKEN}/getWebhookInfo" | jq
```

`url` should match your Cloudflare hostname, `pending_update_count`
should be `0`.

#### 6. Migrate data from the old host (skip if starting fresh)

Use a new LXC with the empty `alertbot` database and access controls from
steps 1–3. Do not start its bot or enable its backup timer before the restore.
Keep the old database for rollback. An archive from an older schema must not
be restored over a newer schema: foreign keys from tables absent in the
archive can prevent `pg_restore --clean` from removing older objects.

On the source machine (old docker-on-LXC setup):

```bash
docker compose stop bot
docker compose exec -T postgres \
    pg_dump -U alertbot --format=custom --no-owner --no-privileges --no-blobs alertbot \
    > alertbot-migration.dump
scp alertbot-migration.dump root@<new-lxc-ip>:/root/alertbot-migration.dump
```

On the new LXC:

```bash
( # The subshell keeps set -e from closing your login shell afterwards.
set -euo pipefail
migration=/root/alertbot-migration.dump
test -f "$migration"
test ! -L "$migration"
chmod 0600 "$migration"
target_tables=$(sudo -u alertbot psql -XAt -v ON_ERROR_STOP=1 -d alertbot \
    -c "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_toast%' AND c.relkind IN ('r','p','v','m','S','f');")
[[ "$target_tables" == 0 ]] || { echo 'Restore requires an empty replacement database' >&2; exit 1; }
systemctl stop alert-bot
cleanup_migration() {
    rm -f -- "$migration"
}
trap cleanup_migration EXIT
sudo -u alertbot pg_restore --exit-on-error --single-transaction \
    --no-owner --no-privileges \
    --dbname=alertbot < "$migration"
sudo -u alertbot psql -d alertbot \
    -c "SELECT count(*), max(created_at) FROM alerts;"
cleanup_migration
trap - EXIT
)
```

Continue with step 4. The first start applies the missing migrations to the
restored database. Verify its startup and reminder data before changing the
tunnel route. Keep the old bot stopped so only one instance sends reminders.
If migration fails, the new bot stays stopped and the old database is intact.

#### 7. Cloudflare Access SSH for CI deploys

The GitHub Actions workflow connects to the LXC over Cloudflare Access
SSH — no SSH port needs to be exposed on the LXC.

**Generate a dedicated key pair** (don't reuse your personal one):

```bash
ssh-keygen -t ed25519 -f ~/.ssh/alertbot-deploy -C "github-actions-deploy"
ssh-copy-id -i ~/.ssh/alertbot-deploy.pub root@<lxc-ip>
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

**GitHub repository secrets/variables.** Settings → Secrets and variables
→ Actions:

| Type     | Name                          | Value                                    |
|----------|-------------------------------|------------------------------------------|
| Secret   | `LXC_SSH_KEY`                 | contents of `~/.ssh/alertbot-deploy`     |
| Secret   | `CF_ACCESS_CLIENT_ID`         | from the Service Token                   |
| Secret   | `CF_ACCESS_CLIENT_SECRET`     | from the Service Token                   |
| Variable | `SSH_HOST`                    | `ssh-alert.yourdomain.de`                |
| Variable | `SSH_USER`                    | `root`                                   |

The next code push to `master` triggers tests → build → direct scp/SSH
deployment via Cloudflare Access. There is no GitHub Release step.

#### 8. Nightly off-site backup → Netcup SFTP

Generate the `age` identity on a separate trusted offline system:

```bash
umask 077
age-keygen -o alertbot-backup.agekey
age-keygen -y alertbot-backup.agekey > alertbot-backup-recipient.txt
```

Store `alertbot-backup.agekey` offline. Copy only the single `age1...`
recipient from `alertbot-backup-recipient.txt` into
`BACKUP_AGE_RECIPIENT` below. The identity must never reside on the production
LXC.

Configure the production LXC:

```bash
apt install -y age curl openssl sshpass postgresql-client-16
pg_dump --version  # must report at least 16.15

# Keep backup credentials outside the bot's Unix identity.
useradd --system --no-create-home --shell /usr/sbin/nologin alertbot-backup
sudo -u postgres createuser --no-createdb --no-createrole --no-superuser \
  --connection-limit=1 alertbot-backup
sudo -u postgres psql -v ON_ERROR_STOP=1 --dbname alertbot <<'SQL'
ALTER ROLE "alertbot-backup" SET temp_file_limit = '256MB';
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
# The backup identity must not reach another database or connect over TCP.
# Every check must fail.
! sudo -u alertbot-backup psql --dbname postgres --command 'SELECT 1;'
! sudo -u alertbot-backup psql \
  "hostaddr=127.0.0.1 dbname=alertbot user=alertbot-backup connect_timeout=2" \
  --command 'SELECT 1;'
! sudo -u alertbot-backup psql \
  "hostaddr=::1 dbname=alertbot user=alertbot-backup connect_timeout=2" \
  --command 'SELECT 1;'

# Existing installations should return no rows here. Review unexpected objects
# before removing them with lo_unlink; --no-blobs keeps them out of backups but
# does not reclaim their primary-database storage.
sudo -u postgres psql --dbname alertbot --command \
  'SELECT oid, pg_get_userbyid(lomowner) AS owner FROM pg_largeobject_metadata;'

ssh-keyscan -t ed25519 yourhost.netcup.net \
  > /etc/alertbot-backup-known-hosts
ssh-keygen -lf /etc/alertbot-backup-known-hosts
# Compare this fingerprint with the provider's value over a separate,
# trusted channel before continuing.
chown root:root /etc/alertbot-backup-known-hosts
chmod 0644 /etc/alertbot-backup-known-hosts

install -o root -g root -m 0600 /dev/null /etc/alertbot-backup.env
cat > /etc/alertbot-backup.env <<EOF
BACKUP_NETCUP_HOST=yourhost.netcup.net
BACKUP_NETCUP_USER=<sftp user>
BACKUP_NETCUP_PASS=<sftp password>
BACKUP_NETCUP_PATH=backups/alertbot
BACKUP_KNOWN_HOSTS=/etc/alertbot-backup-known-hosts
BACKUP_AGE_RECIPIENT=<age1 recipient generated offline>
RETENTION_DAYS=30
EOF

# Keep the private signing key root-only. systemd exposes it only to the
# dedicated backup service through LoadCredential. Keep the public verification
# key under the separate restore trust path.
umask 077
install -o root -g root -m 0700 -d /etc/alertbot-backup
install -o root -g root -m 0755 -d /etc/alertbot-restore
# A replacement LXC after a restore already holds the previous public key. It
# still authenticates every older backup, so keep it under a dated name.
if [[ -e /etc/alertbot-restore/signing-public.pem ]]; then
  mv /etc/alertbot-restore/signing-public.pem \
    "/etc/alertbot-restore/signing-public-until-$(date -u +%Y%m%dT%H%M%SZ).pem"
fi
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 \
  -out /etc/alertbot-backup/signing-private.pem
openssl pkey -in /etc/alertbot-backup/signing-private.pem -pubout \
  -out /etc/alertbot-restore/signing-public.pem
chown root:root /etc/alertbot-backup/signing-private.pem \
  /etc/alertbot-restore/signing-public.pem
chmod 0600 /etc/alertbot-backup/signing-private.pem
chmod 0644 /etc/alertbot-restore/signing-public.pem

systemctl daemon-reload
systemctl enable --now alertbot-backup.timer
systemctl start alertbot-backup.service        # test now
journalctl -u alertbot-backup -n 30
! systemctl is-failed --quiet alertbot-backup.service
```

Copy `/etc/alertbot-restore/signing-public.pem` and
`/etc/alertbot-backup-known-hosts` to an independently trusted place off the
LXC, for example next to the offline `age` identity. A restore after losing the
LXC needs both, and neither may come from the SFTP archive. Keep every earlier
public key as well: a backup verifies only with the key that signed it.

`BACKUP_NETCUP_PATH` names an absolute directory inside the SFTP server's
visible filesystem, with the leading slash omitted in the configuration.
Upload, rename, retention, and the restore URLs below all use
`/backups/alertbot`, regardless of the SFTP account's initial directory.

Daily from 03:30 UTC, with up to 15 minutes randomized delay, the timer creates
a PostgreSQL custom-format dump in a service-private, mode-0700 temporary
directory. It encrypts the dump with `age` and deletes the plaintext
immediately. The script creates a
manifest for the encrypted `.dump.age` file. The manifest binds the format
version, database, canonical file name, UTC creation time, ciphertext byte
size, and SHA-256 hash. It signs that manifest with RSA. It uploads only the
ciphertext, manifest, and detached signature as `.part` files. Separate renames
publish the signature, manifest, and ciphertext in that order. The script
limits a retention listing to 60 seconds and 1 MiB. It prunes every matching
backup part after `RETENTION_DAYS`, including the unencrypted `.sql.gz` dumps
of earlier versions. Uploads, renames, and retention accept only the host keys
in `BACKUP_KNOWN_HOSTS`.

The backup host must provide `pg_dump` 16.15 or newer in major 16, 17.11 or
newer in major 17, or 18.6 or newer in major 18. The script rejects older and
unknown major versions before it reads the database. These floors contain the
official fix for
[CVE-2026-19385](https://www.postgresql.org/support/security/CVE-2026-19385/),
which otherwise lets a crafted database object execute code in `pg_dump`.

The unit runs as `alertbot-backup`. PostgreSQL peer authentication maps that
Unix account to the matching database role. Beyond PostgreSQL's defaults for
`PUBLIC`, that role gets only `CONNECT`, schema `USAGE`, and `SELECT` grants;
it cannot assume the application owner.
It has one connection slot and cannot enter another database.
The dump excludes large objects because this application does not use them.
The `postgres` superuser is not required while a backup runs. Provisioning the
role and its access controls still requires an administrator. The bot's Unix
account cannot read the SFTP environment file or private signing key.

### Restoring from backup

Restore on a replacement LXC with a dedicated, empty `alertbot` database.
Complete steps 1–3 only. Do not run step 4 yet: it starts the bot, which
would migrate the database before restoration.
Do not run step 8 yet. Keep the current database untouched for rollback.
Use the same database name so the documented peer-authentication rules still
apply. The empty-target check below rejects an already migrated database.

Install the restore tools and the trust directory, which step 8 would
otherwise create:

```bash
apt install -y age curl openssl
install -o root -g root -m 0755 -d /etc/alertbot-restore
```

Obtain the restore public key that signed the chosen backup and the SFTP host
key from their independently trusted backup. Install that public key as root-owned
`/etc/alertbot-restore/signing-public.pem` and the verified host key as
root-owned `/etc/alertbot-backup-known-hosts`. Do not create a new signing key
for verification: it cannot authenticate existing backups. Do not obtain the
public key from the SFTP archive directory.

```bash
( # The subshell keeps set -e from closing your login shell afterwards.
set -euo pipefail
restore_dir=$(mktemp -d /root/alertbot-restore.XXXXXX)
identity_mount=/media/alertbot-age
cleanup_restore() {
    local cleanup_status=$?
    if mountpoint -q "$identity_mount"; then
        umount "$identity_mount" || true
    fi
    rm -rf -- "$restore_dir" || true
    return "$cleanup_status"
}
trap cleanup_restore EXIT
backup_name=alertbot-2026-05-09T033000Z.dump.age
[[ "$backup_name" =~ ^alertbot-[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{6}Z\.dump\.age$ ]]
ciphertext="$restore_dir/$backup_name"
backup="$restore_dir/${backup_name%.age}"
manifest="$ciphertext.manifest"
signature="$manifest.sig"
# curl reads SFTP host keys only from $HOME/.ssh/known_hosts and rejects
# every key that is not listed there.
curl_home="$restore_dir/curl-home"
install -d -m 0700 "$curl_home" "$curl_home/.ssh"
install -m 0600 /etc/alertbot-backup-known-hosts "$curl_home/.ssh/known_hosts"

read -r -p 'SFTP user: ' BACKUP_NETCUP_USER
HOME="$curl_home" curl --fail --show-error --silent \
    --connect-timeout 15 --max-time 60 --max-filesize 4096 \
    --user "$BACKUP_NETCUP_USER" \
    --output "$manifest" \
    "sftp://yourhost.netcup.net/backups/alertbot/$backup_name.manifest"
HOME="$curl_home" curl --fail --show-error --silent \
    --connect-timeout 15 --max-time 60 --max-filesize 16384 \
    --user "$BACKUP_NETCUP_USER" \
    --output "$signature" \
    "sftp://yourhost.netcup.net/backups/alertbot/$backup_name.manifest.sig"

# Trust metadata only after the pinned restore key verifies its signature.
openssl dgst -sha256 -verify /etc/alertbot-restore/signing-public.pem \
  -signature "$signature" "$manifest"
mapfile -t metadata < "$manifest"
(( ${#metadata[@]} == 6 ))
[[ "${metadata[0]}" == "format_version=2" ]]
[[ "${metadata[1]}" == database=* ]]
[[ "${metadata[2]}" == filename=* ]]
[[ "${metadata[3]}" == created_at=* ]]
[[ "${metadata[4]}" == size_bytes=* ]]
[[ "${metadata[5]}" == sha256=* ]]
manifest_database=${metadata[1]#database=}
manifest_filename=${metadata[2]#filename=}
manifest_created_at=${metadata[3]#created_at=}
manifest_size=${metadata[4]#size_bytes=}
manifest_sha256=${metadata[5]#sha256=}
[[ "$manifest_database" == alertbot ]]
[[ "$manifest_filename" == "$backup_name" ]]
[[ "$manifest_created_at" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{6}Z$ ]]
[[ "$manifest_filename" == "alertbot-${manifest_created_at}.dump.age" ]]
[[ "$manifest_size" =~ ^[0-9]+$ ]] && (( manifest_size <= 1073741824 ))
[[ "$manifest_sha256" =~ ^[0-9a-f]{64}$ ]]

HOME="$curl_home" curl --fail --show-error --silent \
    --connect-timeout 15 --max-time 1800 --max-filesize 1073741824 \
    --user "$BACKUP_NETCUP_USER" \
    --output "$ciphertext" \
    "sftp://yourhost.netcup.net/backups/alertbot/$backup_name"
actual_size=$(stat -c %s "$ciphertext")
actual_sha256=$(sha256sum "$ciphertext")
actual_sha256=${actual_sha256%% *}
[[ "$actual_size" == "$manifest_size" ]]
[[ "$actual_sha256" == "$manifest_sha256" ]]

# Attach the offline identity on read-only removable media only for this step.
age_identity="$identity_mount/alertbot-backup.agekey"
mountpoint -q "$identity_mount"
findmnt -no OPTIONS "$identity_mount" | tr ',' '\n' | grep -qx ro
[[ -r "$age_identity" ]]
age --decrypt --identity "$age_identity" --output "$backup" "$ciphertext"
unset age_identity
umount "$identity_mount"
# Remove the media before you continue. A failed decrypt exits before this
# point, so it cannot stop the bot or touch the database.
read -r -p 'Offline identity media removed? [y/N] ' media_removed
[[ "$media_removed" == y ]]
unset media_removed

target_tables=$(sudo -u alertbot psql -XAt -v ON_ERROR_STOP=1 -d alertbot \
    -c "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_toast%' AND c.relkind IN ('r','p','v','m','S','f');")
[[ "$target_tables" == 0 ]] || { echo 'Restore requires an empty replacement database' >&2; exit 1; }
systemctl stop alert-bot
sudo -u alertbot pg_restore \
  --exit-on-error --single-transaction \
  --no-owner --no-privileges --dbname=alertbot < "$backup"
sudo -u alertbot psql -X -v ON_ERROR_STOP=1 -d alertbot \
  -c "SELECT count(*), max(created_at) FROM alerts;"
cleanup_restore
trap - EXIT
)
```

Never restore a dump without its signed manifest. Never use a public key
downloaded beside the backup. Only the pinned key under `/etc/alertbot-restore`
authenticates the manifest. Its file name, database, byte size, and hash checks
reject renamed or manipulated ciphertext before decryption. `age` decrypts the
verified ciphertext into the root-only temporary directory before the bot
stops. A failed signature, hash check, or decryption leaves the database and bot
untouched. Keep the `age` identity offline. Attach its read-only removable media
only for a manual restore, unmount it immediately after decryption, and never
copy it onto the production LXC.

`pg_restore` creates the backed-up objects in the empty database. It runs as
the application owner, not as the PostgreSQL superuser. The root shell opens
the verified, root-only archive and passes it
through standard input, so the database user never needs access to `/root`.
After restoration, stop the previous bot through its management connection.
Then complete step 4 on the replacement: it installs the binary and starts
the bot, which applies any missing migrations. Check startup, database
contents, and the webhook configuration before switching the tunnel route.
Only then provision the backup role and timer from step 8. Keep the previous
instance stopped but retain its database until the replacement is accepted.
If restoration fails, the
cleanup trap removes the downloaded files and leaves the bot stopped. Check
the replacement database before retrying; do not erase the previous instance.

## Update workflow (CI/CD)

**Why build in CI and not on the box?** The production LXC is deliberately
small, with no Rust toolchain or build dependencies. Compiling ~250
crates there is a non-starter and would force the container to carry a whole
build chain it never needs at runtime. So the ephemeral GitHub runner does the
entire build, and only the stripped release binary (a few MB) is shipped to the
LXC. The container stays minimal: it runs the binary and Postgres, nothing else
— smaller footprint, smaller attack surface, faster restores.

Push to `master` → GitHub Actions builds → runner scp's straight to the
LXC and restarts. The full pipeline is in
[`.github/workflows/deploy.yml`](.github/workflows/deploy.yml):

```mermaid
flowchart TD
    trig["push to main / master<br/>(skips *.md, docs/, .gitignore)<br/>· or manual workflow_dispatch"] --> test

    subgraph test["Job: test · ubuntu-22.04"]
        t1[checkout + Rust toolchain] --> t2[restore crate cache]
        t2 --> t3["workspace tests + PostgreSQL integration tests + backup regression tests"]
    end

    test -->|"needs: test"| deploy

    subgraph deploy["Job: deploy · ubuntu-22.04 · glibc 2.35 pin"]
        d1["cargo build --release + strip"] --> d2[install cloudflared]
        d2 --> d3["write deploy SSH key<br/>secrets.LXC_SSH_KEY"]
        d3 --> d4["sanity-check Cloudflare<br/>Access service token"]
        d4 --> d5["scp binary to LXC<br/>over Cloudflare Access SSH"]
        d5 --> d6["ssh: install + systemctl restart"]
        d6 --> d7["tail journalctl ~20s"]
    end

    d6 -.->|deploys onto| host[("alert-bot LXC<br/>no public SSH port")]
```

1. **Test** — workspace tests, isolated PostgreSQL integration tests, and offline
   backup failure tests.
2. **Build & deploy** (one job, so the runner can scp the binary it just
   built):
   1. `cargo build --release --bin alert-bot` on `ubuntu-22.04`.
      Pinned so the binary's glibc baseline (2.35) stays compatible
      with Debian 13 (2.41 — backward-compatible). If we let
      `ubuntu-latest` drift past Debian, deploy silently breaks.
   2. `strip target/release/alert-bot`
   3. `scp` the binary to the LXC over Cloudflare Access SSH.
   4. On the LXC: `install -m 0755` to `/usr/local/bin/alert-bot`,
      then `systemctl restart alert-bot`.
3. **Tail** — 20s of `journalctl -u alert-bot -f` for inspection. This currently
   does not fail the job on a later startup error; readiness checks remain open
   in `BACKLOG.md`.

No GitHub Release roundtrip — keeps the repo free to stay private.

### Manual trigger

The workflow has `workflow_dispatch` so you can re-deploy any commit
from the Actions tab without pushing — useful for retrying a flaky
deploy or re-applying after a Cloudflare config change.

### Tail logs from your dev machine

```bash
./scripts/logs.sh                       # default: alert-bot, last 100 + follow
./scripts/logs.sh postgresql            # postgres logs
./scripts/logs.sh alertbot-backup 50    # last backup run
```

Requires `LXC_HOST` exported in your shell, or set it once in
`scripts/local.env` (gitignored). Defaults to a placeholder otherwise.

### Rollback

Trigger the workflow on an older commit:

- GitHub → Actions → "Build and deploy to production" → **Run
  workflow** → pick a branch or tag → run.

That rebuilds + scp's the older commit's binary. Or build locally and
scp manually (same one-liner as step 4 in the production setup above).

Postgres data isn't touched by binary swaps, so code-only rollbacks are
safe. Schema rollbacks need a `pg_dump` restore — keep your daily Netcup
backups close.

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

Requires Rust ≥ 1.96.

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
