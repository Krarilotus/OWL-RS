# The reference OWL 2 DL reasoners behind NRESE's own runner (docs/design/owl2-dl.md §11):
# HermiT, ELK and Openllet on OWL API 5.1.20, in one JVM per batch, and KoncludeCLI
# (v0.7.0-1138, static Linux build) as a process of the runner's.
#   docker build -t nrese-bench/dl-reference benches/reasoning/dl
FROM eclipse-temurin:21-jdk AS build
RUN apt-get update && apt-get install -y --no-install-recommends maven curl unzip && rm -rf /var/lib/apt/lists/*
RUN curl -sSL -o /tmp/konclude.zip https://github.com/konclude/Konclude/releases/download/v0.7.0-1138/Konclude-v0.7.0-1138-Linux-x64-GCC-Static-Qt5.12.10.zip \
 && echo "37216c18f45a80165aec1e76aeefd62d933d6a3f8f134c5aca0707c676fda76e  /tmp/konclude.zip" | sha256sum -c - \
 && unzip -q /tmp/konclude.zip -d /tmp/k && mv /tmp/k/Konclude-* /opt/konclude && rm -rf /tmp/konclude.zip /tmp/k
WORKDIR /src
COPY runner/pom.xml .
RUN mvn -q -B dependency:go-offline
COPY runner/src src
RUN mvn -q -B -o package -DskipTests

# Konclude's build needs libpcre3 and GLib, which newer Ubuntu bases dropped: Debian 12
# with the Temurin JRE copied in.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends libpcre3 libglib2.0-0 && rm -rf /var/lib/apt/lists/*
COPY --from=eclipse-temurin:21-jre /opt/java/openjdk /opt/java/openjdk
ENV PATH=/opt/java/openjdk/bin:$PATH
COPY --from=build /src/target/dl-runner.jar /opt/dl-runner.jar
COPY --from=build /opt/konclude /opt/konclude
ENTRYPOINT ["java", "-Xmx8g", "-jar", "/opt/dl-runner.jar"]
