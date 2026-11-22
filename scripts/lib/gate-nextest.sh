# shellcheck shell=bash
# The gates' pinned cargo-nextest, for scripts/lib/gate-job.sh (Linux) and scripts/lib/windows-job.sh:
#
#   gate_nextest CACHE TOOLS PLATFORM
#       prints the directory of nexte.st's PLATFORM (linux or windows) build of the cargo-nextest
#       version TOOLS (scripts/lib/gate-tools.txt) pins, taken only by its archive's sha256 and run
#       only as the executable it holds, by its sha256 (TOOLS' cargo-nextest-PLATFORM line), once its
#       version command succeeds and names that version; else fails, saying why. Each version and
#       archive has an entry of its own, CACHE/tools/nextest-<version>-<archive sha256, 12>, made where
#       it doesn't hold the pinned executable (suite_python_entry: aside, under the cache's lock, moved
#       into place whole, once it answers). An entry that holds it is never made again: another job
#       may be running it; if it doesn't answer, this job gives no verdict.

declare -F suite_python_entry > /dev/null || . "$(dirname "${BASH_SOURCE[0]}")/suite-python.sh"

gate_nextest() {
  local want sum exe_sum entry
  want=$(sed -n 's/^cargo-nextest //p' "$2" | tr -d '\r')
  [ -n "$want" ] || { echo "$2 pins no cargo-nextest" >&2; return 1; }
  read -r sum exe_sum < <(awk -v p="cargo-nextest-$3" -v v="$want" '$1 == p && $2 == v { print $3, $4 }' "$2" \
    | tr -d '\r')
  [[ ${sum:-} =~ ^[0-9a-f]{64}$ && ${exe_sum:-} =~ ^[0-9a-f]{64}$ ]] \
    || { echo "$2 pins no sha256 of cargo-nextest $want's $3 archive and executable (cargo-nextest-$3)" >&2; return 1; }
  entry=$(suite_python_entry "$1/tools/nextest-$want-${sum:0:12}" gate_nextest_has gate_nextest_unpack \
    "$want" "$sum" "$exe_sum" "$3") || return 1
  gate_nextest_runs "$entry" "$want" "$3" \
    || { echo "$entry: its cargo-nextest doesn't answer as $want: no verdict" >&2; return 1; }
  echo "$entry"
}

# DIR PLATFORM: the cargo-nextest cargo runs from DIR on PLATFORM: cargo-nextest.exe on Windows (a
# plain cargo-nextest where there is none: the suite's stand-ins), cargo-nextest elsewhere.
gate_nextest_exe() {
  if [ "$2" = windows ] && [ -e "$1/cargo-nextest.exe" ]; then
    echo "$1/cargo-nextest.exe"
  else
    echo "$1/cargo-nextest"
  fi
}

# DIR VERSION SHA256 EXE-SHA256 PLATFORM: DIR, if its cargo-nextest is the pinned executable (EXE-SHA256).
gate_nextest_has() {
  [ "$(sha256sum -b < "$(gate_nextest_exe "$1" "$5")" 2> /dev/null | cut -c1-64)" = "$4" ] && echo "$1"
}

# DIR VERSION PLATFORM: whether DIR's cargo-nextest's version command succeeds and names VERSION.
gate_nextest_runs() {
  local runner
  runner=$("$(gate_nextest_exe "$1" "$3")" nextest --version 2> /dev/null) || return 1
  [ "$(printf '%s\n' "$runner" | tr -d '\r' | awk 'NR == 1 { print $2 }')" = "$2" ]
}

# PART VERSION SHA256 EXE-SHA256 PLATFORM: nexte.st's PLATFORM build of cargo-nextest VERSION, unpacked
# at PART, taken only if its archive's sha256 is SHA256 (another archive is never unpacked), its
# executable's is EXE-SHA256 (checked before it runs) and it answers as VERSION. curl's deadline bounds
# it. tar reads the archive on its standard input: GNU tar takes `C:` in an archive's name for a host.
gate_nextest_unpack() {
  local got url=$5
  [ "$5" != windows ] || url=windows-tar
  curl -fsSL --max-time 300 -o "$1/nextest.tar.gz" "https://get.nexte.st/$2/$url" \
    || { echo "get.nexte.st's cargo-nextest $2 didn't download" >&2; return 1; }
  got=$(sha256sum -b < "$1/nextest.tar.gz" | cut -c1-64)
  [ "$got" = "$3" ] || { echo "cargo-nextest $2's archive has sha256 $got, not the pinned $3: not installed" >&2; return 1; }
  tar -xzf - -C "$1" < "$1/nextest.tar.gz" && rm -f -- "$1/nextest.tar.gz" || return 1
  gate_nextest_has "$@" > /dev/null \
    || { echo "cargo-nextest $2's archive holds no executable of the pinned sha256 $4: not installed" >&2; return 1; }
  gate_nextest_runs "$1" "$2" "$5" || { echo "cargo-nextest $2 from its pinned archive doesn't answer: not installed" >&2; return 1; }
}
