#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
lb="$here/../load-bundle.sh"
[ -f "$lb" ] || { echo "FAIL: load-bundle.sh missing"; exit 1; }
bash -n "$lb" || { echo "FAIL: load-bundle.sh does not parse"; exit 1; }

# offline invariant
grep -Eq '\b(curl|wget|apt|apt-get|pip[0-9]*)\b' "$lb" && { echo "FAIL: network fetch present"; exit 1; }
grep -q 'docker pull' "$lb" && { echo "FAIL: docker pull present"; exit 1; }

# --verify-only path works on a hand-built fixture (no docker needed)
here2="$(cd "$here/.." && pwd)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
mkdir -p "$T/images"
printf '{"version":"test"}\n' > "$T/bundle.json"
printf 'fake-image-tar\n' > "$T/images/x.tar"
# shellcheck source=/dev/null
source "$here2/lib/manifest.sh"; manifest_generate "$T"

bash "$lb" --bundle "$T" --verify-only >/dev/null || { echo "FAIL: verify-only rejected a clean bundle"; exit 1; }

# engine-aware image load: BROCCOLI_ENGINE=podman must drive `podman load`, not
# a hardcoded `docker load` (podman-parity regression guard). $T is still clean
# (the fake podman shim lives in a sibling dir, NOT under $T, so it never
# perturbs the manifest-verified bundle tree).
pbin="$(mktemp -d)"; trap 'rm -rf "$T" "$pbin"' EXIT
cat > "$pbin/podman" <<'E'
#!/usr/bin/env bash
[ "$1" = load ] && { echo "PODMAN-LOAD $*"; exit 0; }
exit 0
E
chmod +x "$pbin/podman"
pout="$(PATH="$pbin:$PATH" BROCCOLI_ENGINE=podman bash "$lb" --bundle "$T" 2>&1)" \
  || { echo "FAIL: podman-engine image load failed"; echo "$pout"; exit 1; }
grep -q 'PODMAN-LOAD load' <<<"$pout" || { echo "FAIL: load-bundle did not use the podman engine to load images"; echo "$pout"; exit 1; }

# --- --pristine: a freshly-transported bundle must carry NONE of the on-host
#     env files. They are manifest-excluded, so integrity alone can't see a
#     planted one — --pristine is the check that does. $T is still clean here. ---
bash "$lb" --bundle "$T" --verify-only --pristine >/dev/null \
  || { echo "FAIL: --pristine rejected a clean bundle"; exit 1; }
mkdir -p "$T/compose"; printf 'BROCCOLI__DATABASE__URL=postgres://evil\n' > "$T/compose/.env.server"
# integrity is UNCHANGED (the planted file is manifest-excluded) — the blind spot
bash "$lb" --bundle "$T" --verify-only >/dev/null \
  || { echo "FAIL: plain verify should still pass (host-env is manifest-excluded)"; exit 1; }
# but --pristine MUST catch the planted file
if bash "$lb" --bundle "$T" --verify-only --pristine >/dev/null 2>&1; then
  echo "FAIL: --pristine accepted a bundle carrying a planted .env.server"; exit 1
fi
rm -rf "$T/compose"

printf 'TAMPER\n' >> "$T/bundle.json"
if bash "$lb" --bundle "$T" --verify-only >/dev/null 2>&1; then
  echo "FAIL: verify-only accepted a tampered bundle"; exit 1
fi
echo "PASS: load-bundle verifies manifest, rejects tamper, is offline"
