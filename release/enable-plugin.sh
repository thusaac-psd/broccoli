#!/usr/bin/env bash
# Enable optional plugins shipped with this bundle, for example
#   ./enable-plugin.sh codelink-qualifier codelink-bracket
# Run it on each node whose role loads plugins, then restart every server.
# Reload All on the admin Plugins page reloads only the server that handled
# the click, so it is enough only when there is a single server.
# Without arguments, lists the optional plugins in this bundle.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/plugins.sh
. "$here/lib/plugins.sh"

dir="$here"
[ -d "$dir/plugins-available" ] || dir="$here/compose"
[ -d "$dir/plugins-available" ] || { echo "no plugins-available directory next to $0" >&2; exit 2; }

if [ $# -eq 0 ]; then
  echo "Optional plugins in this bundle:"
  list_optional_plugins "$dir" | sed 's/^/  /'
  exit 0
fi
enable_bundle_plugins "$dir" "$@"
