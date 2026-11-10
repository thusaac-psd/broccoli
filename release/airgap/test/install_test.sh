#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
ins="$here/../install.sh"
[ -f "$ins" ] || { echo "FAIL: install.sh missing"; exit 1; }
bash -n "$ins" || { echo "FAIL: install.sh does not parse"; exit 1; }

# dispatches all three roles + rejects unknown
grep -q 'server)'      "$ins" || { echo "FAIL: no server role"; exit 1; }
grep -q 'worker)'      "$ins" || { echo "FAIL: no worker role"; exit 1; }
grep -q 'contestant)'  "$ins" || { echo "FAIL: no contestant role"; exit 1; }

# composes the TLS gateway (serves the airgap leaf) and honors --pull never
grep -q 'docker-compose.gateway-airgap.yaml.template' "$ins" || { echo "FAIL: server role does not compose the TLS gateway"; exit 1; }
grep -q -- '--pull never'  "$ins" || { echo "FAIL: does not pass --pull never"; exit 1; }

# offline invariant (hardened: word-anchored pattern)
grep -Eq '\b(curl|wget|apt|apt-get|pip[0-9]*)\b' "$ins" && { echo "FAIL: network fetch present"; exit 1; }
grep -q 'docker pull' "$ins" && { echo "FAIL: docker pull present"; exit 1; }

# the contestant trust boundary re-verifies AND asserts the bundle is pristine
# (no manifest-excluded host-env files planted before USB transfer)
grep -Eq -- '--bundle "\$BUNDLE" --verify-only --pristine' "$ins" \
  || { echo "FAIL: contestant path must verify --pristine (planted host-env guard)"; exit 1; }

# unknown role exits nonzero
if bash "$ins" --role bogus --bundle /tmp/nope 2>/dev/null; then
  echo "FAIL: unknown role accepted"; exit 1
fi
# A worker never connects over TLS, so it must not need (or fail on) CA trust,
# and its preflight checks only the host, not the image's isolate/compilers.
worker_branch="$(sed -n '/^  worker)$/,/^    ;;$/p' "$ins")"
printf '%s\n' "$worker_branch" | grep -q 'os_helper\|trust-ca' \
  && { echo "FAIL: the worker role still runs the CA trust helper"; exit 1; } || true
printf '%s\n' "$worker_branch" | grep -q 'preflight" --host-only' \
  || { echo "FAIL: the worker role checks the host for the image's isolate/compilers"; exit 1; }
echo "PASS: install.sh dispatches roles, composes TLS gateway, offline"
