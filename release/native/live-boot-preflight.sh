#!/usr/bin/env bash
# Worker / live-boot PREFLIGHT -- a go/no-go that this machine can judge SAFELY.
# Encapsulates the checks proven during bring-up: x86_64 + systemd, cgroup-v2
# controllers, the isolate resource-limit verdicts (MLE via cg-oom, TLE via
# timeout), and a real C/C++/Python compile. Run as root on a worker, ideally
# AFTER `install-native.sh worker` (so isolate + cgroup delegation are in place):
#
#   sudo ./live-boot-preflight.sh [--host-only] [box1-LAN-IP]
#
# --host-only skips the isolate and toolchain checks, for container workers
# whose isolate and compilers live in the worker image.
#
# Exit 0 = GO (all critical checks pass); nonzero = NO-GO (fix [FAIL] items).
# [WARN] items are non-fatal (e.g. run before install, or no box1 IP given).
set -uo pipefail
PASS=0; FAIL=0; WARN=0
ok(){   echo "  [ OK ] $*"; PASS=$((PASS+1)); }
bad(){  echo "  [FAIL] $*"; FAIL=$((FAIL+1)); }
warn(){ echo "  [WARN] $*"; WARN=$((WARN+1)); }
hdr(){  echo; echo "== $* =="; }

# --host-only: for a Docker/Podman worker, whose isolate and compilers live in
# the worker image. Checks only what the host provides (platform, cgroup v2).
HOST_ONLY=0
if [ "${1:-}" = "--host-only" ]; then HOST_ONLY=1; shift; fi

[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 2; }

hdr "platform"
[ "$(uname -m)" = x86_64 ] && ok "arch x86_64" || bad "arch $(uname -m) -- bundle is x86_64-only"
. /etc/os-release 2>/dev/null || true
case "${VERSION_ID:-}" in
  20.04|22.04|24.04) ok "Ubuntu ${VERSION_ID}";;
  *) warn "Ubuntu ${VERSION_ID:-?} (validated: 20.04 / 22.04 / 24.04)";;
esac
[ "$(ps -o comm= -p 1 2>/dev/null)" = systemd ] && ok "systemd is PID 1" \
  || bad "systemd is not PID 1 -- installer uses systemctl (live boot: enable systemd)"

hdr "cgroup v2 (sandbox limits)"
if [ -f /sys/fs/cgroup/cgroup.controllers ]; then
  ok "cgroup-v2 unified hierarchy mounted"
  for c in memory cpu pids; do
    grep -qw "$c" /sys/fs/cgroup/cgroup.controllers && ok "controller: $c" \
      || bad "controller missing: $c -- boot with unified cgroup-v2"
  done
else
  bad "cgroup-v2 not mounted at /sys/fs/cgroup (hybrid/v1 boot? add kernel arg systemd.unified_cgroup_hierarchy=1)"
fi

if [ "$HOST_ONLY" = 1 ]; then
  hdr "isolate + toolchain"
  ok "skipped: they ship in the worker image (--host-only)"
else
hdr "isolate sandbox + resource-limit verdicts"
if ! command -v isolate >/dev/null 2>&1; then
  bad "isolate not installed -- run install-native.sh worker"
else
  ok "isolate present: $(command -v isolate)"
  [ -u "$(command -v isolate)" ] && ok "setuid bit set" \
    || warn "no setuid bit yet (broccoli-isolate-setup sets it at install)"
  [ -e /run/isolate/cgroup ] && ok "cgroup delegation configured ($(cat /run/isolate/cgroup))" \
    || warn "/run/isolate/cgroup absent (run install-native.sh worker / broccoli-isolate-setup first)"
  if [ -u "$(command -v isolate)" ] && [ -e /run/isolate/cgroup ] && command -v python3 >/dev/null 2>&1; then
    BID=991; M="$(mktemp)"
    isolate --cg -b "$BID" --cleanup >/dev/null 2>&1 || true
    if isolate --cg -b "$BID" --init >/dev/null 2>&1; then
      # MLE: allocate+touch past a 64MB cgroup cap -> kernel OOM-kills -> cg-oom-killed
      isolate --cg -b "$BID" --cg-mem=65536 --mem=262144 --time=10 --wall-time=15 --meta="$M" \
        --run -- /usr/bin/python3 -c 'a=[]
while True:
    a.append(bytearray(8*1024*1024))' >/dev/null 2>&1 || true
      grep -q '^cg-oom-killed:1' "$M" && ok "memory cap -> cg-oom-killed (MLE path works)" \
        || bad "MLE probe failed (meta: $(grep -E "status|cg-oom" "$M" 2>/dev/null | tr '\n' ' '))"
      # TLE: busy loop past a 1s CPU cap -> status:TO
      isolate --cg -b "$BID" --cg-mem=131072 --mem=131072 --time=1 --wall-time=5 --meta="$M" \
        --run -- /usr/bin/python3 -c 'x=0
while True: x+=1' >/dev/null 2>&1 || true
      grep -q '^status:TO' "$M" && ok "time cap -> status:TO (TLE path works)" \
        || bad "TLE probe failed (meta: $(grep -E "status|time:" "$M" 2>/dev/null | tr '\n' ' '))"
      isolate --cg -b "$BID" --cleanup >/dev/null 2>&1 || true
    else
      bad "isolate --init failed -- cgroup delegation not ready"
    fi
    rm -f "$M"
  else
    warn "skipped live MLE/TLE probe (needs setuid isolate + /run/isolate/cgroup + python3; run install first)"
  fi
fi

hdr "toolchain (c / c++ / python)"
T="$(mktemp -d)"; printf 'int main(){return 0;}\n' >"$T/a.c"; printf 'int main(){return 0;}\n' >"$T/a.cpp"
{ command -v gcc >/dev/null 2>&1 && gcc "$T/a.c"   -o "$T/a"  2>/dev/null; } && ok "gcc compiles C" \
  || bad "gcc cannot compile C -- install build-essential (dpkg toolchain/<ver>/*.deb, or apt)"
{ command -v g++ >/dev/null 2>&1 && g++ "$T/a.cpp" -o "$T/ax" 2>/dev/null; } && ok "g++ compiles C++" \
  || bad "g++ cannot compile C++ -- install build-essential"
command -v python3 >/dev/null 2>&1 && ok "python3: $(python3 --version 2>&1)" || bad "python3 missing"
rm -rf "$T"
fi  # HOST_ONLY

hdr "connectivity to box 1"
if [ $# -ge 1 ]; then
  code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 6 "http://$1/api/v1/health" 2>/dev/null || echo 000)
  [ "$code" = 200 ] && ok "box1 http://$1 health 200" || bad "box1 http://$1 unreachable (HTTP $code)"
else
  warn "no box1 IP given -- skipped server reachability (re-run: sudo $0 <box1-LAN-IP>)"
fi

echo; echo "==== preflight: $PASS ok, $WARN warn, $FAIL fail ===="
if [ "$FAIL" = 0 ]; then echo "GO: this machine can judge safely."; exit 0
else echo "NO-GO: fix the [FAIL] item(s) above, then re-run."; exit 1; fi
