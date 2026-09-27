# LUBM data generator (UBA), the maintained fork with N-Triples output and threads.
# Pinned to a commit so generated data is reproducible across machines.
# The fork targets Java 7, which JDK 17+ no longer compiles for.
FROM maven:3-eclipse-temurin-11 AS build
ARG UBA_COMMIT=48686cd616f564c8fc360dc5abbcc294678655c4
RUN git clone --quiet https://github.com/rvesse/lubm-uba.git /src \
 && cd /src && git checkout --quiet "$UBA_COMMIT" && git rev-parse HEAD > /commit \
 && mvn -q -DskipTests clean package

FROM eclipse-temurin:21-jre
COPY --from=build /src/target/lubm-uba.jar /uba/lubm-uba.jar
COPY --from=build /commit /uba/commit
WORKDIR /uba
ENTRYPOINT ["java", "-jar", "/uba/lubm-uba.jar"]
