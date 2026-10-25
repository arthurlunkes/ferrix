#!/usr/bin/env bash
# Benchmark Ferrix × nginx.
#
# Uso: benchmarks/run.sh [ferrix|nginx|both] [duração]   (padrão: both 10s)
#
# Requisitos: Docker (nginx e wrk rodam em containers), taskset e uma máquina com pelo
# menos 16 núcleos lógicos. Cada lado fica preso a um conjunto de núcleos para que um não
# roube CPU do outro:
#   0-3   servidor sob teste (Ferrix ou nginx, 4 workers cada)
#   4-7   upstream (nginx devolvendo 20 bytes)
#   8-15  gerador de carga (wrk, 8 threads, 256 conexões)
set -euo pipefail

WHO=${1:-both}
DUR=${2:-10s}
CONNS=${CONNS:-256}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
BIN="$ROOT/target/release/ferrix"
WORK=$(mktemp -d)
NGINX_IMG=nginx:1.27-alpine
WRK_IMG=williamyeh/wrk

cleanup() {
    [ -n "${FERRIX_PID:-}" ] && kill "$FERRIX_PID" 2>/dev/null || true
    docker rm -f ferrix-bench-up ferrix-bench-nginx >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT

echo "==> compilando o Ferrix (release)"
cargo build --release -q --manifest-path "$ROOT/Cargo.toml"

# Arquivos servidos: um pequeno (~1,4 KB) e um grande (~133 KB).
mkdir -p "$WORK/www"
head -c 1024 /dev/urandom | base64 > "$WORK/www/small.txt"
head -c 100000 /dev/urandom | base64 -w0 > "$WORK/www/big.txt"
echo '<h1>ok</h1>' > "$WORK/www/index.html"

sed "s#@WWW@#$WORK/www#" "$HERE/conf/ferrix.json" > "$WORK/ferrix.json"

docker run -d --name ferrix-bench-up --network host --cpuset-cpus 4-7 \
    -v "$HERE/conf/upstream.conf:/etc/nginx/nginx.conf:ro" $NGINX_IMG >/dev/null

targets=()
if [ "$WHO" = ferrix ] || [ "$WHO" = both ]; then
    taskset -c 0-3 "$BIN" run -c "$WORK/ferrix.json" > "$WORK/ferrix.log" 2>&1 &
    FERRIX_PID=$!
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

[ -n "${FERRIX_PID:-}" ] && echo && echo "memória do Ferrix (RSS): $(ps -o rss= -p "$FERRIX_PID") KB"
