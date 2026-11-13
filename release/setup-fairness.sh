#!/usr/bin/env bash
set -euo pipefail

ALLOW_SMT=false
DRY_RUN=false
MAX_CONCURRENCY="${BROCCOLI__WORKER__MAX_CONCURRENCY:-1}"
BOX_IDS=""
CPU_LIST=""
OUTPUT="${OUTPUT:-fairness.env}"
CGROUP_ROOT="${CGROUP_ROOT:-/sys/fs/cgroup}"
ISOLATE_RUN_DIR="${ISOLATE_RUN_DIR:-/run/isolate}"
ISOLATE_PARENT="${ISOLATE_PARENT:-isolate}"
ISOLATE_CONFIG="${ISOLATE_CONFIG:-/usr/local/etc/isolate}"

usage() {
  cat >&2 <<'EOF'
usage: release/setup-fairness.sh [options]

Linux-only host setup for opting a Broccoli worker into max_concurrency > 1.
It refuses SMT by default, switches CPU governors to performance when possible,
confines every isolate box to the dedicated judging CPU pool, and writes a
worker env snippet.

Options:
  --max-concurrency N   Number of sandbox slots to prepare, default 1.
  --cpus LIST           Comma/range CPU list to use, e.g. 2-5 or 2,4,6,8.
  --box-ids LIST        Comma/range isolate box IDs, default 0..999.
  --output PATH         Env snippet path, default fairness.env.
  --isolate-config PATH Isolate config path, default /usr/local/etc/isolate.
  --allow-smt           Allow running when SMT/hyperthreading is enabled.
  --dry-run             Print planned writes without modifying the host.
  -h, --help            Show this help.
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

log() {
  echo "==> $*"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --allow-smt) ALLOW_SMT=true ;;
    --dry-run) DRY_RUN=true ;;
    --max-concurrency)
      [[ $# -ge 2 ]] || die "--max-concurrency requires a value"
      MAX_CONCURRENCY="$2"
      shift
      ;;
    --cpus)
      [[ $# -ge 2 ]] || die "--cpus requires a value"
      CPU_LIST="$2"
      shift
      ;;
    --box-ids)
      [[ $# -ge 2 ]] || die "--box-ids requires a value"
      BOX_IDS="$2"
      shift
      ;;
    --output)
      [[ $# -ge 2 ]] || die "--output requires a value"
      OUTPUT="$2"
      shift
      ;;
    --isolate-config)
      [[ $# -ge 2 ]] || die "--isolate-config requires a value"
      ISOLATE_CONFIG="$2"
      shift
      ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
  shift
done

[[ "$(uname -s)" == "Linux" ]] || die "setup-fairness.sh must run on the Linux worker host"
[[ "$MAX_CONCURRENCY" =~ ^[1-9][0-9]*$ ]] || die "--max-concurrency must be a positive integer"

expand_list() {
  local input="$1"
  local -a out=()
  local part start end n

  IFS=',' read -ra parts <<< "$input"
  for part in "${parts[@]}"; do
    [[ -n "$part" ]] || die "empty list item in '$input'"
    if [[ "$part" =~ ^([0-9]+)-([0-9]+)$ ]]; then
      start="${BASH_REMATCH[1]}"
      end="${BASH_REMATCH[2]}"
      (( start <= end )) || die "invalid descending range '$part'"
      for ((n = start; n <= end; n++)); do
        out+=("$n")
      done
    elif [[ "$part" =~ ^[0-9]+$ ]]; then
      out+=("$part")
    else
      die "invalid list item '$part'"
    fi
  done
  printf '%s\n' "${out[@]}"
}

detect_smt() {
  if [[ -r /sys/devices/system/cpu/smt/active ]] &&
    [[ "$(cat /sys/devices/system/cpu/smt/active)" == "1" ]]; then
    return 0
  fi

  local siblings
  for siblings in /sys/devices/system/cpu/cpu*/topology/thread_siblings_list; do
    [[ -r "$siblings" ]] || continue
    if [[ "$(cat "$siblings")" == *,* ]] || [[ "$(cat "$siblings")" == *-* ]]; then
      return 0
    fi
  done
  return 1
}

write_file() {
  local path="$1"
  local value="$2"
  if [[ "$DRY_RUN" == "true" ]]; then
    echo "dry-run: write '$value' > $path"
    return 0
  fi
  printf '%s\n' "$value" > "$path"
}

mkdir_safe() {
  local path="$1"
  if [[ "$DRY_RUN" == "true" ]]; then
    echo "dry-run: mkdir -p $path"
    return 0
  fi
  mkdir -p "$path"
}

enable_controller() {
  local target="$1"
  local controller="$2"
  [[ -f "$target/cgroup.subtree_control" ]] || return 0
  if grep -qw "$controller" "$target/cgroup.subtree_control" 2>/dev/null; then
    return 0
  fi
  write_file "$target/cgroup.subtree_control" "+$controller"
}

set_governor_performance() {
  local governor_file changed=0 skipped=0
  shopt -s nullglob
  for governor_file in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    if [[ -w "$governor_file" || "$DRY_RUN" == "true" ]]; then
      write_file "$governor_file" performance
      changed=$((changed + 1))
    else
      skipped=$((skipped + 1))
    fi
  done
  shopt -u nullglob

  if (( changed > 0 )); then
    log "set CPU governor to performance for $changed CPUs"
  elif (( skipped > 0 )); then
    log "CPU governor files exist but are not writable; rerun as root to change them"
  else
    log "CPU governor control is not available on this host; leaving it unchanged"
  fi
}

write_isolate_config_block() {
  local tmp
  local start="# Broccoli fairness cpuset begin"
  local end="# Broccoli fairness cpuset end"

  if [[ "$DRY_RUN" == "true" ]]; then
    echo "dry-run: update $ISOLATE_CONFIG with the shared CPU-pool assignment"
    local preview_count="${#BOXES[@]}"
    if (( preview_count > 16 )); then
      preview_count=16
    fi
    for ((i = 0; i < preview_count; i++)); do
      echo "dry-run: append box${BOXES[$i]}.cpus = $CPU_LIST"
    done
    if (( ${#BOXES[@]} > preview_count )); then
      echo "dry-run: ... ${#BOXES[@]} total box assignments"
    fi
    return 0
  fi

  [[ -f "$ISOLATE_CONFIG" ]] || die "isolate config not found: $ISOLATE_CONFIG"
  [[ -w "$ISOLATE_CONFIG" ]] || die "isolate config is not writable: $ISOLATE_CONFIG"

  tmp="$(mktemp)"
  awk -v start="$start" -v end="$end" '
    $0 == start { skip = 1; next }
    $0 == end { skip = 0; next }
    !skip { print }
  ' "$ISOLATE_CONFIG" > "$tmp"

  {
    echo "$start"
    echo "# Generated by release/setup-fairness.sh."
    echo "# isolate writes these values to each box cgroup's cpuset.cpus."
    echo "# Every box gets the whole dedicated CPU pool, not one CPU per box id:"
    echo "# the workers on a host share one box-id space and take the lowest free"
    echo "# ids, so which ids are live together depends on what else is judging"
    echo "# and any static one-CPU-per-box-id map can co-schedule two runs on one"
    echo "# CPU while other pool CPUs sit idle. Confining every box to the pool"
    echo "# lets the kernel scheduler give each of the at most max_concurrency"
    echo "# concurrent runs its own CPU."
    for ((i = 0; i < ${#BOXES[@]}; i++)); do
      echo "box${BOXES[$i]}.cpus = $CPU_LIST"
    done
    echo "$end"
  } >> "$tmp"

  install -m 0644 "$tmp" "$ISOLATE_CONFIG"
  rm -f "$tmp"
}

if detect_smt && [[ "$ALLOW_SMT" != "true" ]]; then
  die "SMT/hyperthreading appears enabled; disable it in firmware/kernel or pass --allow-smt"
fi

if [[ -z "$CPU_LIST" ]]; then
  CPU_LIST="$(seq -s, 0 "$((MAX_CONCURRENCY - 1))")"
fi
CPUS=()
while IFS= read -r item; do
  CPUS+=("$item")
done < <(expand_list "$CPU_LIST")
(( ${#CPUS[@]} >= MAX_CONCURRENCY )) ||
  die "CPU list '$CPU_LIST' has fewer CPUs than max_concurrency=$MAX_CONCURRENCY"

if [[ -z "$BOX_IDS" ]]; then
  BOX_IDS="0-999"
fi
BOXES=()
while IFS= read -r item; do
  BOXES+=("$item")
done < <(expand_list "$BOX_IDS")
(( ${#BOXES[@]} >= MAX_CONCURRENCY )) ||
  die "box ID list '$BOX_IDS' has fewer IDs than max_concurrency=$MAX_CONCURRENCY"

if [[ -r "$ISOLATE_RUN_DIR/cgroup" ]]; then
  ISOLATE_CGROUP_PARENT="$(cat "$ISOLATE_RUN_DIR/cgroup")"
else
  ISOLATE_CGROUP_PARENT="$CGROUP_ROOT/$ISOLATE_PARENT"
fi

[[ -d "$CGROUP_ROOT" ]] || die "cgroup root does not exist: $CGROUP_ROOT"
[[ -f "$CGROUP_ROOT/cgroup.controllers" ]] || die "cgroup v2 is not mounted at $CGROUP_ROOT"
grep -qw cpuset "$CGROUP_ROOT/cgroup.controllers" ||
  die "cgroup cpuset controller is not available at $CGROUP_ROOT"

set_governor_performance

log "preparing isolate parent cgroup $ISOLATE_CGROUP_PARENT"
mkdir_safe "$ISOLATE_CGROUP_PARENT"
enable_controller "$CGROUP_ROOT" cpuset
enable_controller "$ISOLATE_CGROUP_PARENT" cpuset

if [[ -f "$ISOLATE_CGROUP_PARENT/cpuset.mems" && -r "$ISOLATE_CGROUP_PARENT/cpuset.mems.effective" ]]; then
  current_mems="$(cat "$ISOLATE_CGROUP_PARENT/cpuset.mems" 2>/dev/null || true)"
  if [[ -z "$current_mems" ]]; then
    write_file "$ISOLATE_CGROUP_PARENT/cpuset.mems" "$(cat "$ISOLATE_CGROUP_PARENT/cpuset.mems.effective")"
  fi
fi

write_isolate_config_block

if [[ "$DRY_RUN" == "true" ]]; then
  echo "dry-run: write worker env snippet to $OUTPUT"
else
  umask 077
  {
    echo "# Generated by release/setup-fairness.sh"
    echo "BROCCOLI__WORKER__MAX_CONCURRENCY=$MAX_CONCURRENCY"
    echo "BROCCOLI__WORKER__FAIRNESS_UNSAFE_ALLOW=false"
    echo "BROCCOLI_FAIRNESS_CPUSET_PARENT=$ISOLATE_CGROUP_PARENT"
    echo "BROCCOLI_FAIRNESS_CPUSET_BOX_IDS=$BOX_IDS"
    echo "BROCCOLI_FAIRNESS_CPUSET_CPUS=$CPU_LIST"
  } > "$OUTPUT"
fi

log "confined ${#BOXES[@]} isolate boxes to CPU pool $CPU_LIST (${#CPUS[@]} CPUs for $MAX_CONCURRENCY concurrent slots)"
log "wrote worker env snippet: $OUTPUT"
