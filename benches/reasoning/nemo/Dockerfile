# Nemo (TU Dresden, Rust datalog engine), the release binary, running rule programs over RDF.
# Pinned release; the archive is checked against the published checksum.
FROM debian:bookworm-slim
ARG NEMO_VERSION=v0.10.1
ADD https://github.com/knowsys/nemo/releases/download/${NEMO_VERSION}/nemo_${NEMO_VERSION}_x86_64-unknown-linux-gnu.tar.gz /tmp/nemo.tar.gz
ADD https://github.com/knowsys/nemo/releases/download/${NEMO_VERSION}/nemo_${NEMO_VERSION}_x86_64-unknown-linux-gnu.tar.gz.sha256sum /tmp/nemo.sha256
RUN cd /tmp && echo "$(cut -d' ' -f1 nemo.sha256)  nemo.tar.gz" | sha256sum -c - \
 && tar -xzf nemo.tar.gz && find /tmp -name nmo -type f -exec install -m 755 {} /usr/local/bin/nmo \; \
 && rm -rf /tmp/*
COPY owl2rl.rls /nemo/owl2rl.rls
ENTRYPOINT ["nmo"]
