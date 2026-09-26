# Apache Jena command-line tools (TDB2 loaders) and Fuseki (the SPARQL server) for the
# comparison runs. `tdb2.xloader` needs jq; everything else comes with the JRE image.
FROM eclipse-temurin:21-jre
ARG JENA_VERSION=6.2.0
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends jq curl ca-certificates \
 && curl -sSL "https://dlcdn.apache.org/jena/binaries/apache-jena-${JENA_VERSION}.tar.gz" | tar xz -C /opt \
 && curl -sSL "https://dlcdn.apache.org/jena/binaries/apache-jena-fuseki-${JENA_VERSION}.tar.gz" | tar xz -C /opt \
 && ln -s "/opt/apache-jena-${JENA_VERSION}" /opt/jena \
 && ln -s "/opt/apache-jena-fuseki-${JENA_VERSION}" /opt/fuseki \
 && rm -rf /var/lib/apt/lists/*
ENV PATH="/opt/jena/bin:/opt/fuseki:${PATH}"
