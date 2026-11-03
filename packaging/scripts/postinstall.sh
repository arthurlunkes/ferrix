#!/bin/sh
# Runs after the package is installed or upgraded (deb, rpm and Arch packages).
set -e

if ! getent group ferrix >/dev/null 2>&1; then
    groupadd --system ferrix
fi
if ! getent passwd ferrix >/dev/null 2>&1; then
    useradd --system --gid ferrix --home-dir /etc/ferrix --no-create-home \
        --shell /usr/sbin/nologin --comment "Ferrix web server" ferrix
fi

mkdir -p /etc/ferrix
chown ferrix:ferrix /etc/ferrix
chmod 750 /etc/ferrix

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || true
    if systemctl is-active --quiet ferrix; then
        # Upgrade: pick up the new binary.
        systemctl restart ferrix || true
    else
        systemctl enable ferrix >/dev/null 2>&1 || true
        if ! systemctl start ferrix; then
            echo "Ferrix is installed but did not start; see: journalctl -u ferrix"
            echo "(another web server may already be using port 80)"
        fi
    fi
fi

cat <<'EOF'

Ferrix is installed.
  Open:          http://localhost/
  Configuration: /etc/ferrix/ferrix.yaml (every option is listed there, commented out)
  Apply changes: sudo systemctl reload ferrix
  Logs:          journalctl -u ferrix -f
EOF
