# shellcheck shell=bash
# The gates' pinned cargo-nextest, for scripts/lib/gate-job.sh (Linux) and scripts/lib/windows-job.sh:
#
#   gate_nextest CACHE TOOLS PLATFORM
#       prints the directory of nexte.st's PLATFORM (linux or windows) build of the cargo-nextest
#       version TOOLS (scripts/lib/gate-tools.txt) pins, taken only by its archive's sha256 and run
#       only as the executable it holds, by its sha256 (TOOLS' cargo-nextest-PLATFORM line). Each
#       version and archive has an entry of its own, CACHE/tools/nextest-<version>-<archive sha256, 12>,
#       made where it isn't there as it must be (suite_python_entry: aside, under the cache's lock,
#       moved into place whole); else fails, saying why.

declare -F suite_python_entry > /dev/null || . "$(dirname "${BASH_SOURCE[0]}")/suite-python.sh"

gate_nextest() {
  local want sum exe_sum
  want=$(sed -n 's/^cargo-nextest //p' "$2" | tr -d '\r')
  [ -n "$want" ] || { echo "$2 pins no cargo-nextest" >&2; return 1; }
  read -r sum exe_sum < <(awk -v p="cargo-nextest-$3" -v v="$want" '$1 == p && $2 == v { print $3, $4 }' "$2" \
    | tr -d '\r')
  [[ ${sum:-} =~ ^[0-9a-f]{64}$ && ${exe_sum:-} =~ ^[0-9a-f]{64}$ ]] \
    || { echo "$2 pins no sha256 of cargo-nextest $want's $3 archive and executable (cargo-nextest-$3)" >&2; return 1; }
  suite_python_entry "$1/tools/nextest-$want-${sum:0:12}" gate_nextest_is gate_nextest_unpack \
    "$want" "$sum" "$exe_sum" "$3"
}

# DIR VERSION SHA256 EXE-SHA256 PLATFORM: DIR, if its cargo-nextest is the pinned executable (EXE-SHA256,
# checked before it runs: a cache entry made any other way is never run) and its version command
# succeeds and names VERSION.
gate_nextest_is() {
  local runner exe=$1/cargo-nextest.exe
  [ -e "$exe" ] || exe=$1/cargo-nextest
  [ "$(sha256sum -b < "$exe" 2> /dev/null | cut -c1-64)" = "$4" ] || return 1
  runner=$("$exe" nextest --version 2> /dev/null) || return 1
  [ "$(printf '%s\n' "$runner" | tr -d '\r' | awk 'NR == 1 { print $2 }')" = "$2" ] || return 1
  echo "$1"
}

# PART VERSION SHA256 EXE-SHA256 PLATFORM: nexte.st's PLATFORM build of cargo-nextest VERSION, unpacked
# at PART, taken only if its archive's sha256 is SHA256: another archive is never unpacked. curl's
# deadline bounds it. tar reads the archive on its standard input: GNU tar takes `C:` in an archive's
# name for a host.
gate_nextest_unpack() {
  local got url=$5
  [ "$5" != windows ] || url=windows-tar
  curl -fsSL --max-time 300 -o "$1/nextest.tar.gz" "https://get.nexte.st/$2/$url" \
    || { echo "get.nexte.st's cargo-nextest $2 didn't download" >&2; return 1; }
  got=$(sha256sum -b < "$1/nextest.tar.gz" | cut -c1-64)
  [ "$got" = "$3" ] || { echo "cargo-nextest $2's archive has sha256 $got, not the pinned $3: not installed" >&2; return 1; }
  tar -xzf - -C "$1" < "$1/nextest.tar.gz" && rm -f -- "$1/nextest.tar.gz"
}
