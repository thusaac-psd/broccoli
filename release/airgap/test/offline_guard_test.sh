#!/usr/bin/env bash
# No script that runs on a target machine reaches the network: none fetches
# with curl, wget, apt or pip, none pulls an image, and every `compose up`
# passes `--pull never`. Every shell and PowerShell script under release/ is a
# target-side script except the staging-side ones listed below, which build or
# stage the bundle on a networked box. A line that talks only to this LAN's own
# servers (a health probe) says so with an `offline-ok:` comment.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
release="$(cd "$here/../.." && pwd)"

# Staging-side: run on a networked box to build or stage the bundle.
staging=(
  airgap/build-bundle.sh
  airgap/ca/mint-ca.sh
  native/build-native-bundle.sh
  native/stage-debs.sh
  native/stage-postgres.sh
  native/stage-redis.sh
  native/stage-toolchain.sh
  examples/testlib-checker/fetch-testlib.sh
)

is_staging() {
  local s
  for s in "${staging[@]}"; do [ "$1" = "$s" ] && return 0; done
  return 1
}

rc=0
for s in "${staging[@]}"; do
  [ -f "$release/$s" ] || { echo "FAIL: staging-side $s does not exist; drop it from the list"; rc=1; }
done

targets=()
while IFS= read -r f; do
  is_staging "${f#"$release"/}" || targets+=("$f")
done < <(find "$release" -type f \( -name '*.sh' -o -name '*.ps1' \) -not -path '*/test/*' | sort)
[ "${#targets[@]}" -gt 10 ] || { echo "FAIL: found only ${#targets[@]} target-side scripts"; exit 1; }

# The lines of FILE that do anything, without comments or `offline-ok:` lines.
code_lines() {
  awk '!/offline-ok:/ && !/^[[:space:]]*#/ && NF { print FNR ": " $0 }' "$1"
}

for f in "${targets[@]}"; do
  rel="${f#"$release"/}"
  case "$f" in
    *.ps1)
      if code_lines "$f" | grep -Eqi 'invoke-webrequest|invoke-restmethod|net\.webclient|downloadfile|downloadstring|start-bitstransfer|bitsadmin|\b(iwr|irm|curl|wget)\b|certutil[^|]*-urlcache'; then
        echo "FAIL: $rel contains a network fetch"; rc=1
      fi
      continue ;;
  esac
  if code_lines "$f" | grep -Eq '\b(curl|wget|apt|apt-get|pip[0-9]*)\b'; then
    echo "FAIL: $rel contains a network fetch:"; code_lines "$f" | grep -E '\b(curl|wget|apt|apt-get|pip[0-9]*)\b'; rc=1
  fi
  if code_lines "$f" | grep -Eq '(docker|podman)[[:space:]]+(image[[:space:]]+)?pull'; then
    echo "FAIL: $rel uses 'docker/podman pull'"; rc=1
  fi
  if code_lines "$f" | grep -E '\bup[[:space:]]+-d\b' | grep -vq -- '--pull never'; then
    echo "FAIL: $rel runs compose up without --pull never"; rc=1
  fi
done

[ "$rc" = "0" ] && echo "PASS: no target-side script fetches from the network (${#targets[@]} scripts)"
exit $rc
