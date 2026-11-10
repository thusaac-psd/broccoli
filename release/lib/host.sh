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
