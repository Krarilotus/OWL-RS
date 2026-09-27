# OWL2Bench (ISWC 2020) ABox generator with its four profile TBoxes (EL, QL, RL, DL).
# Pinned to a commit so datasets are reproducible; seed 1 is the seed of the paper's datasets.
#
#   docker run -v <dir>:/out nrese-bench/owl2bench <universities> <EL|QL|RL|DL> [seed]
#
# Writes /out/OWL2<profile>-<universities>.owl: TBox plus ABox, RDF/XML.
FROM alpine/git AS source
ARG OWL2BENCH_COMMIT=5a27536c8e7f7621b34c61c8a57ed9eb63e7829a
RUN git clone --quiet https://github.com/kracr/owl2bench.git /src \
 && cd /src && git checkout --quiet "$OWL2BENCH_COMMIT" && git rev-parse HEAD > /src/commit

FROM eclipse-temurin:21-jre
# The jar needs the TBoxes, RandomNames.xlsx and config.properties in its working directory;
# without config.properties it silently generates an empty ABox.
COPY --from=source /src/OWL2Bench.jar /src/RandomNames.xlsx /src/config.properties /src/commit /work/
# The TBoxes come from OWL2Bench/, not the repository root: commit 1539dad (2022) moved the
# generator and those TBoxes to https://kracr.iiitd.edu.in/OWL2Bench#, but the root copies
# still use http://benchmark/OWL2Bench#, so with them no TBox axiom touches the ABox.
COPY --from=source /src/OWL2Bench/UNIV-BENCH-OWL2EL.owl /src/OWL2Bench/UNIV-BENCH-OWL2QL.owl /src/OWL2Bench/UNIV-BENCH-OWL2RL.owl /src/OWL2Bench/UNIV-BENCH-OWL2DL.owl /work/
WORKDIR /work
ENTRYPOINT ["sh", "-c", "java -jar /work/OWL2Bench.jar \"$@\" && mv /work/OWL2*.owl /out/", "owl2bench"]
