#!/usr/bin/env bash
# TLB Infinitum - one command demo.
#
# Builds and starts price-fetcher and price-web, then opens the dashboard
# in your browser.
#
# This uses the UDP transport instead of CAN, so it needs no CAN bus and no
# root rights, and runs on macOS, Linux and Windows (WSL) alike.
# The real device uses CAN; see README.md.
#
# Requirements: Rust (https://rustup.rs) and an internet connection.
# Stop it with Ctrl+C.

set -euo pipefail
cd "$(dirname "$0")"

URL="http://localhost:8080"

echo ">>> Building (the first time takes a few minutes)..."
( cd price-web && cargo build --quiet )
( cd price-fetcher && cargo build --quiet )

# Start the web server first, so it is listening before prices arrive
echo ">>> Starting the web server..."
price-web/target/debug/price-web --udp &
WEB=$!
sleep 1

echo ">>> Starting the price fetcher..."
price-fetcher/target/debug/price-fetcher --udp &
FETCHER=$!

# Stop both programs when this script ends, whatever ends it
cleanup() { kill "$WEB" "$FETCHER" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

# Open the browser once the page actually answers
# Note the "|| true" everywhere: explorer.exe returns an error code even when
# it works, and the script must not stop because of it.
open_browser() {
  case "$(uname -s)" in
    Darwin) open "$URL" || true ;;
    *)
      if grep -qi microsoft /proc/version 2>/dev/null; then
        explorer.exe "$URL" >/dev/null 2>&1 || true   # Windows, through WSL
      else
        xdg-open "$URL" >/dev/null 2>&1 || true       # Linux desktop
      fi
      ;;
  esac
}

for _ in $(seq 1 30); do
  if curl -s -f --max-time 2 -o /dev/null "$URL"; then
    sleep 1          # give the server a moment before the browser asks
    open_browser
    break
  fi
  sleep 1
done

echo ">>> Running. Dashboard: $URL   (Ctrl+C to stop)"
wait "$WEB"
