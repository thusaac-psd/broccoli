#!/usr/bin/env bash
# Prove the built worker image's g++ DEFAULT dialect is C++14 (gnu++14), not C++17
# -- the default C++ flags carry no -std, so submissions inherit gcc 9.3's
# default, matching NOI Linux 2.0.
#
# Uses FRONT-END facts, not a library discriminator: libstdc++ keeps "removed"
# templates (std::random_shuffle, std::auto_ptr) compilable past their standard via
# _GLIBCXX_USE_DEPRECATED, so a stdlib removal proves nothing. Two checks:
#   (1) the __cplusplus macro under default flags is exactly 201402L; and
#   (2) a dynamic exception specification `throw(int)` -- valid C++14, a HARD
#       front-end ERROR in C++17 -- compiles under the default and is rejected
#       under -std=c++17.
# Self-skips unless docker + a prebuilt worker image are available.
set -euo pipefail
IMAGE="${BROCCOLI_WORKER_IMAGE:-}"
if ! command -v docker >/dev/null 2>&1; then echo "SKIP: docker not available"; exit 0; fi
if [ -z "$IMAGE" ]; then
  echo "SKIP: set BROCCOLI_WORKER_IMAGE to a built runtime-icpc/runtime-full image"; exit 0
fi

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
cat > "$T/disc.cpp" <<'CPP'
void f() throw(int) {}   // dynamic exception spec: valid C++14, HARD ERROR in C++17
int main() { return 0; }
CPP
# The worker image's entrypoint drops to an unprivileged `worker` user whose
# in-container uid differs from the host owner of this mktemp dir (mode 0700).
# Linux DAC is numeric, so `worker` can neither traverse /w nor write into it.
# Make the source world-traversable+readable and send g++ output to the
# always-writable /tmp so the check exercises the DIALECT, not mount perms.
chmod 0755 "$T"; chmod 0644 "$T/disc.cpp"

run() { docker run --rm -v "$T:/w:ro" "$IMAGE" bash -lc "$1"; }

# (1) Authoritative: the default dialect macro is exactly C++14 (201402L), not C++17+.
macro="$(run 'echo | g++ -E -dM -x c++ -' | grep '__cplusplus' || true)"
case "$macro" in
  *201402L*) : ;;
  *) echo "FAIL: default __cplusplus macro is [$macro], want 201402L -- default dialect is not gnu++14"; exit 1 ;;
esac

# (2) Front-end discriminator: default (no -std) => gnu++14 => MUST compile.
if ! run 'g++ /w/disc.cpp -o /tmp/a.out'; then
  echo "FAIL: default g++ rejected a C++14 dynamic exception spec -- default is not gnu++14"; exit 1
fi
# ...and explicit -std=c++17 MUST reject it (ISO C++17 removed dynamic exception specs).
if run 'g++ -std=c++17 /w/disc.cpp -o /tmp/b.out' 2>/dev/null; then
  echo "FAIL: -std=c++17 accepted a dynamic exception spec -- discriminator invalid"; exit 1
fi
echo "PASS: worker g++ default dialect is C++14 (gnu++14), not C++17"
