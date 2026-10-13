# VLog and GLog (karmaresearch, Apache-2.0), built from pinned commits. GLog is a fork of
# VLog and builds the same way; the build arguments choose which, and the image calls the
# binary /opt/vlog/vlog either way:
#   VLog 1.3.7:  --build-arg REPO=karmaresearch/vlog --build-arg COMMIT=068d5f0e6b5cddc524ca58818d4b01b327cc653d
#   GLog:        --build-arg REPO=karmaresearch/glog --build-arg COMMIT=69d48423e5452ebffbef8702e8a83036720097ed --build-arg BIN=glog
# Their CMake files clone Trident and KOGNAC from the tip of master at build time; here
# both are cloned at pinned commits first and handed to the build as prebuilt libraries,
# so an image is reproducible.
FROM ubuntu:22.04 AS build
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential cmake git ca-certificates zlib1g-dev libcurl4-openssl-dev libsparsehash-dev \
 && rm -rf /var/lib/apt/lists/*
ARG JOBS=4
ARG KOGNAC_COMMIT=ec961644647e2b545cfb859148cde3dff94d317e
ARG TRIDENT_COMMIT=621ed49c648c811d5f69617833539f419766d3be
ARG REPO=karmaresearch/vlog
ARG COMMIT=068d5f0e6b5cddc524ca58818d4b01b327cc653d
ARG BIN=vlog
RUN git clone https://github.com/karmaresearch/kognac.git /opt/kognac \
 && git -C /opt/kognac checkout -q ${KOGNAC_COMMIT} \
 && cd /opt/kognac && cmake -DCMAKE_BUILD_TYPE=Release . && make -j${JOBS}
RUN git clone https://github.com/karmaresearch/trident.git /opt/trident \
 && git -C /opt/trident checkout -q ${TRIDENT_COMMIT} \
 && cd /opt/trident && cmake -DCMAKE_BUILD_TYPE=Release -DSPARQL=1 \
      -DKOGNAC_LIB=/opt/kognac -DKOGNAC_INC=/opt/kognac/include . \
 && make -j${JOBS}
RUN git clone https://github.com/${REPO}.git /opt/engine \
 && git -C /opt/engine checkout -q ${COMMIT} \
 && mkdir /opt/engine/build && cd /opt/engine/build \
 && cmake -DCMAKE_BUILD_TYPE=Release \
      -DKOGNAC_LIB=/opt/kognac -DKOGNAC_INC=/opt/kognac/include \
      -DTRIDENT_LIB=/opt/trident -DTRIDENT_INC=/opt/trident/include .. \
 && make -j${JOBS} \
 && mkdir -p /out/lib \
 && cp ${BIN} /out/vlog \
 && ldd ${BIN} | awk '$3 ~ /^\/opt\// {print $1, $3}' | while read name path; do cp -L "$path" "/out/lib/$name"; done \
 && printf '%s %s\nkognac %s\ntrident %s\n' "${REPO}" "${COMMIT}" "${KOGNAC_COMMIT}" "${TRIDENT_COMMIT}" > /out/VERSION

FROM ubuntu:22.04
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends zlib1g libcurl4 libgomp1 time \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/ /opt/vlog/
ENV LD_LIBRARY_PATH=/opt/vlog/lib
RUN if ldd /opt/vlog/vlog | grep 'not found'; then exit 1; fi
ENTRYPOINT ["/opt/vlog/vlog"]
