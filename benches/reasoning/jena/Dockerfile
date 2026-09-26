# Jena rule-reasoner runner for the reasoning benchmark: Materialise.java compiled against
# the pinned Jena of benches/competitors/jena (build that image first).
FROM nrese-bench/jena:6.2.0 AS jena
FROM eclipse-temurin:21-jdk AS build
COPY --from=jena /opt/apache-jena-6.2.0 /opt/jena
COPY Materialise.java /src/Materialise.java
RUN javac -cp '/opt/jena/lib/*' -d /runner /src/Materialise.java

FROM eclipse-temurin:21-jre
COPY --from=jena /opt/apache-jena-6.2.0 /opt/jena
COPY --from=build /runner /runner
ENTRYPOINT ["java", "-cp", "/runner:/opt/jena/lib/*", "Materialise"]
