#!/usr/bin/env bash
# Server spawn sites and store writes are classified. The HTTP exceptions move in v1.6.
set -euo pipefail
cd "$(dirname "$0")/.."

sites() {
    git grep --untracked --no-recurse-submodules -nE "$@" || test "$?" -eq 1
}

server=crates/nrese-server/src
effect=':(exclude)crates/nrese-server/src/http/effect.rs'
separator='[[:space:]]*::[[:space:]]*'
sites "\b(spawn_blocking|block_in_place)\b|\bblocking${separator}(within\b|\{)|\b(std${separator}thread|rayon|tokio(${separator}task)?)${separator}spawn\b|\bstd${separator}thread${separator}Builder\b" \
    -- "$server" "$effect" |
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
    allow["state.rs|let principal = tokio::task::spawn_blocking(login)"] = "v1.6 login"

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
    allow["replication.rs|std::thread::spawn(move || {"] = "replication image stream"
    allow["main.rs|std::thread::Builder::new()"] = "shutdown deadline"
    allow["main.rs|let _ = std::thread::Builder::new().spawn(move || {"] = "shutdown deadline log"
    allow["replication.rs|let task = tokio::spawn(async move {"] = "replication client"
    allow["replication.rs|tokio::spawn(async move {"] = "replication tests"
    count["replication.rs|tokio::spawn(async move {"] = 2
    allow["ai/service.rs|tokio::spawn(async move {"] = "suggestion tasks"
    count["ai/service.rs|tokio::spawn(async move {"] = 2
    allow["auth/oidc_introspection.rs|tokio::spawn(async move {"] = "OIDC tests"
    count["auth/oidc_introspection.rs|tokio::spawn(async move {"] = 2
    allow["http/blocking.rs|let handler = tokio::spawn(within("] = "read helper test"
    allow["http/result_stream.rs|let watchdog = tokio::spawn(async move {"] = "stream watchdog"
    allow["http/result_stream.rs|let busy = tokio::spawn(async {"] = "stream tests"
    count["http/result_stream.rs|let busy = tokio::spawn(async {"] = 2
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

methods='bulk_load(_with)?|rematerialise(_until)?|execute_update(_str)?|execute_graph_(write|delete)|execute_statements|apply_replication_log|patch_repository_settings|pin_query|unpin_query|clear_query_cache|clear_inferred|use_dl|use_reasoning_rules'
sites "(\.|\bStoreService${separator})[[:space:]]*(r#)?($methods)[[:space:]]*\(" -- "$server" "$effect" |
awk '
BEGIN {
    allow["main.rs|store.use_dl("] = "CLI store startup"
    allow["main.rs|.clear_inferred()"] = "CLI store startup"
    allow["main.rs|store.rematerialise(program)"] = "CLI rematerialisation"
    allow["main.rs|let report = store.bulk_load(&request).map_err(|error| match error {"] = "CLI import"
    allow["replication.rs|let applied = store.apply_replication_log(&frames, &expected, &stop);"] = "v1.6 replication apply"
    allow["http/api_v1.rs|tokio::task::spawn_blocking(move || store.rematerialise(program))"] = "v1.6 rematerialisation"
    allow["http/api_v1.rs|store.bulk_load(&request).map_err(|error| match error {"] = "synchronous import through effect::untracked"
    allow["http/api_v1.rs|let loaded = store.bulk_load_with(&request, job.progress());"] = "v1.6 async import"
    allow["http/api_v1.rs|.rematerialise(program)"] = "v1.6 async import reasoning"
    allow["http/api_v1.rs|tokio::task::spawn_blocking(move || changing.patch_repository_settings(merge))"] = "v1.6 settings patch"
    allow["http/api_v1.rs|changing.patch_repository_settings(|current| {"] = "v1.6 rules upload"
    allow["http/api_v1.rs|state.patch_repository_settings(|current| {"] = "v1.6 rules removal"
    allow["http/cache_api.rs|state.store().clear_query_cache();"] = "v1.6 cache clear"
    allow["http/cache_api.rs|let pinning = tokio::task::spawn_blocking(move || store.pin_query(&name, &request, &token));"] = "v1.6 cache pin"
    allow["http/cache_api.rs|match state.store().unpin_query(&name) {"] = "v1.6 cache unpin"
}
{
    key = $0
    sub("^crates/nrese-server/src/", "", key)
    sub(/:[0-9]+:[[:space:]]*/, "|", key)
    if (!(key in allow) || ++seen[key] > 1) {
        print "unlisted store write: " $0
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
