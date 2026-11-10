# shellcheck shell=bash
# Source the helpers every release installer shares (release/lib/*.sh:
# plugin staging, env-file values, host facts). build-bundle.sh copies them
# next to this file, into the bundle's lib/; in a repo checkout they live in
# release/lib/.
_release_lib_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ -f "$_release_lib_dir/env.sh" ] || _release_lib_dir="$(cd "$_release_lib_dir/../../lib" && pwd)"
# shellcheck source=../../lib/plugins.sh
. "$_release_lib_dir/plugins.sh"
# shellcheck source=../../lib/env.sh
. "$_release_lib_dir/env.sh"
# shellcheck source=../../lib/host.sh
. "$_release_lib_dir/host.sh"
