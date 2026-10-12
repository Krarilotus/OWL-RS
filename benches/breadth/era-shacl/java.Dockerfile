# An ERA-SHACL Java engine (Jena, TopBraid, RDF4J, Corese) built once from upstream's
# engines/<engine> directory (the build context), at the commit benches/breadth/era-shacl
# pins. Upstream's image runs `mvn clean install exec:java` at every start; this one
# compiles at build time and starts the same main class on the same JDK image. The heap is
# the run's choice (JAVA_OPTS), not upstream's fixed -Xmx32G, which doesn't fit 31 GiB.
FROM maven:3.9.6 AS build
WORKDIR /src
COPY . .
RUN mvn -q -B -DskipTests package dependency:copy-dependencies -DoutputDirectory=target/lib

FROM maven:3.9.6
COPY --from=build /src/target/classes /app/classes
COPY --from=build /src/target/lib /app/lib
RUN mkdir -p /data /shapes /reports
ENTRYPOINT ["sh", "-c", "exec java $JAVA_OPTS -cp '/app/classes:/app/lib/*' oeg.shacl.validator.Validator \"$@\"", "validator"]
