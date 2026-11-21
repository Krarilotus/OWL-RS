# A fleet gate's passes, remembered by content (sourced by scripts/check.sh, scripts/lib/gate-job.sh
# and scripts/lib/windows-job.sh).
#
# A tree that passed the push tier (or the merge check's Windows part, `windows`) on the fleet
# never needs it again. The key is what the result depends on:
#
#   <tier>[ w3c-optional] <tree> <file list> rustc-<release>-<commit> flags=<rustflags> tools=<id> fleet
#
# - tier: push, quick or windows. A quick check's pass (scripts/check.sh quick) is its own tier's,
#   keyed as push's and found by its exact key only: it never stands for a push pass, and no pass
#   covers another's list for it (gate_covering is the push tier's);
# - tree: the pushed commit's tree, what was tested, byte for byte; for `windows`, the hash of the
#   checked commit's inputs (windows_inputs), so a pass carries to a later commit with the same
#   inputs;
# - file list: the hash of the files the tier was selected from (the pushed range's; none for
#   `windows`, which runs everything); a push pass keeps its list, and covers a push of the same
#   commit whose files it holds when its other files reach no crate (gate_covering);
# - the toolchain rustc +<the commit's channel> -vV reports (a host with
#   another install or an override doesn't match), and the flags rustc gets
#   (scripts/cargo-guarded.sh --print-rustflags: `-C target-cpu=native` means each machine's own
#   CPU, recorded with the pass, not keyed);
# - tools: the gate's other tools and prerequisites, pinned (scripts/lib/gate-tools.txt, the
#   suite's benches/suite/requirements.txt), which the fleet job installs;
# - fleet: a pass is only ever a fleet job's, verified (scripts/lib/fleet-gate.sh): no other line
#   in the file (a hand-written one, an older gate's) has this form.
# - for `windows`, `python=<id>` after tools: the Python runtime the checked commit declares for
#   the suite (gate_runtime), which its job's suite ran with, verified (scripts/lib/fleet-gate.sh):
#   a pass under another runtime, or one recorded before the tier declared one (its key has no
#   python=), is never carried (PR #76).
#
# A pass is a line `<key> | <the commit it ran on> <the job, its host and CPU>` in
# <git-common-dir>/nrese-gate-passed, written by scripts/check.sh only (a push pass recorded before
# 21 Nov 2026 names no commit; a quick pass always names it).

# rustc-<release>-<commit> flags=<rustflags> tools=<id>, for the toolchain, flags and tools a
# build of COMMIT uses, independently of the caller's checkout.
gate_identity() (
  local rev=${1:?gate_identity COMMIT} root channel info release commit flags tools
  set -o pipefail
  # Cargo takes CARGO_ENCODED_RUSTFLAGS over RUSTFLAGS, and the identity (and what a fleet gate is
  # sent) names RUSTFLAGS only: a gate under it would stand for flags it never ran with.
  # Refused, before any pass is looked up or run.
  if [ -n "${CARGO_ENCODED_RUSTFLAGS+set}" ]; then
    echo "the gate doesn't run under CARGO_ENCODED_RUSTFLAGS (it overrides RUSTFLAGS): unset it, set RUSTFLAGS" >&2
    return 1
  fi
  root=$(git rev-parse --show-toplevel) || return 1
  channel=$(git show "$rev:rust-toolchain.toml" | tr -d '\r' | sed -n 's/^channel *= *"\(.*\)"/\1/p') || return 1
  [ -n "$channel" ] || return 1
  # Explicit selection ignores checkout/directory overrides; a missing channel fails without
  # rustup installing it implicitly (the job owns toolchain installation and its deadline).
  info=$(RUSTUP_AUTO_INSTALL=0 rustc "+$channel" -vV 2> /dev/null | tr -d '\r') || return 1
  release=$(printf '%s\n' "$info" | sed -n 's/^release: //p')
  commit=$(printf '%s\n' "$info" | sed -n 's/^commit-hash: //p')
  [ -n "$release" ] && [ -n "$commit" ] || return 1
  flags=$( . "$root/scripts/lib/gate-publish.sh"; effective_rustflags "$rev") || return 1
  flags=$(printf '%s' "$flags" | tr -d '\r' | tr ' ' _)
  tools=$({ git show "$rev:scripts/lib/gate-tools.txt" && git show "$rev:benches/suite/requirements.txt"; } \
    | tr -d '\r' | git hash-object --stdin) || return 1
  printf 'rustc-%s-%s flags=%s tools=%s\n' "$release" "${commit:0:12}" "$flags" "${tools:0:12}"
)

# The CPU `native` resolves to here (recorded with a pass).
gate_cpu() {
  rustc --print target-cpus 2> /dev/null | tr -d '\r' | sed -n 's/.*currently \([^)]*\)).*/\1/p' | head -1
}

# gate_key TIER TREE LIST_HASH IDENTITY
gate_key() {
  printf '%s%s %s %s %s fleet\n' "$1" "${NRESE_W3C_OPTIONAL:+ w3c-optional}" "$2" "$3" "$4"
}

# windows_inputs COMMIT: the hash of what the Windows part may read at COMMIT: every tracked file
# but the documentation (docs/, the top level's *.md) no other file names. Any file but those can
# be read (a crate's tests read the Dockerfile and ops/), and a doc that code,
# a test or a comment names by its path may be (docs/ops/config-reference.md, compiled into
# nrese-server), so it stays an input.
windows_inputs() {
  local files docs named
  files=$(git ls-tree -r --full-tree "$1") || return 1
  docs=$(printf '%s\n' "$files" | awk -F '\t' '$2 ~ /^docs\// || $2 ~ /^[^\/]*\.md$/ { print $2 }')
  if [ -n "$docs" ]; then
    # Bare matches whatever the user's settings (line numbers, columns, colour, files taken for
    # binary, which give "Binary file ... matches").
    named=$(printf '%s\n' "$docs" | git grep -a --no-line-number --no-column --no-color -h -o -F -f - "$1" -- . \
      ':(exclude)docs' ':(exclude,glob)*.md')
    [ $? -le 1 ] || return 1
  fi
  printf '%s\n' "$files" | awk -F '\t' -v named="${named:-}" '
    BEGIN { n = split(named, list, "\n"); for (i = 1; i <= n; i++) kept[list[i]] = 1 }
    ($2 ~ /^docs\// || $2 ~ /^[^\/]*\.md$/) && !($2 in kept) { next }
    { print }' | git hash-object --stdin
}

# gate_runtime COMMIT: the Python runtime COMMIT declares for the Windows tier's suite, as the
# suite's environment reports it (scripts/lib/suite-python.sh suite_python_runtime): `python
# <sys.version>` of scripts/lib/gate-tools.txt's python-windows, then the pins of
# benches/suite/requirements.txt, name==version (the names normalised, sorted). Fails where COMMIT
# declares either none.
gate_runtime() {
  local build pins
  build=$(git show "$1:scripts/lib/gate-tools.txt" 2> /dev/null | tr -d '\r' | sed -n 's/^python-windows [^ ]* [^ ]* //p')
  pins=$(git show "$1:benches/suite/requirements.txt" 2> /dev/null | tr -d '\r' \
    | awk '/^[A-Za-z0-9]/ { n = $1; sub(/==.*/, "", n); v = substr($1, length(n) + 1); n = tolower(n)
                            gsub(/[-_.]+/, "-", n); print n v }' | LC_ALL=C sort)
  [ -n "$build" ] && [ -n "$pins" ] || return 1
  printf 'python %s\n%s\n' "$build" "$pins"
}

# gate_python RUNTIME: the key's name of RUNTIME (gate_runtime), `python=<id>`.
gate_python() { printf 'python=%s\n' "$(printf '%s\n' "$1" | git hash-object --stdin | cut -c1-12)"; }

# landing_ref REF: whether a push to REF lands its commit: on the line, refactor/engine-v2, or on a
# release branch cut from it at a freeze, release/<name>. Such a push takes the merge check's
# Windows part besides its push tier (.githooks/pre-push), and both tiers' jobs are merge gates on
# the fleet (scripts/lib/fleet-gate.sh).
landing_ref() {
  case $1 in
    refs/heads/refactor/engine-v2 | refs/heads/release/?*) return 0 ;;
    *) return 1 ;;
  esac
}

gate_passed_file() { printf '%s/nrese-gate-passed\n' "$(git rev-parse --git-common-dir)"; }

# gate_pass KEY: the record of a pass for KEY (where it ran), if one is recorded.
gate_pass() {
  [ -z "${NRESE_GATE_RERUN:-}" ] && awk -v k="$1 |" \
    'index($0, k) == 1 { print substr($0, length(k) + 2); found = 1; exit } END { exit !found }' \
    "$(gate_passed_file)" 2> /dev/null
}

# gate_list_keep LIST LIST_HASH: LIST, the file list a push pass is recorded for, kept in the
# repository's objects, if it hashes to LIST_HASH (the key's file list), so gate_covering can read it
# back. Git prunes it with other unreachable objects in time; a pass whose list is gone counts for
# its exact list only.
gate_list_keep() { [ "$(git hash-object -w "$1")" = "$2" ]; }

# gate_covering TIER TREE LIST IDENTITY COMMIT ACCEPTS: the record of a pass of COMMIT (its key's
# tier, tree, identity and W3C mark as gate_key's; its record naming COMMIT first) whose kept file
# list holds every file of LIST, and whose files beyond LIST the command ACCEPTS takes, if one is
# recorded. The caller's ACCEPTS answers whether those files leave the checks of LIST unchanged
# and only add steps of their own (scripts/check.sh reaches_no_crate). The same commit: the same
# checkout and the same random seeds. Two approved branches then land one after the other on one
# gate run when the first changed no crate: the second's pass, gated on top of the first, covers
# the narrower range left once the first landed.
gate_covering() {
  local tier=$1 tree=$2 list=$3 identity=$4 commit=$5 accepts=$6
  local prefix="$tier${NRESE_W3C_OPTIONAL:+ w3c-optional} $tree " suffix=" $identity fleet"
  local passed line key where hash held beyond
  local -a extra
  passed=$(gate_passed_file)
  [ -z "${NRESE_GATE_RERUN:-}" ] && [ -f "$passed" ] || return 1
  while IFS= read -r line; do
    # A record, as gate_record writes it: `<key> | <where>`, WHERE naming the commit first.
    case $line in *" | "?*) ;; *) continue ;; esac
    key=${line%% | *}
    where=${line#* | }
    case $key in "$prefix"*"$suffix") ;; *) continue ;; esac
    case $where in "$commit "?*) ;; *) continue ;; esac
    hash=${key#"$prefix"}
    hash=${hash%"$suffix"}
    # An object id of the repository's width (the tree's).
    [ ${#hash} -eq ${#tree} ] && [[ $hash =~ ^[0-9a-f]+$ ]] || continue
    held=$(git cat-file blob "$hash" 2> /dev/null) || continue
    # The kept list's files beyond LIST; none at all if LIST has a file the kept list lacks.
    beyond=$(printf '%s\n' "$held" | awk '!pushed { if ($0 != "") held[$0] = 1; next }
      !($0 in held) { missing = 1; exit } { delete held[$0] }
      END { if (missing) exit 1; for (f in held) print f }' - pushed=1 "$list") || continue
    extra=()
    [ -z "$beyond" ] || mapfile -t extra <<< "$beyond"
    "$accepts" "${extra[@]}" || continue
    printf '%s\n' "$where"
    return 0
  done < "$passed"
  return 1
}

# gate_record KEY WHERE: KEY passed (WHERE: the commit it ran on, then the fleet job, its host and
# CPU).
gate_record() {
  printf '%s | %s\n' "$1" "$2" >> "$(gate_passed_file)"
}
