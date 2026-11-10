# shellcheck shell=bash
# Facts about the machine an installer runs on, shared by every installer.
# Run on the host itself, never inside a container: containers made from one
# image share that image's machine-id.

# node_id PREFIX: an id for this machine's server or worker, PREFIX-HOST-HASH,
# for example worker-ubuntu-3f9c2a1b. Hostnames repeat across cloned and
# live-boot fleets (every box is "ubuntu"), and two workers or servers sharing
# an id take each other's work, so HASH comes from this machine's machine-id
# together with the MAC address of its first physical network card: a disk
# cloned onto many boxes keeps one machine-id, but each box has its own card.
# With neither, HASH is random. The installer writes the id into the role's
# env file, which keeps it stable across re-runs.
node_id() {
  local prefix="$1" host seed="" mac="" file dir hash
  host="$(hostname -s 2>/dev/null | tr 'A-Z' 'a-z' | tr -cd 'a-z0-9-' | cut -c1-40)"
  for file in "${BROCCOLI_MACHINE_ID_FILE:-/etc/machine-id}" /var/lib/dbus/machine-id; do
    if [ -s "$file" ]; then
      seed="$(cat "$file")"
      break
    fi
  done
  # A physical card has a device link; bridges, veths and loopback do not.
  for dir in "${BROCCOLI_SYS_NET_DIR:-/sys/class/net}"/*; do
    [ -e "$dir/device" ] && [ -s "$dir/address" ] || continue
    mac="$(cat "$dir/address")"
    [ "$mac" = 00:00:00:00:00:00 ] || break
    mac=""
  done
  seed="$seed${mac:+:$mac}"
  [ -n "$seed" ] || seed="$(openssl rand -hex 16)"
  hash="$(printf 'broccoli-node-id:%s' "$seed" | openssl dgst -sha256 | sed 's/^.*= *//' | cut -c1-8)"
  printf '%s-%s%s\n' "$prefix" "${host:+$host-}" "$hash"
}

# trusted_proxies_json "ADDR..." : the server.trusted_proxies value trusting
# exactly the gateways given as IPs or CIDRs, e.g. "10.0.0.5" becomes
# ["10.0.0.5/32"]. A server trusts X-Forwarded-For only from these peers, so
# behind a gateway this must name the gateway, and nothing wider: any client
# inside a trusted range can forge its own address.
trusted_proxies_json() {
  local addr out=""
  for addr in $1; do
    case "$addr" in
      *[!0-9A-Fa-f.:/]* | "")
        echo "error: '$addr' is not an IP address or CIDR" >&2
        return 1
        ;;
      */*) ;;
      *:*) addr="$addr/128" ;;
      *) addr="$addr/32" ;;
    esac
    out="$out${out:+,}\"$addr\""
  done
  printf '[%s]\n' "$out"
}

# gateway_trust_json "HOST ...": trusted_proxies_json for gateways given as
# IPs, CIDRs or names. A name is resolved here, since the server only ever
# sees the gateway's address.
gateway_trust_json() {
  local h out="" list
  read -r -d "" -a list <<< "$1" || true
  for h in "${list[@]}"; do out="$out${out:+ }$(host_address "$h")"; done
  [ -n "$out" ] || { echo "error: no gateway address given" >&2; return 1; }
  trusted_proxies_json "$out"
}

# host_address HOST: HOST as an IPv4 address, resolved here when it is a name.
# Containers do not read the host's /etc/hosts, so a name listed only there
# (a typical venue LAN name) must reach them as an address. Prints HOST as
# given when it is already an address or does not resolve.
host_address() {
  local addr=""
  case "$1" in
    *[!0-9.]*) case "$1" in *:*) ;; *) addr="$(getent ahostsv4 "$1" 2>/dev/null | awk 'NR == 1 { print $1 }')" || addr="" ;; esac ;;
  esac
  printf '%s\n' "${addr:-$1}"
}

# source_ip_for HOST: this machine's address on the route to HOST, which is
# the peer address HOST sees for connections from here. Empty if unknown.
source_ip_for() {
  command -v ip >/dev/null 2>&1 || return 0
  { ip route get "$1" 2>/dev/null || true; } \
    | awk '{ for (i = 1; i < NF; i++) if ($i == "src") { print $(i + 1); exit } }'
}

# upstream_list "HOST[:PORT] ...": a gateway's upstream servers as one
# space-separated ADDR:PORT line. The port defaults to 3000, and a name is
# resolved here (host_address), since the gateway container cannot read this
# host's /etc/hosts. Caddy splices the list into its Caddyfile before parsing
# it, so anything other than a host and a port is refused.
upstream_list() {
  local u host port out="" list
  read -r -d "" -a list <<< "$1" || true
  for u in "${list[@]}"; do
    case "$u" in
      *[!]A-Za-z0-9.:[-]*) echo "error: upstream '$u' is not HOST or HOST:PORT" >&2; return 1 ;;
      \[*\]:*) host="${u%:*}" port="${u##*:}" ;;
      \[*\]) host="$u" port=3000 ;;
      *:*:*) echo "error: write the IPv6 upstream '$u' as [ADDR]:PORT" >&2; return 1 ;;
      *:*) host="${u%:*}" port="${u##*:}" ;;
      *) host="$u" port=3000 ;;
    esac
    case "$port" in
      "" | *[!0-9]*) host="" ;;
    esac
    [ -n "$host" ] && [ "$host" != "[]" ] \
      || { echo "error: upstream '$u' is not HOST or HOST:PORT" >&2; return 1; }
    case "$host" in \[*) ;; *) host="$(host_address "$host")" ;; esac
    out="$out${out:+ }$host:$port"
  done
  [ -n "$out" ] || { echo "error: no upstream servers given" >&2; return 1; }
  printf '%s\n' "$out"
}

# gateway_source_addrs "UPSTREAM ...": the addresses the upstream servers see
# this machine's connections come from, space-separated, which is what each
# of them must trust for X-Forwarded-For. Falls back to this machine's first
# address when there is no route to an upstream yet.
gateway_source_addrs() {
  local u host addr out="" list
  read -r -d "" -a list <<< "$1" || true
  for u in "${list[@]}"; do
    host="${u%:*}"
    host="${host#[}"
    host="${host%]}"
    addr="$(source_ip_for "$host")"
    [ -n "$addr" ] || addr="$(hostname -I 2>/dev/null | awk '{print $1}')" || addr=""
    [ -n "$addr" ] || continue
    case " $out " in
      *" $addr "*) ;;
      *) out="$out${out:+ }$addr" ;;
    esac
  done
  printf '%s\n' "$out"
}
