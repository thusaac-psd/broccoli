# shellcheck shell=bash
# Certificate signing shared by mint-ca.sh and issue-leaf.sh. Both backdate
# notBefore by a day: offline venues rarely have NTP, and a machine whose
# clock runs behind the issuing host's would otherwise reject the root or the
# leaf as "not yet valid". Works with OpenSSL 1.1+, 3.x and LibreSSL.

# utc_time EPOCH: EPOCH as an ASN.1 UTCTime (YYMMDDHHMMSSZ), via GNU or BSD date.
utc_time() {
  date -u -d "@$1" +%y%m%d%H%M%SZ 2>/dev/null || date -u -r "$1" +%y%m%d%H%M%SZ
}

# backdated_start: a UTCTime one day before now.
backdated_start() {
  utc_time $(($(date -u +%s) - 86400))
}

# cert_not_after CERT: CERT's notAfter ("Mar  9 12:00:00 2027 GMT") as a UTCTime.
cert_not_after() {
  openssl x509 -in "$1" -noout -enddate | sed 's/^notAfter=//' | awk '{
    m = (index("JanFebMarAprMayJunJulAugSepOctNovDec", $1) + 2) / 3
    split($3, t, ":")
    printf "%02d%02d%02d%02d%02d%02dZ\n", $4 % 100, m, $2, t[1], t[2], t[3]
  }'
}

# sign_csr CSR OUT START END EXTFILE KEY [CA_CERT]: sign CSR into OUT, valid
# from START to END (UTCTimes), with the extensions in EXTFILE. Signed by the
# CA whose certificate is CA_CERT and key is KEY, or self-signed with KEY when
# CA_CERT is omitted. `openssl ca` is the one signer that takes an explicit
# notBefore everywhere (`x509 -not_before` needs OpenSSL 3.4); it wants a
# throwaway database, and nothing of it outlives this call.
sign_csr() {
  local csr="$1" out="$2" start="$3" end="$4" ext="$5" key="$6" ca_cert="${7:-}" db rc=0
  db="$(mktemp -d)"
  : > "$db/index.txt"
  openssl rand -hex 16 > "$db/serial"
  cat > "$db/ca.cnf" <<CNF
[ ca ]
default_ca = broccoli_ca
[ broccoli_ca ]
database = $db/index.txt
new_certs_dir = $db
serial = $db/serial
default_md = sha256
policy = broccoli_policy
unique_subject = no
[ broccoli_policy ]
organizationName = optional
commonName = supplied
CNF
  if [ -n "$ca_cert" ]; then
    set -- -cert "$ca_cert"
  else
    set -- -selfsign
  fi
  openssl ca -batch -notext -preserveDN -config "$db/ca.cnf" "$@" -keyfile "$key" \
    -startdate "$start" -enddate "$end" -extfile "$ext" \
    -in "$csr" -out "$out" >"$db/ca.log" 2>&1 || rc=$?
  [ "$rc" = 0 ] || cat "$db/ca.log" >&2
  rm -rf "$db"
  return "$rc"
}
