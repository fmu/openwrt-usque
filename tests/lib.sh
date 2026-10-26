#!/usr/bin/env bash
# Shared helpers for the owrt-usque on-router test scripts.
# Source this file: `source "$(dirname "$0")/lib.sh"`.
#
# All router access goes through ssh (dropbear has no sftp-server), so files
# are moved with `cat >` / `cat <` or tar-over-ssh.

ROUTER="${ROUTER:-root@192.168.1.1}"
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new)
TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$TESTS_DIR/.." && pwd)"
ART_DIR="$TESTS_DIR/artifacts"
BASE_DIR="$TESTS_DIR/baseline"
ROUTER_TMP=/tmp/usque-test

mkdir -p "$ART_DIR" "$BASE_DIR"

# rt <command...> : run a shell command on the router
rt() {
  ssh "${SSH_OPTS[@]}" "$ROUTER" "$@" 2>&1
}

# rtq : quiet, stdin-fed remote shell (use with heredoc)
rtin() {
  ssh "${SSH_OPTS[@]}" "$ROUTER" 'sh -s'
}

# rtpush <local> <remote> : copy a file to the router without scp
rtpush() {
  local src=$1 dst=$2
  ssh "${SSH_OPTS[@]}" "$ROUTER" "mkdir -p ${ROUTER_TMP}; cat > '$dst'" < "$src"
}

# rtget <remote> <local>
rtget() {
  ssh "${SSH_OPTS[@]}" "$ROUTER" "cat '$1'" > "$2"
}

log()  { printf '\n### %s\n' "$*"; }
ok()   { printf '  [PASS] %s\n' "$*"; }
bad()  { printf '  [FAIL] %s\n' "$*"; }
info() { printf '  [ .. ] %s\n' "$*"; }

# check <description> <expected-fixed-string> <actual-text>
check_contains() {
  local desc=$1 want=$2 got=$3
  if printf '%s' "$got" | grep -qF -- "$want"; then ok "$desc"; return 0; fi
  bad "$desc (wanted '$want')"; printf '%s\n' "$got" | sed 's/^/        | /' ; return 1
}

# Redact credentials from any config blob printed into evidence.
redact_config() {
  sed -E 's/("(private_key|access_token|refresh_token|license|id)"[[:space:]]*:[[:space:]]*")[^"]*/\1<redacted>/g'
}
