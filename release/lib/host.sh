# shellcheck shell=bash
# Facts about the machine an installer runs on, shared by every installer.
# Run on the host itself, never inside a container: containers made from one
# image share that image's machine-id.

# node_id PREFIX: an id for this machine's server or worker, PREFIX-HOST-HASH,
# for example worker-ubuntu-3f9c2a1b. Hostnames repeat across cloned and
# live-boot fleets (every box is "ubuntu"), and two workers or servers sharing
# an id take each other's work, so HASH comes from this machine's machine-id,
# else its first MAC address, else random bytes. The installer writes the id
# into the role's env file, which keeps it stable across re-runs.
node_id() {
  local prefix="$1" host seed="" file hash
  host="$(hostname -s 2>/dev/null | tr 'A-Z' 'a-z' | tr -cd 'a-z0-9-' | cut -c1-40)"
  for file in "${BROCCOLI_MACHINE_ID_FILE:-/etc/machine-id}" /var/lib/dbus/machine-id; do
    if [ -s "$file" ]; then
      seed="$(cat "$file")"
      break
    fi
  done
  if [ -z "$seed" ]; then
    seed="$(cat /sys/class/net/*/address 2>/dev/null | grep -v '^00:00:00:00:00:00$' | head -n 1)" || seed=""
  fi
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
