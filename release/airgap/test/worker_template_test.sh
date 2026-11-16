#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
wt="$here/../../docker-compose.worker.yaml.template"
[ -f "$wt" ] || { echo "FAIL: worker template missing: $wt"; exit 1; }

# The worker template must not bind-mount isolate's config: the image bakes isolate plus
# its config, which lives at the single FILE /usr/local/etc/isolate. A host
# bind-mount of that path makes Docker auto-create an empty DIRECTORY at the
# missing source and mount it over the image's config file, so the container
# dies at start with "mount ... not a directory" and the worker never joins.
# The mount is also redundant (image is self-contained). Assert it is gone.
grep -Eq '^[[:space:]]*-[[:space:]]*/usr/local/etc/isolate:' "$wt" \
  && { echo "FAIL: worker template bind-mounts host /usr/local/etc/isolate (dir-over-file) — worker cannot start"; exit 1; } || true

# The worker must still run the isolate backend with cgroups, from the baked config.
grep -q 'BROCCOLI__WORKER__SANDBOX_BACKEND: isolate' "$wt" \
  || { echo "FAIL: worker template does not select the isolate sandbox backend"; exit 1; }
grep -q 'BROCCOLI__WORKER__ISOLATE_BIN: /usr/local/bin/isolate' "$wt" \
  || { echo "FAIL: worker template does not point ISOLATE_BIN at the baked binary"; exit 1; }

echo "PASS: worker template runs isolate from the baked config, no dir-over-file mount"
