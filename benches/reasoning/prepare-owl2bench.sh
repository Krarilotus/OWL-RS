#!/usr/bin/env bash
# OWL2Bench data for the reasoning benchmark, generated into the Docker volume `nrese-bench-data`.
#
#   benches/reasoning/prepare-owl2bench.sh [profile:universities...]   (default: RL:1 RL:10 EL:1 QL:1 DL:1)
#
# Produces /data/owl2bench-<profile>-<N>.nt: that profile's TBox plus the generated ABox,
# converted from the generator's RDF/XML to N-Triples. Seed 1 is the seed of the paper's datasets.
# One university is ≈ 50 k axioms; 200 are ≈ 14 M.
set -euo pipefail
export MSYS_NO_PATHCONV=1
native() { cygpath -m "$1" 2>/dev/null || echo "$1"; }
HERE=$(native "$(cd "$(dirname "$0")" && pwd)")
JENA_IMAGE=${JENA_IMAGE:-nrese-bench/jena:6.2.0}

docker build -q -t nrese-bench/owl2bench "$HERE/owl2bench" >/dev/null
for spec in ${*:-RL:1 RL:10 EL:1 QL:1 DL:1}; do
  profile=${spec%%:*} n=${spec#*:}
  name=owl2bench-$(echo "$profile" | tr '[:upper:]' '[:lower:]')-$n
  if docker run --rm -v nrese-bench-data:/data alpine test -s "/data/$name.nt"; then
    echo "$name: present"
    continue
  fi
  docker run --rm -v nrese-bench-data:/data --entrypoint sh nrese-bench/owl2bench -c \
    "mkdir -p /data/owl2bench-gen && cd /work && java -jar OWL2Bench.jar $n $profile 1 >/dev/null &&
     mv OWL2$profile-$n.owl /data/owl2bench-gen/"
  docker run --rm -v nrese-bench-data:/data --entrypoint sh "$JENA_IMAGE" -c \
    "/opt/apache-jena-6.2.0/bin/riot --quiet --output=nt /data/owl2bench-gen/OWL2$profile-$n.owl > /data/$name.nt &&
     rm /data/owl2bench-gen/OWL2$profile-$n.owl && echo $name: \$(wc -l < /data/$name.nt) triples"
done
docker run --rm -v nrese-bench-data:/data alpine sh -c 'rmdir /data/owl2bench-gen 2>/dev/null || true'
