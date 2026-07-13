#!/usr/bin/env bash
# nightly.sh - cron/systemd entry point for the nightly DataFusion benchmark run.
#
# The heavy lifting lives in nightly/nightly.py; this wrapper only:
#   1. takes an exclusive flock so overlapping cron fires are harmless
#   2. fixes up cron's minimal PATH so cargo + python3 are visible
#   3. runs `git pull --rebase` so the harness itself is up to date
#      (skipped if the working tree is dirty, e.g. unpublished results;
#      set NIGHTLY_NO_PULL to skip always, e.g. under GitHub Actions checkout)
#   4. runs nightly/nightly.py, teeing output to nightly/logs/wrapper-<date>.log
#   5. propagates nightly.py's exit code; invokes $NIGHTLY_NOTIFY_CMD on
#      failure, and on regression alerts after a successful run
#
# Environment:
#   NIGHTLY_NO_PULL      skip the git pull --rebase step (any non-empty value
#                        other than "0")
#   NIGHTLY_NOTIFY_CMD   generic notification hook: run through
#                        `bash -c "${NIGHTLY_NOTIFY_CMD} \"\$1\"" _ "$msg"`,
#                        so quoting inside the variable works and the short
#                        message arrives as "$1" appended to your command.
#                        Fires when the wrapper or nightly.py fails, and after
#                        a successful run that produced regression alerts.
#                        Inline example (message becomes curl's -d argument):
#                          NIGHTLY_NOTIFY_CMD='curl -fsS https://ntfy.sh/my-topic -d'
#                        For anything that wants the message on stdin (mail,
#                        slack webhooks with JSON, ...) use a wrapper script
#                        that takes it as $1:
#                          NIGHTLY_NOTIFY_CMD='/usr/local/bin/nightly-notify.sh'
#                          # nightly-notify.sh: echo "$1" | mail -s "nightly" ops@example.com
#
# Locking: this script holds flock(1) on nightly/.shlock for its whole lifetime
# (including the git pull). nightly.py additionally creates its own portable
# lock file nightly/.lock. Both are intentional: the flock guards the wrapper,
# the python lock guards direct `python3 nightly/nightly.py` invocations.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

# cron runs with a minimal PATH; make sure cargo (needed by the build step) and
# the usual local bins are visible without hand-editing the crontab.
export PATH="${HOME}/.cargo/bin:/usr/local/bin:${PATH}"

LOG_DIR="${REPO_ROOT}/nightly/logs"
mkdir -p "${LOG_DIR}"
LOG_FILE="${LOG_DIR}/wrapper-$(date -u +%F).log"
LOCK_FILE="${REPO_ROOT}/nightly/.shlock"

log() {
  printf '[nightly.sh %s] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" | tee -a "${LOG_FILE}"
}

notify() {
  # Generic notification hook. NIGHTLY_NOTIFY_CMD is interpreted by bash -c so
  # quoting inside the variable works; the message is appended as "$1".
  if [ -n "${NIGHTLY_NOTIFY_CMD:-}" ]; then
    bash -c "${NIGHTLY_NOTIFY_CMD} \"\$1\"" _ "$1" || log "warning: NIGHTLY_NOTIFY_CMD exited non-zero"
  fi
}

# --- lock --------------------------------------------------------------------
if command -v flock >/dev/null 2>&1; then
  exec 9>"${LOCK_FILE}"
  if ! flock -n 9; then
    log "another nightly.sh run holds ${LOCK_FILE}; exiting (overlap is harmless by design)"
    exit 0
  fi
else
  # flock(1) is util-linux (Linux). On macOS / Git Bash we fall back to
  # nightly.py's own portable lock.
  log "warning: flock(1) not available; relying on nightly.py's own lock only"
fi

# --- keep the harness repo up to date ----------------------------------------
if [ -n "${NIGHTLY_NO_PULL:-}" ] && [ "${NIGHTLY_NO_PULL}" != "0" ]; then
  log "NIGHTLY_NO_PULL=${NIGHTLY_NO_PULL}: skipping git pull --rebase"
elif [ -n "$(git status --porcelain)" ]; then
  # A rebase pull permanently fails on a dirty tree, and dirty is the steady
  # state when publish is disabled (results/ grows every night). Deliberately
  # NOT --autostash: stashing results data and reapplying it over a rebase
  # risks silent data loss on conflict.
  log "warning: working tree dirty (publish disabled?), skipping pull"
else
  log "running git pull --rebase"
  if ! git pull --rebase 2>&1 | tee -a "${LOG_FILE}"; then
    msg="datafusion nightly: git pull --rebase failed on $(hostname 2>/dev/null || echo unknown-host) at $(date -u +%Y-%m-%dT%H:%M:%SZ); see ${LOG_FILE}"
    log "${msg}"
    notify "${msg}"
    exit 1
  fi
fi

# --- run the orchestrator, propagate its exit code ----------------------------
log "starting nightly.py ${*:-}"
rc=0
python3 "${REPO_ROOT}/nightly/nightly.py" ${1+"$@"} 2>&1 | tee -a "${LOG_FILE}" || rc=$?

if [ "${rc}" -ne 0 ]; then
  msg="datafusion nightly: nightly.py exited ${rc} on $(hostname 2>/dev/null || echo unknown-host) at $(date -u +%Y-%m-%dT%H:%M:%SZ); see ${LOG_FILE}"
  log "${msg}"
  notify "${msg}"
else
  # Success: surface regression alerts (nightly.py already treated them as
  # success-with-alerts and recorded the count in status.json).
  alerts="$(python3 -c 'import json, sys
try:
    with open(sys.argv[1]) as fh:
        print(int(json.load(fh).get("alerts") or 0))
except Exception:
    print(0)' "${REPO_ROOT}/nightly/out/status.json" 2>/dev/null || echo 0)"
  if [ "${alerts}" -gt 0 ] 2>/dev/null; then
    msg="nightly: ${alerts} regression alert(s) - see nightly/out/alerts.md"
    log "${msg}"
    notify "${msg}"
  fi
fi
log "done (exit ${rc})"
exit "${rc}"
