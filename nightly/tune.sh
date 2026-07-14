#!/usr/bin/env bash
# tune.sh - put the Linux benchmark box into a low-noise state, or undo it.
#
# Usage:
#   sudo ./nightly/tune.sh          apply benchmark tuning
#   sudo ./nightly/tune.sh reset    restore everyday defaults
#
# apply (idempotent; safe to re-run every night):
#   * cpufreq governor -> performance on every online CPU
#   * turbo/boost      -> off (intel_pstate/no_turbo=1 OR cpufreq/boost=0,
#                         whichever control file exists)
#   * ASLR             -> off (kernel.randomize_va_space=0)
#   * SMT              -> off (/sys/devices/system/cpu/smt/control)
#   * irqbalance       -> stopped if running (absence is ignored)
#
# reset (best effort):
#   * SMT on; governor schedutil/ondemand/powersave (first one available);
#     turbo/boost on; ASLR back to 2; irqbalance started.
#
# Prints the before/after state. The read-only verification counterpart is
# check_env.sh. On non-Linux this exits 0 with a message; on Linux it requires
# root for the /sys and /proc/sys writes.
set -euo pipefail

MODE="${1:-apply}"
case "${MODE}" in
  apply|reset) ;;
  *)
    echo "Usage: $0 [reset]" >&2
    exit 1
    ;;
esac

if [ "$(uname -s)" != "Linux" ]; then
  echo "tune.sh: not Linux ($(uname -s)); benchmark tuning only applies to the Linux box. Nothing to do."
  exit 0
fi

if [ "$(id -u)" -ne 0 ]; then
  echo "tune.sh: error: root is required to write /sys and /proc/sys tunables. Re-run as: sudo $0 ${MODE}" >&2
  exit 1
fi

write_sysfs() {
  # write_sysfs <value> <path> - best-effort, idempotent, chatty.
  local value="$1" path="$2" current
  if [ ! -e "${path}" ]; then
    echo "  skip: ${path} not present on this kernel/driver"
    return 0
  fi
  current="$(cat "${path}" 2>/dev/null || echo '?')"
  if [ "${current}" = "${value}" ]; then
    echo "  ok:   ${path} already ${value}"
    return 0
  fi
  if printf '%s' "${value}" > "${path}" 2>/dev/null; then
    echo "  set:  ${path} = ${value} (was ${current})"
  else
    echo "  WARNING: could not write '${value}' to ${path} (still ${current})" >&2
  fi
}

set_governor() {
  local governor="$1" f found=0
  for f in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor; do
    [ -e "${f}" ] || continue
    found=1
    write_sysfs "${governor}" "${f}"
  done
  if [ "${found}" -eq 0 ]; then
    echo "  WARNING: no cpufreq scaling_governor files found (no cpufreq driver?)" >&2
  fi
}

default_governor() {
  # Best-effort pick for `reset`: schedutil, then ondemand, then powersave
  # (intel_pstate's non-performance mode). Prints nothing if none is available.
  local avail g
  avail="$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_available_governors 2>/dev/null || echo '')"
  for g in schedutil ondemand powersave; do
    case " ${avail} " in
      *" ${g} "*)
        echo "${g}"
        return 0
        ;;
    esac
  done
  echo ""
}

turbo_off() {
  if [ -e /sys/devices/system/cpu/intel_pstate/no_turbo ]; then
    write_sysfs 1 /sys/devices/system/cpu/intel_pstate/no_turbo
  elif [ -e /sys/devices/system/cpu/cpufreq/boost ]; then
    write_sysfs 0 /sys/devices/system/cpu/cpufreq/boost
  else
    echo "  WARNING: no turbo/boost control found (neither intel_pstate/no_turbo nor cpufreq/boost)" >&2
  fi
}

turbo_on() {
  if [ -e /sys/devices/system/cpu/intel_pstate/no_turbo ]; then
    write_sysfs 0 /sys/devices/system/cpu/intel_pstate/no_turbo
  elif [ -e /sys/devices/system/cpu/cpufreq/boost ]; then
    write_sysfs 1 /sys/devices/system/cpu/cpufreq/boost
  else
    echo "  WARNING: no turbo/boost control found; nothing to restore" >&2
  fi
}

stop_irqbalance() {
  if ! command -v systemctl >/dev/null 2>&1; then
    echo "  skip: systemctl not available; stop irqbalance manually if it runs"
    return 0
  fi
  if systemctl is-active --quiet irqbalance 2>/dev/null; then
    if systemctl stop irqbalance 2>/dev/null; then
      echo "  set:  irqbalance stopped"
    else
      echo "  WARNING: failed to stop irqbalance" >&2
    fi
  else
    echo "  ok:   irqbalance not running"
  fi
}

start_irqbalance() {
  if ! command -v systemctl >/dev/null 2>&1; then
    echo "  skip: systemctl not available"
    return 0
  fi
  if systemctl start irqbalance 2>/dev/null; then
    echo "  set:  irqbalance started"
  else
    echo "  note: could not start irqbalance (not installed? that is fine)"
  fi
}

print_state() {
  local governors turbo aslr smt irq
  governors="$(cat /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor 2>/dev/null | sort -u | paste -sd, - || true)"
  if [ -e /sys/devices/system/cpu/intel_pstate/no_turbo ]; then
    turbo="intel_pstate/no_turbo=$(cat /sys/devices/system/cpu/intel_pstate/no_turbo 2>/dev/null || echo '?')"
  elif [ -e /sys/devices/system/cpu/cpufreq/boost ]; then
    turbo="cpufreq/boost=$(cat /sys/devices/system/cpu/cpufreq/boost 2>/dev/null || echo '?')"
  else
    turbo="no control file"
  fi
  aslr="$(cat /proc/sys/kernel/randomize_va_space 2>/dev/null || echo unknown)"
  smt="$(cat /sys/devices/system/cpu/smt/control 2>/dev/null || echo unknown)"
  if command -v systemctl >/dev/null 2>&1; then
    irq="$(systemctl is-active irqbalance 2>/dev/null || true)"
    [ -n "${irq}" ] || irq="unknown"
  else
    irq="unknown (no systemctl)"
  fi
  echo "  governor(s): ${governors:-unknown}"
  echo "  turbo/boost: ${turbo}"
  echo "  aslr:        kernel.randomize_va_space=${aslr}"
  echo "  smt:         ${smt}"
  echo "  irqbalance:  ${irq}"
}

echo "=== tune.sh: state before '${MODE}' ==="
print_state
echo

if [ "${MODE}" = "apply" ]; then
  echo "=== tune.sh: applying benchmark tuning ==="
  set_governor performance
  turbo_off
  write_sysfs 0 /proc/sys/kernel/randomize_va_space
  write_sysfs off /sys/devices/system/cpu/smt/control
  stop_irqbalance
else
  echo "=== tune.sh: restoring defaults ==="
  # SMT first, so CPUs that come back online pick up the governor set below.
  write_sysfs on /sys/devices/system/cpu/smt/control
  restore_governor="$(default_governor)"
  if [ -n "${restore_governor}" ]; then
    set_governor "${restore_governor}"
  else
    echo "  WARNING: no schedutil/ondemand/powersave governor available; leaving governor as-is" >&2
  fi
  turbo_on
  write_sysfs 2 /proc/sys/kernel/randomize_va_space
  start_irqbalance
fi

echo
echo "=== tune.sh: state after '${MODE}' ==="
print_state
