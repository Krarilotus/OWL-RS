#!/usr/bin/env bash
# make.sh LINE_SERVER FRESH_OUTPUT_DIRECTORY PORT
# The fixture was written with server commit 58e547384ab85b595c3052d5d38995d701310b38.
# Writes a certified store whose last consumer of an anonymous union was deleted.
set -euo pipefail
bin=$1
out=$2
port=$3
here=$(cd "$(dirname "$0")" && pwd)
url=http://127.0.0.1:$port
mkdir "$out"
cat > "$out/nrese.toml" <<EOF
[server]
bind_addr = "127.0.0.1:$port"
[store]
mode = "on-disk"
data_dir = "$out/data"
[reasoner]
mode = "owl2-rl"
unnamed_classes = "skip"
EOF
"$bin" load --config "$out/nrese.toml" "$here/base.ttl"
"$bin" --config "$out/nrese.toml" > "$out/server.log" 2>&1 &
pid=$!
stop() {
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
}
trap stop EXIT
ready=false
for _ in $(seq 1 100); do
    if curl -fsS --max-time 2 "$url/readyz" > /dev/null 2>&1; then
        ready=true
        break
    fi
    kill -0 "$pid"
    sleep 0.05
done
"$ready"
prefix='PREFIX ex: <http://example.org/> PREFIX owl: <http://www.w3.org/2002/07/owl#>'
ask() {
    curl -fsS --max-time 5 --data-urlencode "query=$prefix ASK { $1 }" "$url/dataset/query"
}
stale='?u owl:unionOf ?list . ex:y a ?u'
ask "$stale" | tee "$out/before-delete.json" | grep -Eq '"boolean"[[:space:]]*:[[:space:]]*true'
curl -fsS --max-time 10 -H 'Content-Type: application/sparql-update' --data-binary \
    "$prefix DELETE { ex:k ex:consumer ?u } WHERE { ex:k ex:consumer ?u }" "$url/dataset/update"
ask "$stale" | tee "$out/after-delete.json" | grep -Eq '"boolean"[[:space:]]*:[[:space:]]*true'
ask 'ex:y a ex:C . ex:y ex:note ex:kept . ?u owl:unionOf ?list' > "$out/unchanged.json"
curl -fsS --max-time 5 "$url/dataset/info" > "$out/info.json"
test -s "$out/data/reasoning.state"
