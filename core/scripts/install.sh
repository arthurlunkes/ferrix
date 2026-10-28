#!/usr/bin/env bash
# ==============================================================================
# Ferrix — Instalação em Linux via systemd
#
# Instala o ferrix.service (proxy + API de administração) e, se o Node.js estiver
# disponível para compilar, os arquivos do painel web em /usr/share/ferrix/dashboard,
# servidos pelo próprio Ferrix (admin.dashboard_dir) — sem nenhum processo extra.
#
# Uso:
#   ./install.sh              build + instala o Core e o painel
#   ./install.sh --core-only  build + instala só o Core (sem painel web)
# ==============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
PROJECT_ROOT="$(cd "${CORE_DIR}/.." && pwd)"
FRONTEND_DIR="${PROJECT_ROOT}/frontend"
DASHBOARD_DIR="/usr/share/ferrix/dashboard"

BIN_NAME="ferrix"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/ferrix"
CORE_SERVICE_FILE="/etc/systemd/system/ferrix.service"

CORE_ONLY=false
for arg in "$@"; do
    case "$arg" in
        --core-only) CORE_ONLY=true ;;
    esac
done

# Dono dos serviços: o usuário que chamou o script (mesmo executado com sudo),
# nunca root — o Core só precisa escrever em ${CONFIG_DIR}.
RUN_USER="${SUDO_USER:-$(id -un)}"

as_root() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    else
        sudo "$@"
    fi
}

echo "🦀 Instalando o Ferrix (usuário dos serviços: ${RUN_USER})..."

# ------------------------------------------------------------------------------
# 1. Core em Rust
# ------------------------------------------------------------------------------
echo "==> Compilando o Core (cargo build --release)..."
cd "${CORE_DIR}"
cargo build --release

# O binário sai em <raiz-do-workspace>/target/release, não em core/target — os
# dois viviam em Cargo.toml separados antes do projeto virar um workspace.
RELEASE_BIN="${PROJECT_ROOT}/target/release/${BIN_NAME}"
if [ ! -f "${RELEASE_BIN}" ]; then
    echo "❌ Erro: binário não encontrado em ${RELEASE_BIN}" >&2
    exit 1
fi

echo "==> Instalando o executável em ${INSTALL_DIR}/${BIN_NAME}..."
as_root cp -f "${RELEASE_BIN}" "${INSTALL_DIR}/${BIN_NAME}"
as_root chmod +x "${INSTALL_DIR}/${BIN_NAME}"

# O serviço roda como ${RUN_USER}, não root — mas portas abaixo de 1024 (80, 443...) só
# aceitam bind de root por padrão no Linux. setcap dá exatamente essa permissão ao
# binário, sem precisar rodar o processo inteiro como root — é o mesmo mecanismo que o
# próprio nginx usa em pacotes de distribuição.
if command -v setcap >/dev/null 2>&1; then
    echo "==> Concedendo permissão para escutar em portas privilegiadas (setcap)..."
    as_root setcap 'cap_net_bind_service=+ep' "${INSTALL_DIR}/${BIN_NAME}"
else
    echo "⚠️  'setcap' não encontrado (pacote 'libcap2-bin' no Debian/Ubuntu)." >&2
    echo "    Sem ele, o Ferrix só consegue escutar em portas 80/443 rodando como root." >&2
    echo "    Instale-o e rode de novo, ou ajuste o serviço para rodar como root." >&2
fi

echo "==> Preparando ${CONFIG_DIR}..."
as_root mkdir -p "${CONFIG_DIR}"
if [ ! -f "${CONFIG_DIR}/ferrix.yaml" ] && [ -f "${PROJECT_ROOT}/ferrix.yaml" ]; then
    as_root cp "${PROJECT_ROOT}/ferrix.yaml" "${CONFIG_DIR}/ferrix.yaml"
fi
# O processo roda como ${RUN_USER}, não root: sem isto ele não conseguiria
# persistir hot-reload nem gravar o arquivo de PID.
as_root chown -R "${RUN_USER}:${RUN_USER}" "${CONFIG_DIR}"

if command -v systemctl >/dev/null 2>&1; then
    echo "==> Configurando ${CORE_SERVICE_FILE}..."
    TMP_SERVICE=$(mktemp)
    cat <<EOF > "${TMP_SERVICE}"
[Unit]
Description=Ferrix Core - Reverse Proxy Engine & Control Plane API
After=network.target network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${RUN_USER}
WorkingDirectory=${CONFIG_DIR}
ExecStart=${INSTALL_DIR}/${BIN_NAME} --config ${CONFIG_DIR}/ferrix.yaml
# `systemctl reload ferrix`: re-read the configuration without dropping connections.
ExecReload=/bin/kill -HUP \$MAINPID
# O systemd já reinicia o Ferrix se ele cair; dispensa o processo master dele e
# economiza a memória de um segundo processo.
Environment=FERRIX_SUPERVISOR=0
Restart=on-failure
RestartSec=1s
LimitNOFILE=65536
NoNewPrivileges=true
# Permite escutar em portas privilegiadas (80, 443...) sem rodar como root. Concedida
# pelo próprio systemd na hora de subir o processo, então não conflita com
# NoNewPrivileges — diferente do setcap no binário, que esse ajuste bloquearia.
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
EOF
    as_root mv "${TMP_SERVICE}" "${CORE_SERVICE_FILE}"
    as_root systemctl daemon-reload
    echo "✅ ferrix.service configurado."
fi

if [ "${CORE_ONLY}" = true ]; then
    echo ""
    echo "🎉 Ferrix Core instalado (sem o painel web, por --core-only)."
    echo "   Iniciar:        sudo systemctl start ferrix"
    echo "   Habilitar boot: sudo systemctl enable ferrix"
    exit 0
fi

# ------------------------------------------------------------------------------
# 2. Painel web (frontend React), servido pelo próprio Ferrix
# ------------------------------------------------------------------------------
if ! command -v npm >/dev/null 2>&1; then
    echo "⚠️  npm não encontrado — pulando o painel web." >&2
    echo "    O Core acima já foi instalado e funciona sozinho (sem painel)." >&2
    echo "    Instale o Node.js (só para compilar) e rode este script de novo." >&2
    exit 0
fi

echo "==> Compilando o painel web (frontend/)..."
( cd "${FRONTEND_DIR}" && npm install && npm run build )

echo "==> Instalando o painel em ${DASHBOARD_DIR}..."
as_root mkdir -p "${DASHBOARD_DIR}"
as_root cp -r "${FRONTEND_DIR}/dist/." "${DASHBOARD_DIR}/"

echo ""
echo "🎉 Ferrix instalado com sucesso (Core + Painel)!"
echo ""
echo "   Para iniciar agora:   sudo systemctl start ferrix"
echo "   Para habilitar no boot: sudo systemctl enable ferrix"
echo "   Para ver os logs:     journalctl -u ferrix -f"
echo ""
echo "   Para ligar o painel, adicione em ${CONFIG_DIR}/ferrix.yaml:"
echo "     admin:"
echo "       dashboard_dir: ${DASHBOARD_DIR}"
echo "   e acesse http://<admin.listen> (por padrão http://127.0.0.1:8081)."
echo "   O painel precisa das métricas ligadas (metrics: {enabled: true})."
echo ""
echo "   Na primeira vez que o Core rodar, ele gera um token de administração e"
echo "   grava em ${CONFIG_DIR}/ferrix.yaml — veja com:"
echo "     journalctl -u ferrix | grep -i token"
