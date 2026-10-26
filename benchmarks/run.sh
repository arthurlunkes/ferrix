#!/usr/bin/env bash
# Benchmark Ferrix against nginx, Traefik, Pingora, Pingap and Ferron.
#
# Uso: benchmarks/run.sh [servidores...] [-d duração]
#        benchmarks/run.sh                      todos, 10 s por cenário
#        benchmarks/run.sh ferrix nginx -d 30s  só esses dois, 30 s por cenário
#      METRICS=1 benchmarks/run.sh ...          mede o Ferrix com metrics.enabled = true
#
# Requisitos: só Docker e uma máquina com pelo menos 16 núcleos lógicos. Tudo roda em
# containers com versões fixas, então o resultado não depende do que está instalado na
# máquina. Os servidores rodam UM DE CADA VEZ, sempre presos aos mesmos núcleos:
#   0-3   servidor sob teste (4 workers/threads em todos)
#   4-7   upstream (nginx devolvendo 20 bytes)
#   8-15  gerador de carga (wrk, 8 threads, 256 conexões)
#
# Cenários: /small.txt (~1,4 KB), /big.txt (~133 KB) e /proxy/ (proxy reverso para o
# upstream). Traefik e Pingora são só proxies, não servem arquivos: nos cenários de
# arquivo eles aparecem como "-".
set -euo pipefail

DUR=10s
SERVERS=()
while [ $# -gt 0 ]; do
    case "$1" in
        -d) DUR=$2; shift 2 ;;
        *) SERVERS+=("$1"); shift ;;
    esac
done
[ ${#SERVERS[@]} -eq 0 ] && SERVERS=(ferrix nginx traefik pingora pingap ferron)

CONNS=${CONNS:-256}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
WORK=$(mktemp -d)
chmod 755 "$WORK"   # alguns servidores rodam com usuário sem privilégios

NGINX_IMG=nginx:1.27-alpine
WRK_IMG=williamyeh/wrk
TRAEFIK_IMG=traefik:v3.5.6
PINGAP_IMG=vicanso/pingap:0.16.1
FERRON_IMG=ferronserver/ferron:2.8.1
FERRIX_IMG=ferrix-bench:local
PINGORA_IMG=ferrix-bench-pingora:local

cleanup() {
    docker rm -f ferrix-bench-up ferrix-bench-server >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT

mkdir -p "$WORK/www"
head -c 1024 /dev/urandom | base64 > "$WORK/www/small.txt"
head -c 100000 /dev/urandom | base64 -w0 > "$WORK/www/big.txt"
echo '<h1>ok</h1>' > "$WORK/www/index.html"
chmod -R a+rX "$WORK"

sed "s#@WWW@#/work/www#" "$HERE/conf/ferrix.json" > "$WORK/ferrix.json"
if [ "${METRICS:-0}" = 1 ]; then
    sed -i 's#^{#{ "metrics": { "enabled": true },#' "$WORK/ferrix.json"
fi

for s in "${SERVERS[@]}"; do
    case $s in
        ferrix)
            echo "==> compilando a imagem do Ferrix (Dockerfile, alvo core)"
            docker build -q --target core -t "$FERRIX_IMG" "$ROOT" >/dev/null ;;
        pingora)
            echo "==> compilando o proxy de benchmark do Pingora (benchmarks/pingora)"
            docker build -q -t "$PINGORA_IMG" "$HERE/pingora" >/dev/null ;;
    esac
done

docker run -d --name ferrix-bench-up --network host --cpuset-cpus 4-7 \
    -v "$HERE/conf/upstream.conf:/etc/nginx/nginx.conf:ro" $NGINX_IMG >/dev/null

# Sobe um servidor nos núcleos 0-3 e imprime a porta em que ele escuta.
start() {
    local run=(docker run -d --name ferrix-bench-server --network host --cpuset-cpus 0-3)
    case $1 in
        ferrix)  "${run[@]}" -v "$WORK:/work:ro" "$FERRIX_IMG" run -c /work/ferrix.json; echo 9001 >&3 ;;
        nginx)   "${run[@]}" -v "$HERE/conf/nginx.conf:/etc/nginx/nginx.conf:ro" \
                     -v "$WORK/www:/www:ro" $NGINX_IMG; echo 9002 >&3 ;;
        traefik) "${run[@]}" -e GOMAXPROCS=4 -v "$HERE/conf/traefik.yml:/etc/traefik/traefik.yml:ro" \
                     -v "$HERE/conf/traefik-dynamic.yml:/etc/traefik/traefik-dynamic.yml:ro" \
                     $TRAEFIK_IMG; echo 9003 >&3 ;;
        pingap)  "${run[@]}" -v "$HERE/conf/pingap.toml:/opt/pingap/conf/pingap.toml:ro" \
                     -v "$WORK/www:/www:ro" $PINGAP_IMG pingap -c /opt/pingap/conf/pingap.toml; echo 9004 >&3 ;;
        ferron)  "${run[@]}" -v "$HERE/conf/ferron.kdl:/etc/ferron.kdl:ro" -v "$WORK/www:/www:ro" \
                     $FERRON_IMG /usr/sbin/ferron -c /etc/ferron.kdl; echo 9005 >&3 ;;
        pingora) "${run[@]}" "$PINGORA_IMG"; echo 9006 >&3 ;;
        *) echo "servidor desconhecido: $1" >&2; exit 1 ;;
    esac
}

serves_files() { case $1 in traefik|pingora) return 1 ;; *) return 0 ;; esac; }

wrk() {
    docker run --rm --network host --cpuset-cpus 8-15 $WRK_IMG \
        -t8 -c"$CONNS" -d"$2" --latency "http://127.0.0.1$1"
}

# PSS (memória proporcional) somada de todos os processos do container, lida por um
# container auxiliar com acesso ao /proc do host: funciona até em imagens sem shell.
pss_mb() {
    local pids
    pids=$(docker top ferrix-bench-server -o pid | tail -n +2 | tr '\n' ' ')
    docker run --rm --privileged --pid=host $NGINX_IMG sh -c \
        "for p in $pids; do awk '/^Pss:/{print \$2}' /proc/\$p/smaps_rollup; done" |
        awk '{s+=$1} END{printf "%.1f MB", s/1024}'
}

results=()
for s in "${SERVERS[@]}"; do
    echo "==> $s"
    port=$(start "$s" 3>&1 >/dev/null)
    sleep 2
    for path in /small.txt /big.txt /proxy/; do
        if [ "$path" != /proxy/ ] && ! serves_files "$s"; then
            results+=("$(printf "%-8s %-11s %12s %9s %9s" "$s" "$path" - - -)")
            continue
        fi
        wrk ":$port$path" 3s >/dev/null   # aquecimento
        out=$(wrk ":$port$path" "$DUR")
        if echo "$out" | grep -qE 'Non-2xx|Socket errors'; then
            echo "!! $s $path teve erros:"; echo "$out" | grep -E 'Non-2xx|Socket errors'
        fi
        results+=("$(printf "%-8s %-11s %12s %9s %9s" "$s" "$path" \
            "$(echo "$out" | awk '/Requests\/sec/{print $2}')" \
            "$(echo "$out" | awk '/ 50%/{print $2}')" \
            "$(echo "$out" | awk '/ 99%/{print $2}')")")
    done
    results+=("$(printf "%-8s %-11s %s" "$s" "memória" "$(pss_mb) (PSS, após a carga)")")
    docker rm -f ferrix-bench-server >/dev/null
done

printf "\n%-8s %-11s %12s %9s %9s\n" servidor cenário "req/s" p50 p99
printf "%s\n" "${results[@]}"
