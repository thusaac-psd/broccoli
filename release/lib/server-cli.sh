# shellcheck shell=bash
# server_cli ARGS...: run `broccoli-server ARGS` with this node's server
# configuration, the way the server itself runs: the native install's binary
# from its data directory (it reads config/config.toml there), or the running
# server container through `docker exec`, which carries the container's
# environment. Stdin and stdout pass through, so an archive can stream in or
# out without a shared directory. BROCCOLI_SERVER_CONTAINER names the container
# when the host runs more than one server.
server_cli() {
  local data="${BROCCOLI_DATA_DIR:-/data/broccoli}" containers
  if [ -z "${BROCCOLI_SERVER_CONTAINER:-}" ] && [ -x "$data/target/release/server" ] \
     && [ -f "$data/config/config.toml" ]; then
    ( cd "$data" && ./target/release/server "$@" )
    return
  fi
  command -v docker >/dev/null 2>&1 || {
    echo "error: no native server in $data and no docker to reach a server container" >&2
    return 2
  }
  containers="${BROCCOLI_SERVER_CONTAINER:-$(docker ps -q --filter label=com.docker.compose.service=server)}"
  case "$containers" in
    "") echo "error: no running server container on this host; run this on the server node" >&2; return 2 ;;
    *[[:space:]]*) echo "error: several server containers run here; set BROCCOLI_SERVER_CONTAINER to one of: $(echo $containers)" >&2; return 2 ;;
  esac
  docker exec -i "$containers" /usr/local/bin/broccoli-server "$@"
}
