# NRESE as an ERA-SHACL engine: upstream's CLI contract (DATA SHAPES REPORT; prints "Load
# time:" and "Validation time:" in seconds) over the perf lab, which bulk-loads the data
# into memory and validates it in-process, as the other engines do. The build context is
# a directory holding perf_lab (benches/breadth/kit.py build_nrese) and nrese-era.sh.
FROM debian:bookworm-slim
COPY perf_lab nrese-era.sh /usr/local/bin/
RUN chmod +x /usr/local/bin/perf_lab /usr/local/bin/nrese-era.sh && mkdir -p /data /shapes /reports
ENTRYPOINT ["/usr/local/bin/nrese-era.sh"]
