#!/bin/sh
set -e

CONFIG_PATH="${FERRIX_CONFIG:-/etc/ferrix/ferrix.yaml}"

if [ ! -f "$CONFIG_PATH" ] && [ -f /etc/ferrix/ferrix.example.yaml ]; then
    echo "[Ferrix Full] Initializing default configuration at $CONFIG_PATH..."
    mkdir -p "$(dirname "$CONFIG_PATH")"
    cp /etc/ferrix/ferrix.example.yaml "$CONFIG_PATH"
fi

echo "================================================================"
echo "🚀 Starting Ferrix Core (Rust Engine)..."
echo "================================================================"
/usr/local/bin/ferrix --config "$CONFIG_PATH" &
CORE_PID=$!

trap 'echo "[Ferrix Full] Stopping services..."; kill $CORE_PID; exit 0' SIGTERM SIGINT

echo "================================================================"
echo "🌐 Starting Ferrix Web Dashboard & Gateway (Node.js)..."
echo "   Dashboard Web: http://0.0.0.0:8080"
echo "   Proxy Traffic: http://0.0.0.0:8000"
echo "   Core Admin:    http://127.0.0.1:8081"
echo "================================================================"
cd /app/backend && exec node dist/server.js

