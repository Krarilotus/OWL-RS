#!/usr/bin/env bash
# Runs builds for this working tree on the build host instead of this machine.
#
# The host is an ssh alias: NRESE_BUILD_HOST, else `git config nrese.buildHost`. The working
# tree, uncommitted and untracked files included, goes to the host as a snapshot commit, into
# a repository of its own there (~/owlrs/repo.git). It is checked out in one worktree per
# local worktree (~/owlrs/wt/<name>), so the host's target directory, and with it incremental
# builds, carries over from one run to the next.
#
#   scripts/lib/remote.sh check MODE [FILES]   the gate (scripts/check.sh MODE); FILES lists
#                                              the changed files the local side selected
#   scripts/lib/remote.sh run CMD...           any command, e.g. cargo test -p nrese-dl
#   scripts/lib/remote.sh tree                 prints the working tree's id (what a run sends)
#
# Exit status: the remote command's.
set -u
cd "$(git rev-parse --show-toplevel)"
name=$(basename "$PWD")

index=$(mktemp)
trap 'rm -f "$index"' EXIT
GIT_INDEX_FILE=$index git read-tree HEAD
GIT_INDEX_FILE=$index git -c core.safecrlf=false add -A
tree=$(GIT_INDEX_FILE=$index git write-tree)
[ "${1:-}" = tree ] && { echo "$tree"; exit 0; }

host=${NRESE_BUILD_HOST:-$(git config --get nrese.buildHost)}
[ -n "$host" ] || { echo "no build host (NRESE_BUILD_HOST or git config nrese.buildHost)" >&2; exit 2; }
root=${NRESE_BUILD_ROOT:-owlrs}
snapshot=$(git commit-tree "$tree" -p HEAD -m "snapshot of $name")

# While the host's owners use it (its guard wrote ~/owlrs/RETREAT, tools/office-guard), a build
# runs capped: 8 threads, 6 GB, the lowest CPU and I/O priority, in a scope the guard spares.
# An unreachable host falls back to nrese.buildHostFallback.
capped=""
if ! ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" true 2>/dev/null; then
  fallback=$(git config --get nrese.buildHostFallback)
  [ -n "$fallback" ] || { echo "$host unreachable and no fallback host" >&2; exit 75; }
  echo "$host unreachable; building on $fallback" >&2
  host=$fallback
elif ssh -o BatchMode=yes "$host" "test -f $root/RETREAT"; then
  echo "$host is in use by its owners; building capped" >&2
  capped="systemd-run --user --scope -q --unit=owlrs-capped-$name-$$ -p MemoryMax=6G -p CPUQuota=800% nice -n 19 ionice -c3 env CARGO_BUILD_JOBS=8"
fi
ssh -o BatchMode=yes "$host" "[ -d $root/repo.git ] || git init -q --bare $root/repo.git" || exit 2
# Transport only: the hooks of this repository would start the gate again on this push.
git -c core.hooksPath=/dev/null push -q -f "$host:$root/repo.git" "$snapshot:refs/heads/snapshot/$name" || exit 2

prepare="export PATH=\$HOME/.cargo/bin:\$PATH NRESE_ON_BUILD_HOST=1 && cd $root &&
  { [ -d wt/$name ] || git -C repo.git worktree add -q --detach ../wt/$name $snapshot; } &&
  cd wt/$name && git checkout -q -f --detach $snapshot && git clean -q -fd"
case ${1:-} in
  check)
    # A local opt-out of the W3C suites' data (scripts/check.sh) holds on the build host too.
    ssh -o BatchMode=yes "$host" "$prepare && ${NRESE_W3C_OPTIONAL:+NRESE_W3C_OPTIONAL=1 }NRESE_FILES_FROM=/dev/stdin $capped bash scripts/check.sh $2" \
      < "${3:-/dev/null}"
    ;;
  run)
    shift
    ssh -o BatchMode=yes "$host" "$prepare && $capped bash -c $(printf %q "$*")"
    ;;
  *)
    echo "usage: scripts/lib/remote.sh check MODE [FILES] | run CMD... | tree" >&2
    exit 2
    ;;
esac
