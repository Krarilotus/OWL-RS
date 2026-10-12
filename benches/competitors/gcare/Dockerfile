# G-CARE (SIGMOD 2020; github.com/yspark-dblab/gcare, no licence stated: internal use) for
# the planner's estimates (benches/sparql/gcare): its estimators gcare_graph and
# gcare_relation, built at the commit of its published experiments with its own CMake build.
FROM ubuntu:24.04 AS build
ARG GCARE_COMMIT=d603d2b
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends \
      git ca-certificates cmake make g++ libboost-all-dev && rm -rf /var/lib/apt/lists/*
RUN git clone -q https://github.com/yspark-dblab/gcare /gcare && git -C /gcare checkout -q "$GCARE_COMMIT"
RUN mkdir -p /gcare/build && cd /gcare/build && cmake .. >/dev/null && make -j8

FROM ubuntu:24.04
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends libboost-all-dev && rm -rf /var/lib/apt/lists/*
COPY --from=build /gcare/build/gcare_graph /gcare/build/gcare_relation /usr/local/bin/
ARG GCARE_COMMIT=d603d2b
LABEL org.opencontainers.image.revision="$GCARE_COMMIT"
