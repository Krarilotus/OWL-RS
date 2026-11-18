#!/usr/bin/env bash
# Checks that the runner builds with the dependency versions of the checkout it is copied
# into: every package in this crate's Cargo.lock that ROOT/Cargo.lock also has must be there
# at the same version. Otherwise a side whose root lock moved a dependency would be counted
# with the runner's version, not the one its build ships. Run it on each side before the
# build; a refusal prints each package, its version here and ROOT's, and exits 1.
#
#   bash check-lock.sh ROOT
set -euo pipefail
root=${1:?usage: check-lock.sh ROOT}
here=$(cd "$(dirname "$0")" && pwd)
# "name version" for each package of a lock, each once.
versions() {
  awk '{ gsub(/[\r"]/, "") }
    /^\[\[package\]\]$/ { name = "" }
    /^name = / { name = $3 }
    /^version = / && name != "" { print name, $3 }' "$1" | sort -u
}
[ -f "$root/Cargo.lock" ] || { echo "check-lock.sh: no $root/Cargo.lock" >&2; exit 1; }
awk 'NR == FNR { at[$1] = at[$1] " " $2; has[$1 " " $2] = 1; next }
  ($1 in at) && !(($1 " " $2) in has) {
    print "check-lock.sh: " $1 " " $2 " here, ROOT has" at[$1] > "/dev/stderr"; refused = 1 }
  END { exit refused }' <(versions "$root/Cargo.lock") <(versions "$here/Cargo.lock")
