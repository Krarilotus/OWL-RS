#!/usr/bin/env bash
# Every blocking call in HTTP is classified. The effectful exceptions move in v1.6.
set -euo pipefail
cd "$(dirname "$0")/.."

sites() {
    git grep --untracked --no-recurse-submodules -nE "$@" || test "$?" -eq 1
}

sites '\bspawn_blocking\b|\bblocking[[:space:]]*::[[:space:]]*within\b' -- \
    crates/nrese-server/src/http crates/nrese-server/src/replication.rs \
    crates/nrese-server/src/state.rs ':(exclude)crates/nrese-server/src/http/effect.rs' |
awk '
BEGIN {
    allow["http/access_api.rs|let record = tokio::task::spawn_blocking(move || access.access().apply(&by, change, &reason))"] = "v1.6 access change"
    allow["http/access_api.rs|let change_user = tokio::task::spawn_blocking(move || {"] = "v1.6 user change"
    allow["http/access_api.rs|let opened = tokio::task::spawn_blocking(move || {"] = "v1.6 login session"
    allow["http/admin_dataset.rs|let written = tokio::task::spawn_blocking(move || {"] = "v1.6 image backup"
    allow["http/api_v1.rs|tokio::task::spawn_blocking(move || repositories.repositories().create(&created, settings))"] = "v1.6 repository creation"
    allow["http/rdf4j.rs|tokio::task::spawn_blocking(move || repositories.repositories().create(&id, settings))"] = "v1.6 RDF4J repository creation"
    allow["http/api_v1.rs|tokio::task::spawn_blocking(move || store.rematerialise(program))"] = "v1.6 rematerialisation"
    allow["http/api_v1.rs|let file = tokio::task::spawn_blocking(move || {"] = "v1.6 import upload staging"
    allow["http/api_v1.rs|tokio::task::spawn_blocking(move || {"] = "v1.6 async import, rule upload and rule removal"
    count["http/api_v1.rs|tokio::task::spawn_blocking(move || {"] = 3
    allow["http/api_v1.rs|tokio::task::spawn_blocking(move || changing.patch_repository_settings(merge))"] = "v1.6 settings patch"
    allow["http/cache_api.rs|let pinning = tokio::task::spawn_blocking(move || store.pin_query(&name, &request, &token));"] = "v1.6 cache pin"
    allow["replication.rs|let applied = tokio::task::spawn_blocking(move || {"] = "v1.6 replication apply"
    allow["replication.rs|let (dir, manifest) = tokio::task::spawn_blocking(move || {"] = "v1.6 replication image staging"
    allow["state.rs|let principal = tokio::task::spawn_blocking(move || access.login(&user, &password, client))"] = "v1.6 login"

    allow["http/access_api.rs|tokio::task::spawn_blocking(move || access.access().history(limit))"] = "access history read"
    allow["http/api_v1.rs|let answer = tokio::task::spawn_blocking(move || {"] = "justification read"
    allow["http/api_v1.rs|let found = tokio::task::spawn_blocking(move || {"] = "explanation read"
    allow["http/api_v1.rs|let files = tokio::task::spawn_blocking(move || {"] = "import directory listing"
    allow["http/api_v1.rs|let sizes = tokio::task::spawn_blocking(move || store.graph_sizes(&scope))"] = "graph sizes read"
    allow["http/blocking.rs|tokio::time::timeout(deadline, tokio::task::spawn_blocking(work))"] = "read helper"
    allow["http/classification.rs|super::blocking::within("] = "classification read"
    allow["http/draft_check.rs|let task = tokio::task::spawn_blocking(move || {"] = "draft check"
    allow["http/graph_store.rs|tokio::task::spawn_blocking(move || store.graph_exists(&read, &target)),"] = "graph existence read"
    allow["http/handlers.rs|tokio::task::spawn_blocking(move || store.autocomplete(&scope, &typed, limit, infer))"] = "autocomplete read"
    allow["http/rdf4j.rs|let count = tokio::task::spawn_blocking(move || {"] = "statement count read"
    allow["http/result_stream.rs|tokio::task::spawn_blocking(move || {"] = "query result stream"
    allow["http/result_stream.rs|let stopped = tokio::task::spawn_blocking(move || {"] = "stream cancellation test"
    allow["http/result_stream.rs||ms| tokio::task::spawn_blocking(move || std::thread::sleep(Duration::from_millis(ms)));"] = "stream timeout test"
    allow["http/result_stream.rs|tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(1)))"] = "stream timing test"
    allow["http/shacl.rs|let validation = super::blocking::within("] = "SHACL validation read"
    allow["replication.rs|let batch = tokio::task::spawn_blocking(move || store.replication_log(query.after, max))"] = "replication log read"
}
{
    key = $0
    sub("^crates/nrese-server/src/", "", key)
    sub(/:[0-9]+:[[:space:]]*/, "|", key)
    if (!(key in allow) || ++seen[key] > (key in count ? count[key] : 1)) {
        print "unlisted blocking call: " $0
        failed = 1
    }
}
END { exit failed }
'

sites '(\.|::)[[:space:]]*(r#)?(commit|rematerialise(_until)?)\b' -- crates/nrese-store/src/mutation |
awk '
{
    if ($0 ~ /^crates\/nrese-store\/src\/mutation\/pipeline\.rs:/) {
        # v1.6 rematerialise preflight and quarantine revalidation.
        if ($0 ~ /match self\.store\.rematerialise_until\(rules, &stop\) \{$/ && ++preflight == 1) next
        if ($0 ~ /if quarantined && let Err\(error\) = self\.store\.rematerialise\(&rules\) \{$/ && ++revalidation == 1) next
        if ($0 ~ /\(self\.store\.lists\)\.commit\(before, summary\.revision, lists_left_out\);$/ && ++lists == 1) next
    }
    line = $0
    sub(/(^|[^[:alnum:]_#])publishing\(ticket, report, tx, \|tx\| (tx\.commit\(\)|ql\.commit\(tx\))\)/, "", line)
    if (line ~ /(\.|::)[[:space:]]*(r#)?(commit|rematerialise(_until)?)([^[:alnum:]_]|$)/) {
        print "unwrapped transaction publication: " $0
        failed = 1
    }
}
END { exit failed }
'
