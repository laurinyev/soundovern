#!/usr/bin/env bash
# Headless end-to-end check: server captures the real default sink monitor, the CLI client
# renders what it receives to a WAV (no speakers => no feedback loop). Optional sim loss in $1 (%).
set -euo pipefail
cd "$(dirname "$0")/.."
LOSS="${1:-0}"
OUT="${OUT:-/tmp/soundovern-loopback.wav}"
cargo build -q --release -p soundovern-server -p soundovern-cli-client
./target/release/soundovern-server --no-mdns --port 7421 --pin 123456 >/tmp/soundovern-server.log 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null || true' EXIT
sleep 1.5
./target/release/soundovern-cli-client 127.0.0.1:7421 --pin 123456 --wav "$OUT" --seconds 8 --sim-loss "$LOSS"
echo "WAV: $OUT (play something on the desktop while this runs to get non-silent audio)"
