#!/bin/sh
# Start the f4x MCP server (`f4x mcp`, registered in .mcp.json).
#
# It builds f4x-cli into its own target dir, target/mcp, so starting the server never
# waits on the cargo lock of another build in target/ (cargo test, cargo xtask, a
# Windows build). If that build fails, e.g. because the tree is mid-edit, the last
# server that built is used. Cargo's output goes to stderr: stdout is the MCP stream.
root=$(cd "$(dirname "$0")/.." && pwd)
bin="$root/target/mcp/debug/f4x"
if ! cargo build --quiet --manifest-path "$root/Cargo.toml" -p f4x-cli --target-dir "$root/target/mcp" >&2; then
    echo "f4x-mcp: build failed; using the last built server" >&2
fi
if [ ! -x "$bin" ]; then
    echo "f4x-mcp: $bin does not exist" >&2
    exit 1
fi
exec "$bin" mcp "$@"
