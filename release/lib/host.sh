# shellcheck shell=bash
# Facts about the machine an installer runs on, shared by every installer.
# Run on the host itself, never inside a container.

# node_id_of ENGINE IMAGE ROLE: the id ROLE's binary takes on this machine when
# none is configured, asked of the image itself, so no installer derives an id:
# the binary's rule (common::node_identity::default_node_id, documented in the
# operator runbook) is the only one. The container runs in the host's network,
# where it sees the host's network cards, with the host's machine-id mounted
# where the binary reads it. The image must already be loaded: the engine never
# pulls (`--pull never`), since a closed lab reaches no registry. A container
# gets a new hostname whenever it is recreated, so a Compose deployment records
# this id in the role's env file, which keeps it stable across re-runs. Prints
# the id, or fails.
node_id_of() {
  local engine="$1" image="$2" role="$3" bin id machine_id="${BROCCOLI_MACHINE_ID_FILE:-/etc/machine-id}"
  local -a mount=()
  case "$role" in
    server) bin=/usr/local/bin/broccoli-server ;;
    worker) bin=/usr/local/bin/broccoli-worker ;;
    *) return 1 ;;
  esac
  [ -s "$machine_id" ] && mount=(--mount "type=bind,source=$machine_id,target=/run/broccoli/host/machine-id,readonly")
  id="$("$engine" run --rm --pull never --network host "${mount[@]}" --entrypoint "$bin" "$image" --node-id 2>/dev/null | tail -n 1)" || return 1
  case "$id" in
    ''|*[!A-Za-z0-9._-]*) return 1 ;;
  esac
  [ "${#id}" -le 128 ] || return 1
  printf '%s\n' "$id"
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
    | awk '!found { for (i = 1; i < NF; i++) if ($i == "src") { print $(i + 1); found = 1; break } }'
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
# address when there is no route to an upstream yet. An upstream on this very
# machine sees the gateway container's Docker bridge address instead, which
# no LAN address covers, so that case is warned about on stderr.
gateway_source_addrs() {
  local u host addr out="" list local_upstream=""
  read -r -d "" -a list <<< "$1" || true
  for u in "${list[@]}"; do
    host="${u%:*}"
    host="${host#[}"
    host="${host%]}"
    addr="$(source_ip_for "$host")"
    case "$host" in
      127.* | ::1 | localhost) local_upstream="$u" ;;
      *) [ "$addr" != "$host" ] || local_upstream="$u" ;;
    esac
    [ -n "$addr" ] || addr="$(hostname -I 2>/dev/null | awk '{print $1}')" || addr=""
    [ -n "$addr" ] || continue
    case " $out " in
      *" $addr "*) ;;
      *) out="$out${out:+ }$addr" ;;
    esac
  done
  if [ -n "$local_upstream" ]; then
    echo "warning: upstream $local_upstream runs on this machine; it sees this gateway as a Docker bridge address, not a LAN one. Run the gateway on a host of its own." >&2
  fi
  printf '%s\n' "$out"
}
