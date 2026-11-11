#!/usr/bin/env bash
# Issue the server TLS leaf for the air-gapped LAN host. Runs on the
# target (install time) OR on staging when --lan-host is known early.
# Each --host becomes an IP: SAN if it parses as an IP literal, else DNS:.
#
# The leaf's notBefore is backdated a day (see sign.sh), and its notAfter
# never passes the CA's.
#
# --check issues nothing: it exits 0 when OUT already holds a leaf that chains
# to the CA, covers every --host, and stays valid for at least another day,
# and 1 otherwise. It needs only root.crt, not root.key.
# TARGET-SIDE: no network access.
set -euo pipefail

usage() { echo "Usage: issue-leaf.sh --ca-dir DIR --host H [--host H2 ...] --out DIR [--days N] [--check]"; }

CA_DIR="" OUT="" DAYS=120 CHECK=0
HOSTS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --ca-dir) CA_DIR="$2"; shift 2 ;;
    --host)   HOSTS+=("$2"); shift 2 ;;
    --out)    OUT="$2"; shift 2 ;;
    --days)   DAYS="$2"; shift 2 ;;
    --check)  CHECK=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown arg: $1" >&2; usage; exit 2 ;;
  esac
done
[ -n "$CA_DIR" ] && [ -n "$OUT" ] && [ "${#HOSTS[@]}" -gt 0 ] \
  || { echo "--ca-dir, --out, and at least one --host are required" >&2; usage; exit 2; }
case "$DAYS" in ""|*[!0-9]*) echo "--days must be a whole number" >&2; exit 2 ;; esac

# Reject any --host that is not a bare DNS label or IP literal. A comma would
# splice a second SAN entry and a newline would inject an arbitrary extfile
# directive (e.g. an extra X.509 extension) — allowlisting the character set
# stops both before the value reaches openssl. `case` (not grep) so an embedded
# newline is matched rather than swallowed as a line separator.
for h in "${HOSTS[@]}"; do
  case "$h" in
    ""|*[!A-Za-z0-9.:-]*)
      echo "invalid --host '$h': only letters, digits, dot, colon, hyphen allowed" >&2; exit 2 ;;
  esac
done

is_ip() {
  # IPv4 dotted-quad, or an IPv6 literal (hex groups + colons). The old
  # colon-anywhere test mislabeled any colon-bearing string as an IP. Inputs
  # are already char-allowlisted above, so this only decides IP: vs DNS:.
  if printf '%s' "$1" | grep -Eq '^([0-9]{1,3}\.){3}[0-9]{1,3}$'; then return 0; fi
  if printf '%s' "$1" | grep -Eq '^[0-9A-Fa-f:]+$' && printf '%s' "$1" | grep -q ':'; then return 0; fi
  return 1
}

# san_entry HOST: HOST as written in an extfile subjectAltName.
san_entry() {
  if is_ip "$1"; then printf 'IP:%s' "$1"; else printf 'DNS:%s' "$1"; fi
}

# san_key ENTRY: ENTRY in one spelling per name, for comparing. OpenSSL
# prints an IPv6 SAN expanded and upper-case (FD00:0:0:0:0:0:0:1) where a
# --host is usually compressed (fd00::1), so both become eight lower-case
# groups without leading zeros.
san_key() {
  local v="${1#IP:}" head tail g out="" groups=() front=() back=() i
  case "$1" in
    IP:*:*) ;;
    *) printf '%s\n' "$1" | tr 'A-Z' 'a-z'; return ;;
  esac
  v="$(printf '%s' "$v" | tr 'A-F' 'a-f')"
  case "$v" in
    *::*)
      head="${v%%::*}" tail="${v#*::}"
      [ -z "$head" ] || IFS=: read -r -a front <<< "$head"
      [ -z "$tail" ] || IFS=: read -r -a back <<< "$tail"
      groups=("${front[@]}")
      for ((i = ${#front[@]} + ${#back[@]}; i < 8; i++)); do groups+=(0); done
      groups+=("${back[@]}")
      ;;
    *) IFS=: read -r -a groups <<< "$v" ;;
  esac
  for g in "${groups[@]}"; do
    case "$g" in "" | *[!0-9a-f]*) g=x ;; *) g="$(printf '%x' "0x$g")" ;; esac
    out="$out${out:+:}$g"
  done
  printf 'ip:%s\n' "$out"
}

# leaf_sans CERT: the leaf's SANs, one per line, in san_key's spelling.
# Read from -text, which every OpenSSL and LibreSSL prints alike.
leaf_sans() {
  local san
  openssl x509 -in "$1" -noout -text \
    | awk '/X509v3 Subject Alternative Name/ { getline; print; exit }' \
    | tr ',' '\n' | sed -e 's/^[[:space:]]*//' -e 's/^IP Address:/IP:/' \
    | while IFS= read -r san; do san_key "$san"; done
}

leaf_is_current() {
  local h sans
  [ -f "$OUT/server.crt" ] && [ -f "$OUT/server.key" ] || return 1
  openssl verify -CAfile "$CA_DIR/root.crt" "$OUT/server.crt" >/dev/null 2>&1 || return 1
  openssl x509 -in "$OUT/server.crt" -noout -checkend 86400 >/dev/null 2>&1 || return 1
  sans="$(leaf_sans "$OUT/server.crt")"
  for h in "${HOSTS[@]}"; do
    printf '%s\n' "$sans" | grep -qxF "$(san_key "$(san_entry "$h")")" || return 1
  done
}

if [ "$CHECK" = 1 ]; then
  [ -f "$CA_DIR/root.crt" ] || { echo "CA dir must contain root.crt" >&2; exit 2; }
  leaf_is_current
  exit
fi

[ -f "$CA_DIR/root.crt" ] && [ -f "$CA_DIR/root.key" ] \
  || { echo "CA dir must contain root.crt and root.key" >&2; exit 2; }

# shellcheck source=sign.sh
. "$(cd "$(dirname "$0")" && pwd)/sign.sh"

openssl x509 -in "$CA_DIR/root.crt" -noout -checkend 86400 >/dev/null 2>&1 \
  || { echo "the CA in $CA_DIR expires within a day; mint a new one (see release/docs/airgap-deployment.md)" >&2; exit 2; }
start="$(backdated_start)"
if openssl x509 -in "$CA_DIR/root.crt" -noout -checkend $((DAYS * 86400)) >/dev/null 2>&1; then
  end="$(utc_time $(($(date -u +%s) + DAYS * 86400)))"
else
  end="$(cert_not_after "$CA_DIR/root.crt")"
  echo "note: the CA expires before $DAYS days; the leaf ends with it ($end)" >&2
fi

mkdir -p "$OUT"
# Lock the dir down BEFORE writing server.key: default umask 022 leaves a fresh
# dir 0755, so the private key would be briefly group/world-readable.
chmod 700 "$OUT"

san=""
for h in "${HOSTS[@]}"; do
  san="${san}${san:+,}$(san_entry "$h")"
done

umask 077
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
openssl ecparam -name prime256v1 -genkey -noout -out "$work/server.key"

# CN = first host for legacy display; SANs are authoritative.
openssl req -new -key "$work/server.key" -subj "/CN=${HOSTS[0]}" -out "$work/server.csr"

cat > "$work/ext" <<EXT
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid
subjectAltName=${san}
EXT

sign_csr "$work/server.csr" "$work/server.crt" "$start" "$end" "$work/ext" \
  "$CA_DIR/root.key" "$CA_DIR/root.crt"

# Replace the old leaf only once the new one is complete.
install -m 0600 "$work/server.key" "$OUT/server.key"
install -m 0644 "$work/server.crt" "$OUT/server.crt"
echo "issued leaf: $OUT/server.crt (SAN: ${san}; valid $start to $end)"
