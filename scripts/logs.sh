#!/usr/bin/env bash
#
# Tail logs of a systemd service on the production LXC.
#
# Usage:
#   ./scripts/logs.sh                       # default: alert-bot, last 100
#   ./scripts/logs.sh postgresql            # follow postgres logs
#   ./scripts/logs.sh alert-bot 500         # last 500 lines + follow
#   ./scripts/logs.sh alertbot-backup 50    # backup job's last 50 lines

set -euo pipefail

# Echte Ziel-IP steht in scripts/local.env (gitignored), nicht im Repo.
[ -f "$(dirname "$0")/local.env" ] && . "$(dirname "$0")/local.env"
LXC_HOST="${LXC_HOST:-root@<lxc-ip>}"
SERVICE="${1:-alert-bot}"
TAIL_LINES="${2:-100}"

if [[ ! "$SERVICE" =~ ^[A-Za-z0-9][A-Za-z0-9_.@:-]*$ ]]; then
    echo "invalid systemd service name" >&2
    exit 2
fi

if [[ ! "$TAIL_LINES" =~ ^[1-9][0-9]{0,4}$ ]] || (( TAIL_LINES > 10000 )); then
    echo "tail line count must be between 1 and 10000" >&2
    exit 2
fi

# OpenSSH joins remote arguments into a shell command. The strict allowlists
# above make every interpolated value inert in that shell.
exec ssh -t "$LXC_HOST" "exec journalctl --unit=${SERVICE} --lines=${TAIL_LINES} --follow"
