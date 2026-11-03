#!/bin/sh
# Runs before the package is removed. On deb/rpm upgrades the old package's script
# also runs; only stop the service when the package is really going away.
set -e

case "$1" in
    upgrade|1) exit 0 ;; # deb upgrade / rpm upgrade (1 = a version stays installed)
esac

if [ -d /run/systemd/system ]; then
    systemctl disable --now ferrix >/dev/null 2>&1 || true
fi
