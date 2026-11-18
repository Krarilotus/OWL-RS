# MillenniumDB (GPL-2.0; github.com/MillenniumDB/MillenniumDB) for the comparison runs:
# `mdb import` builds a database, `mdb server` serves SPARQL at /sparql.
#
# Built from the commit whose results the Sparqloscope paper published (ISWC 2025, DBLP and
# Wikidata truthy: ecbf6dd, results/iswc-25/README.md there), with the project's own
# CMake release build, but on Ubuntu 24.04 with glibc as that evaluation ran natively, not on
# the Alpine (musl) base of the project's Dockerfile: musl's allocator serialises the
# allocations of a multithreaded C++ server, which would handicap it. MDB_COMMIT selects
# another commit (the WDBench runs use the newest).
FROM ubuntu:24.04 AS build
ARG MDB_COMMIT=ecbf6dde5a5864f088eee3b0836ad6adba1d623b
ARG JOBS=8
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends \
      git ca-certificates cmake make g++ libssl-dev libboost-all-dev libicu-dev libncurses-dev \
 && rm -rf /var/lib/apt/lists/*
RUN git clone -q https://github.com/MillenniumDB/MillenniumDB /mdb && git -C /mdb checkout -q "$MDB_COMMIT"
WORKDIR /mdb
RUN cmake -B build -D CMAKE_BUILD_TYPE=Release -D CMAKE_INSTALL_PREFIX=./ \
 && cmake --build build -j "$JOBS" --target install

FROM ubuntu:24.04
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends \
      libssl3t64 libicu74 libncursesw6 libboost-program-options1.83.0 libstdc++6 curl \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /mdb/build/bin /usr/local/bin
ARG MDB_COMMIT=ecbf6dde5a5864f088eee3b0836ad6adba1d623b
LABEL org.opencontainers.image.revision="$MDB_COMMIT"
WORKDIR /data
