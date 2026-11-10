#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
doc="$here/../../docs/airgap-deployment.md"
[ -f "$doc" ] || { echo "FAIL: airgap-deployment.md missing"; exit 1; }
for needle in "build-bundle.sh" "load-bundle.sh" "install.sh" "setup.sh" "--role server" \
              "root.crt" "root.key" "trust-ca" "manifest.sha256" "--pull never" \
              "--role gateway" "--infra-host" "--gateway-host" "--upstream" "server-join.env"; do
  grep -q -- "$needle" "$doc" || { echo "FAIL: runbook missing mention of: $needle"; exit 1; }
done
echo "PASS: airgap runbook covers assembly, transfer, install, trust, integrity"
