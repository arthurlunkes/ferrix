#!/bin/sh
# Ferrix "full" image: one process serves the proxy (:80) and the admin API plus the
# web dashboard (:8080). On first start a config with a random admin token is created.
set -e

CONFIG_PATH="${FERRIX_CONFIG:-/etc/ferrix/ferrix.yaml}"

if [ ! -f "$CONFIG_PATH" ]; then
    mkdir -p "$(dirname "$CONFIG_PATH")"
    TOKEN=$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')
    # Write to a temp file first so a failure never leaves an empty config behind.
    sed "s/@ADMIN_TOKEN@/$TOKEN/" /usr/share/ferrix/ferrix.full.yaml > "$CONFIG_PATH.tmp"
    mv "$CONFIG_PATH.tmp" "$CONFIG_PATH"
    echo "================================================================"
    echo " Created $CONFIG_PATH"
    echo " Dashboard: http://<host>:8080   admin token: $TOKEN"
    echo "================================================================"
fi

exec /usr/local/bin/ferrix run --config "$CONFIG_PATH"
