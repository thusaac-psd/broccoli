#!/usr/bin/env bash
# Mint an internal root CA for an air-gapped broccoli LAN. Runs on the
# networked staging box at bundle-assembly time. root.crt ships to every
# machine (public); root.key is the signing secret (0600, server-only).
set -euo pipefail

usage() { echo "Usage: mint-ca.sh --out DIR [--org NAME] [--cn NAME] [--days N]"; }

OUT="" ORG="Broccoli LAN" CN="Broccoli LAN Root CA" DAYS=120
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --org) ORG="$2"; shift 2 ;;
    --cn)  CN="$2";  shift 2 ;;
    --days) DAYS="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown arg: $1" >&2; usage; exit 2 ;;
  esac
done
[ -n "$OUT" ] || { echo "--out is required" >&2; usage; exit 2; }

# Reject subject DN components that could inject extra RDNs. '/' is the -subj
# field separator and a newline would splice new DN lines — either could forge
# O=/CN= fields. `case` (not grep) so an embedded newline is matched rather than
# swallowed as a line separator.
for v in "$ORG" "$CN"; do
  case "$v" in
    ""|*[[:cntrl:]]*|*/*)
      echo "invalid --org/--cn value: must be non-empty with no control chars or '/'" >&2; exit 2 ;;
  esac
done

mkdir -p "$OUT"
# Lock the dir down BEFORE writing root.key: default umask 022 leaves a fresh
# dir 0755, so the private key would be briefly group/world-readable.
chmod 700 "$OUT"

case "$DAYS" in ""|*[!0-9]*) echo "--days must be a whole number" >&2; exit 2 ;; esac
# shellcheck source=sign.sh
. "$(cd "$(dirname "$0")" && pwd)/sign.sh"

umask 077
openssl ecparam -name prime256v1 -genkey -noout -out "$OUT/root.key"
chmod 0600 "$OUT/root.key"

work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
openssl req -new -key "$OUT/root.key" -subj "/O=${ORG}/CN=${CN}" -out "$work/root.csr"
cat > "$work/ext" <<EXT
basicConstraints=critical,CA:TRUE,pathlen:0
keyUsage=critical,keyCertSign,cRLSign
subjectKeyIdentifier=hash
EXT
# Backdated a day like the leaf (sign.sh): a root that is "not yet valid" on a
# slow-clocked machine fails the whole chain.
sign_csr "$work/root.csr" "$OUT/root.crt" "$(backdated_start)" \
  "$(utc_time $(($(date -u +%s) + DAYS * 86400)))" "$work/ext" "$OUT/root.key"
chmod 0644 "$OUT/root.crt"

echo "minted CA: $OUT/root.crt (key: $OUT/root.key, mode 0600)"
