# Disk checks shared by the build guard and the benchmark scripts. Source it.
#
# A script that writes a lot (builds, datasets, container volumes) calls
# `require_free_gb <n> <what>` before it starts, so it stops with a clear message
# instead of filling the disk.

# Free space on the file system that holds `$1`, in whole GB.
free_gb() {
  df -Pk "$1" | awk 'NR==2 { printf "%d", $4 / 1048576 }'
}

# Size of the directory `$1` in whole GB (0 if it doesn't exist).
dir_gb() {
  if [ -d "$1" ]; then
    du -sk "$1" 2>/dev/null | awk '{ printf "%d", $1 / 1048576 }'
  else
    printf 0
  fi
}

# require_free_gb <GB> <what is about to run> [directory]: exits 1 if less is free.
require_free_gb() {
  local need=$1 what=$2 where=${3:-.} free
  free=$(free_gb "$where")
  if [ "$free" -lt "$need" ]; then
    printf 'not started: %s needs %s GB free, %s GB are (%s)\n' "$what" "$need" "$free" "$where" >&2
    exit 1
  fi
}
