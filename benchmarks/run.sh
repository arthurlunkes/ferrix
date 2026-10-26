#!/usr/bin/env bash
# Benchmark Ferrix × nginx.
#
# Uso: benchmarks/run.sh [ferrix|nginx|both] [duração]   (padrão: both 10s)
#      METRICS=1 benchmarks/run.sh ...   mede o Ferrix com metrics.enabled = true
#
# Requisitos: só Docker e uma máquina com pelo menos 16 núcleos lógicos. Tudo roda em
# containers (o Ferrix é compilado pela imagem `core` do Dockerfile do projeto), então o
# resultado não depende do Rust ou das bibliotecas instaladas na máquina. Cada lado fica
# preso a um conjunto de núcleos para que um não roube CPU do outro:
#   0-3   servidor sob teste (Ferrix ou nginx, 4 workers cada)
#   4-7   upstream (nginx devolvendo 20 bytes)
#   8-15  gerador de carga (wrk, 8 threads, 256 conexões)
set -euo pipefail

WHO=${1:-both}
DUR=${2:-10s}
CONNS=${CONNS:-256}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
FERRIX_IMG=ferrix-bench:local
WORK=$(mktemp -d)
chmod 755 "$WORK"   # o container do Ferrix roda com um usuário sem privilégios
NGINX_IMG=nginx:1.27-alpine
WRK_IMG=williamyeh/wrk

cleanup() {
    docker rm -f ferrix-bench-up ferrix-bench-nginx ferrix-bench-ferrix >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT

echo "==> compilando a imagem do Ferrix (Dockerfile, alvo core)"
docker build -q --target core -t "$FERRIX_IMG" "$ROOT" >/dev/null

# Arquivos servidos: um pequeno (~1,4 KB) e um grande (~133 KB).
mkdir -p "$WORK/www"
head -c 1024 /dev/urandom | base64 > "$WORK/www/small.txt"
head -c 100000 /dev/urandom | base64 -w0 > "$WORK/www/big.txt"
echo '<h1>ok</h1>' > "$WORK/www/index.html"

sed "s#@WWW@#/work/www#" "$HERE/conf/ferrix.json" > "$WORK/ferrix.json"
if [ "${METRICS:-0}" = 1 ]; then
    sed -i 's#^{#{ "metrics": { "enabled": true },#' "$WORK/ferrix.json"
fi

docker run -d --name ferrix-bench-up --network host --cpuset-cpus 4-7 \
    -v "$HERE/conf/upstream.conf:/etc/nginx/nginx.conf:ro" $NGINX_IMG >/dev/null

targets=()
if [ "$WHO" = ferrix ] || [ "$WHO" = both ]; then
    docker run -d --name ferrix-bench-ferrix --network host --cpuset-cpus 0-3 \
        -v "$WORK:/work:ro" "$FERRIX_IMG" run -c /work/ferrix.json >/dev/null
    targets+=("ferrix=9001")
fi
if [ "$WHO" = nginx ] || [ "$WHO" = both ]; then
    docker run -d --name ferrix-bench-nginx --network host --cpuset-cpus 0-3 \
        -v "$HERE/conf/nginx.conf:/etc/nginx/nginx.conf:ro" \
        -v "$WORK/www:/www:ro" $NGINX_IMG >/dev/null
    targets+=("nginx=9002")
fi
sleep 2

wrk() {
    docker run --rm --network host --cpuset-cpus 8-15 $WRK_IMG \
        -t8 -c"$CONNS" -d"$2" --latency "http://127.0.0.1$1"
}

printf "\n%-8s %-12s %12s %10s %10s\n" servidor cenário "req/s" p50 p99
for path in /small.txt /big.txt /proxy/; do
    for t in "${targets[@]}"; do
        name=${t%%=*}; port=${t##*=}
        wrk ":$port$path" 3s >/dev/null   # aquecimento
        out=$(wrk ":$port$path" "$DUR")
        if echo "$out" | grep -qE 'Non-2xx|Socket errors'; then
            echo "!! $name $path teve erros:"; echo "$out" | grep -E 'Non-2xx|Socket errors'
        fi
        printf "%-8s %-12s %12s %10s %10s\n" "$name" "$path" \
            "$(echo "$out" | awk '/Requests\/sec/{print $2}')" \
            "$(echo "$out" | awk '/ 50%/{print $2}')" \
            "$(echo "$out" | awk '/ 99%/{print $2}')"
    done
done

for c in ferrix-bench-ferrix ferrix-bench-nginx; do
    mem=$(docker stats --no-stream --format '{{.MemUsage}}' "$c" 2>/dev/null | cut -d/ -f1)
    [ -n "$mem" ] && echo "memória de ${c#ferrix-bench-}: $mem"
done
