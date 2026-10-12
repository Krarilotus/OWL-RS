# Soufflé (souffle-lang, UPL-1.0), the pinned release package, checked against the
# published SHA-512. Programs are compiled in the container (`souffle -o`), the mode the
# Soufflé authors and the FlowLog paper (VLDB 2026) benchmark; the interpreter is about
# 1.5x slower there.
FROM ubuntu:24.04
ARG SOUFFLE_VERSION=2.5
ARG SOUFFLE_DEB=x86_64-ubuntu-2404-souffle-2.5-Linux.deb
ARG SOUFFLE_SHA512=6b86e554f6aa5abf8a8b55d8312ae37c0957c5bd6c9edeea89246db9406f645ec5e600b84fe6636b1c163da556f0da6c3d2dad46c1083413f2fcf4f95b9ac62c
ADD https://github.com/souffle-lang/souffle/releases/download/${SOUFFLE_VERSION}/${SOUFFLE_DEB} /tmp/souffle.deb
RUN echo "${SOUFFLE_SHA512}  /tmp/souffle.deb" | sha512sum -c - \
 && apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends /tmp/souffle.deb time \
 && rm -rf /var/lib/apt/lists/* /tmp/souffle.deb \
 && souffle --version
ENTRYPOINT ["souffle"]
