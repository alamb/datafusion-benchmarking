#!/usr/bin/env bash
# check_env.sh - read-only benchmark-box environment assertions.
#
# Verifies the low-noise state that tune.sh establishes, plus machine
# quietness (login sessions, load average). Root is NOT required: this script
# only reads. It never changes anything - run `sudo ./nightly/tune.sh` to fix
# failures.
#
# Output: one "PASS: ..." / "FAIL: ..." line per check, then a greppable
# one-line JSON snapshot:
#   ENV_SNAPSHOT: {"governor": ..., "turbo": ..., "aslr": ..., "smt": ...,
#                  "load1": ..., "nproc": ..., "kernel": ...}
#
# Exit codes: 0 = all checks pass (or SKIP on non-Linux), 1 = at least one FAIL.
set -euo pipefail

if [ "$(uname -s)" != "Linux" ]; then
  echo "check_env.sh: SKIP (not Linux: $(uname -s); environment checks only apply to the Linux benchmark box)"
  exit 0
fi

FAILURES=0

check() {
  # check <name> <pass|fail> <detail>
  local name="$1" result="$2" detail="$3"
  if [ "${result}" = "pass" ]; then
    echo "PASS: ${name} (${detail})"
  else
    echo "FAIL: ${name} (${detail})"
    FAILURES=$((FAILURES + 1))
  fi
}

# --- cpufreq governor ---------------------------------------------------------
governors="$(cat /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor 2>/dev/null | sort -u | paste -sd, - || true)"
[ -n "${governors}" ] || governors="unknown"
if [ "${governors}" = "performance" ]; then
  check "cpufreq governor" pass "performance on all online CPUs"
else
  check "cpufreq governor" fail "want 'performance' on every CPU, got: ${governors}"
fi

# --- turbo / boost --------------------------------------------------------------
turbo="unknown"
turbo_detail=""
if [ -e /sys/devices/system/cpu/intel_pstate/no_turbo ]; then
  no_turbo="$(cat /sys/devices/system/cpu/intel_pstate/no_turbo 2>/dev/null || echo '?')"
  if [ "${no_turbo}" = "1" ]; then turbo="off"; else turbo="on"; fi
  turbo_detail="intel_pstate/no_turbo=${no_turbo}"
elif [ -e /sys/devices/system/cpu/cpufreq/boost ]; then
  boost="$(cat /sys/devices/system/cpu/cpufreq/boost 2>/dev/null || echo '?')"
  if [ "${boost}" = "0" ]; then turbo="off"; else turbo="on"; fi
  turbo_detail="cpufreq/boost=${boost}"
else
  turbo="none"
  turbo_detail="no turbo/boost control file on this kernel/driver"
fi
if [ "${turbo}" = "off" ] || [ "${turbo}" = "none" ]; then
  check "turbo/boost disabled" pass "${turbo_detail}"
else
  check "turbo/boost disabled" fail "${turbo_detail}"
fi

# --- ASLR -----------------------------------------------------------------------
aslr="$(cat /proc/sys/kernel/randomize_va_space 2>/dev/null || echo unknown)"
if [ "${aslr}" = "0" ]; then
  check "ASLR disabled" pass "kernel.randomize_va_space=0"
else
  check "ASLR disabled" fail "kernel.randomize_va_space=${aslr} (want 0)"
fi

# --- SMT ------------------------------------------------------------------------
smt="$(cat /sys/devices/system/cpu/smt/control 2>/dev/null || echo unknown)"
case "${smt}" in
  off|forceoff|notsupported|notimplemented)
    check "SMT disabled" pass "smt/control=${smt}"
    ;;
  *)
    check "SMT disabled" fail "smt/control=${smt} (want off/forceoff)"
    ;;
esac

# --- irqbalance -------------------------------------------------------------------
if command -v systemctl >/dev/null 2>&1; then
  irq="$(systemctl is-active irqbalance 2>/dev/null || true)"
  [ -n "${irq}" ] || irq="unknown"
elif pgrep -x irqbalance >/dev/null 2>&1; then
  irq="active"
else
  irq="inactive"
fi
if [ "${irq}" = "active" ] || [ "${irq}" = "activating" ]; then
  check "irqbalance stopped" fail "irqbalance is ${irq}"
else
  check "irqbalance stopped" pass "irqbalance is ${irq}"
fi

# --- login sessions ----------------------------------------------------------------
sessions="$(who 2>/dev/null | wc -l | tr -d '[:space:]' || true)"
[ -n "${sessions}" ] || sessions=0
if [ "${sessions}" -le 1 ]; then
  check "no other login sessions" pass "${sessions} session(s)"
else
  check "no other login sessions" fail "${sessions} login sessions active (want <= 1: nobody else on the box)"
fi

# --- load average --------------------------------------------------------------------
load1="$(cut -d' ' -f1 /proc/loadavg 2>/dev/null || echo -1)"
nproc_val="$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 0)"
load_result="$(awk -v l="${load1}" -v n="${nproc_val}" 'BEGIN { print (n > 0 && l >= 0 && l < n / 4.0) ? "pass" : "fail" }')"
load_threshold="$(awk -v n="${nproc_val}" 'BEGIN { printf "%.2f", n / 4.0 }')"
check "load average low" "${load_result}" "load1=${load1}, want < nproc/4 = ${load_threshold}"

# --- machine-readable snapshot (greppable; nightly.py may capture this line) --------
kernel="$(uname -r)"
printf 'ENV_SNAPSHOT: {"governor": "%s", "turbo": "%s", "aslr": "%s", "smt": "%s", "load1": %s, "nproc": %s, "kernel": "%s"}\n' \
  "${governors}" "${turbo}" "${aslr}" "${smt}" "${load1}" "${nproc_val}" "${kernel}"

if [ "${FAILURES}" -gt 0 ]; then
  echo "check_env.sh: ${FAILURES} check(s) FAILED (run 'sudo ./nightly/tune.sh' and quiesce the box)"
  exit 1
fi
echo "check_env.sh: all checks passed"
