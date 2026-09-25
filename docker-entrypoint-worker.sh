#!/bin/sh
set -eu

CGROUP_ROOT="${CGROUP_ROOT:-/sys/fs/cgroup}"
ISOLATE_PARENT="${ISOLATE_PARENT:-isolate}"
ISOLATE_RUN_DIR="${ISOLATE_RUN_DIR:-/run/isolate}"
ISOLATE_BOX_DIR="${ISOLATE_BOX_DIR:-/var/local/lib/isolate}"
REQUIRED_CONTROLLERS="${REQUIRED_CONTROLLERS:-memory cpu pids}"

warn() {
    printf '%s\n' "WARNING: $*" >&2
}

die() {
    printf '%s\n' "ERROR: $*" >&2
    exit 1
}

have_controller() {
    controller="$1"
    grep -qw "$controller" "$CGROUP_ROOT/cgroup.controllers" 2>/dev/null
}

enable_controller() {
    controller="$1"
    target="$2"
    printf '+%s\n' "$controller" > "$target/cgroup.subtree_control" 2>/dev/null
}

move_processes() {
    from="$1"
    to="$2"

    [ -f "$from/cgroup.procs" ] || return 0

    while IFS= read -r pid; do
        [ -n "$pid" ] || continue
        printf '%s\n' "$pid" > "$to/cgroup.procs" 2>/dev/null || true
    done < "$from/cgroup.procs"
}

has_arg() {
    needle="$1"
    shift
    for arg in "$@"; do
        [ "$arg" = "$needle" ] && return 0
    done
    return 1
}

setup_cgroup_v2_required() {
    [ -f "$CGROUP_ROOT/cgroup.controllers" ] || {
        die "cgroup v2 is not available; run the worker with a cgroup v2 host and the privileges required by isolate"
    }

    parent="$CGROUP_ROOT/$ISOLATE_PARENT"
    worker="$parent/worker"

    if ! mkdir -p "$parent" "$worker" "$ISOLATE_RUN_DIR" 2>/dev/null; then
        die "could not create isolate cgroups; run the worker container with the required cgroup privileges for isolate"
    fi

    move_processes "$CGROUP_ROOT" "$parent"

    for controller in $REQUIRED_CONTROLLERS; do
        if have_controller "$controller"; then
            enable_controller "$controller" "$CGROUP_ROOT" || \
                die "could not enable cgroup controller '$controller' on $CGROUP_ROOT"
        else
            die "cgroup controller '$controller' is not available"
        fi
    done

    move_processes "$parent" "$worker"

    for controller in $REQUIRED_CONTROLLERS; do
        enable_controller "$controller" "$parent" || \
            die "could not enable cgroup controller '$controller' on $parent"
    done

    printf '%s\n' "$parent" > "$ISOLATE_RUN_DIR/cgroup" 2>/dev/null || {
        die "could not write $ISOLATE_RUN_DIR/cgroup"
    }
    chmod 0755 "$ISOLATE_RUN_DIR"

    if [ -f "$parent/cgroup.subtree_control" ]; then
        controllers="$(cat "$parent/cgroup.subtree_control" 2>/dev/null || true)"
        printf '%s\n' "cgroup v2 setup complete: $parent (controllers: ${controllers:-none})"
    fi
}

verify_isolate_runtime() {
    isolate_bin="${BROCCOLI__WORKER__ISOLATE_BIN:-isolate}"
    isolate_path="$(command -v "$isolate_bin" 2>/dev/null || true)"
    [ -n "$isolate_path" ] || die "isolate binary not found at $isolate_bin"

    [ -u "$isolate_path" ] || die "isolate binary at $isolate_path is missing the setuid bit"

    if [ ! -d "$ISOLATE_BOX_DIR" ]; then
        mkdir -p "$ISOLATE_BOX_DIR"
    fi
}

if [ "$#" -eq 0 ]; then
    set -- /usr/local/bin/broccoli-worker
elif [ "${1#-}" != "$1" ]; then
    set -- /usr/local/bin/broccoli-worker "$@"
elif [ "$1" = "broccoli-worker" ]; then
    shift
    set -- /usr/local/bin/broccoli-worker "$@"
fi

if [ "$1" = "/usr/local/bin/broccoli-worker" ] &&
    ! has_arg "--version" "$@" &&
    ! has_arg "-V" "$@" &&
    ! has_arg "--healthcheck" "$@"; then
    verify_isolate_runtime
    if [ "${BROCCOLI__WORKER__ENABLE_CGROUPS:-true}" = "true" ]; then
        setup_cgroup_v2_required
    fi
fi

# The box-id slot lock dir must be shared by every worker container on a host
# (see `box_slot_lock_dir` in packages/worker/src/config.rs) and is typically a
# freshly mounted, root-owned volume. Hand it to the unprivileged worker user
# while still root: if the worker cannot create its lock files it falls back to
# slot 0, which is exactly the cross-container collision the dir exists to stop.
if [ -n "${BROCCOLI__WORKER__BOX_SLOT_LOCK_DIR:-}" ] && [ "$(id -u)" = "0" ]; then
    mkdir -p "$BROCCOLI__WORKER__BOX_SLOT_LOCK_DIR"
    chown worker:worker "$BROCCOLI__WORKER__BOX_SLOT_LOCK_DIR"
fi

if [ "$(id -u)" = "0" ] && [ "${BROCCOLI_WORKER_DROP_PRIVILEGES:-true}" = "true" ]; then
    exec gosu worker:worker "$@"
fi

exec "$@"
