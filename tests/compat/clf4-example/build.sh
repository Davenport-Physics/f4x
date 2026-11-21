#!/bin/sh
# Spike S6: build CommonLibF4's ExampleProject as an F4SE plugin DLL with clang-cl + lld-link.
# Prints the DLL path (target/compat/clf4-example/ExampleProject.dll) on success.
# The flags live in build.py (section "Flags"). See docs/spikes/S6-clf4-canary.md.
set -eu
exec python3 -I "$(dirname "$0")/build.py" "$@"
