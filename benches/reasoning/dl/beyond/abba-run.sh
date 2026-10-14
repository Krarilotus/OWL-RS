#!/usr/bin/env bash
# One run of an A-B-B-A over the fast suite's DL and QL cases (a queue job's chunk):
#
#   bash abba-run.sh CHECKOUT RESULTS REV N
#
# CHECKOUT: the revision's worktree (built by `fast.py build`); REV: base or new; N: the
# run's place (1 to 4). Writes RESULTS/beyondabba-REV-N.json and appends to runs.txt; exits
# 75 where the host's owners came back (the queue reruns the chunk).
set -u
D=$1
R=$2
rev=$3
n=$4
export NRESE_BENCH_REPORTS="$R/reports"
C=dl-transitive,dl-transitive-400,dl-number,dl-nominals,dl-datatypes,dl-roles,el-classify,el-classify-irregular,horn-classify,canon-twins,dl-pipeline,ctx-ore-9724,ctx-ore-2738,ctx-ore-9835,ctx-ore-7914,dl-abox-lubm1,dlmode-lubm1,dlmode-commits,dlmode-reject,dlmode-reject-dl,classify-store-horn,classify-store-tableau,ql-lubm10,ql-owl2bench
L=beyondabba-$rev-$n
(cd "$D" && python3 benches/fast/fast.py run --cases $C --reps 1 --label "$L" --out "$R/$L.json"
 code=$?
 cp "benches/baselines/fast/$L.json" "$R/$L.compact.json"
 exit $code) > "$R/run-$n-$rev.log" 2>&1
code=$?
echo "run $n $rev exit $code" >> "$R/runs.txt"
[ $code -ne 0 ] && [ -e "$HOME/owlrs/RETREAT" ] && exit 75
exit 0
