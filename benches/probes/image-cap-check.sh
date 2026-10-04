#!/usr/bin/env bash
# Checks a nrese Linux image for a capped deployment (written for zebratlas.org/sparql): a release
# bulk-loaded and served under a memory cap without swap, its queries answered over HTTP
# (five times each, then 8 concurrent streams), a restart, and the cgroup peak of each.
# Usage: image-cap-check.sh IMAGE RELEASE_DIR QUERY_DIR OUT_DIR
# RELEASE_DIR holds graph.ttl and SHA256SUMS (the Zebratlas release layout); QUERY_DIR holds
# .rq files. CERT_CAP (default 4g) is the memory cap, CERT_ENV one VAR=value for the
# server (NRESE_QUERY_CACHE_BYTES=0 measures without the result cache). Needs Docker and
# curl; runs on Git Bash or Linux.
set -u
img=$1; rel=$2; queries=$3; out=$4
cap=${CERT_CAP:-4g}
vol=nrese-cert-data
name=nrese-cert-serve
port=18080
mkdir -p "$out/answers"
log() { printf '%s %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$out/cert.log"; }

docker rm -f "$name" >/dev/null 2>&1
docker volume rm "$vol" >/dev/null 2>&1
docker volume create "$vol" >/dev/null

log "image $img ($(docker image inspect "$img" --format '{{.Id}}'))"
log "release $rel: $(grep ' graph.ttl$' "$rel/SHA256SUMS")"
log "cap $cap, no swap"

# 1. Bulk load with the default (parallel) loader.
MSYS_NO_PATHCONV=1 docker run --rm --name nrese-cert-load --memory "$cap" --memory-swap "$cap" \
  -v "$vol:/var/lib/nrese/data" -v "$({ command -v cygpath >/dev/null && cygpath -w "$rel" || printf %s "$rel"; }):/release:ro" --entrypoint sh "$img" -c '
    start=$(date +%s)
    nrese-server load /release/graph.ttl
    code=$?
    echo "load exit $code in $(( $(date +%s) - start )) s, cgroup peak $(( $(cat /sys/fs/cgroup/memory.peak) / 1048576 )) MiB"
    du -sh /var/lib/nrese/data' > "$out/load.log" 2>&1
log "load: $(grep 'load exit' "$out/load.log") ; store $(tail -1 "$out/load.log")"

# 2. Serve under the same cap.
started=$(date +%s%N)
MSYS_NO_PATHCONV=1 docker run -d --name "$name" --memory "$cap" --memory-swap "$cap" \
  ${CERT_ENV:+-e "$CERT_ENV"} -v "$vol:/var/lib/nrese/data" -p "127.0.0.1:$port:8080" "$img" >/dev/null
for _ in $(seq 1 600); do
  curl -sf "http://127.0.0.1:$port/readyz" >/dev/null && break
  sleep 0.5
done
log "ready in $(( ($(date +%s%N) - started) / 1000000 )) ms; $(docker exec "$name" sh -c 'echo $(( $(cat /sys/fs/cgroup/memory.current) / 1048576 ))') MiB after start"

ask() {
  local file=$1 dest=$2
  local accept='text/tab-separated-values'
  grep -qiE '^[[:space:]]*ASK' "$file" && accept='application/sparql-results+json'
  curl -s -o "$dest" -w '%{http_code} %{time_total}' -H "Accept: $accept" \
    --data-urlencode "query@$file" "http://127.0.0.1:$port/dataset/query"
}

# 3. Each query five times, one after another (the first is cold).
for q in "$queries"/*.rq; do
  n=$(basename "$q" .rq)
  times=""
  for i in 1 2 3 4 5; do
    r=$(ask "$q" "$out/answers/$n.out")
    times="$times ${r#* }"
    code=${r%% *}
  done
  rows=$(( $(wc -l < "$out/answers/$n.out") - 1 ))
  grep -q '"boolean"' "$out/answers/$n.out" && rows=$(grep -o '"boolean":[a-z]*' "$out/answers/$n.out")
  log "query $n: http $code, answer $rows, seconds$times"
done

# 4. All queries in 8 concurrent streams, three rounds.
for s in 1 2 3 4 5 6 7 8; do
  ( for round in 1 2 3; do for q in "$queries"/*.rq; do ask "$q" /dev/null >/dev/null; done; done ) &
done
wait
log "concurrent: 8 streams x 3 rounds done; peak $(docker exec "$name" sh -c 'echo $(( $(cat /sys/fs/cgroup/memory.peak) / 1048576 ))') MiB, oom_kill $(docker exec "$name" sh -c 'grep oom_kill /sys/fs/cgroup/memory.events | head -1')"

# 5. Restart: the data survives and the server is ready again.
first=$(ls "$queries"/*.rq | head -1)
started=$(date +%s%N)
docker restart "$name" >/dev/null
for _ in $(seq 1 600); do
  curl -sf "http://127.0.0.1:$port/readyz" >/dev/null && break
  sleep 0.5
done
r=$(ask "$first" "$out/restart.out")
log "restart: ready in $(( ($(date +%s%N) - started) / 1000000 )) ms; first query http ${r%% *} answer $(( $(wc -l < "$out/restart.out") - 1 )) rows"
log "state: $(docker inspect "$name" --format 'OOMKilled={{.State.OOMKilled}} restarts={{.RestartCount}}')"

docker logs "$name" > "$out/server.log" 2>&1
docker rm -f "$name" >/dev/null
docker volume rm "$vol" >/dev/null
log "done"
