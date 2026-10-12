# NRESE in ann-benchmarks (benches/breadth/vectors). Upstream's base image is Ubuntu 22.04,
# whose glibc is older than the Debian bookworm NRESE is built on; this image is upstream's
# base on bookworm instead: the same Python minor version and requirements. The build
# context is the ann-benchmarks checkout, with nrese-server copied into this directory.
FROM python:3.10-bookworm
WORKDIR /home/app
COPY requirements.txt run_algorithm.py ./
RUN pip install --no-cache-dir -r requirements.txt
COPY ann_benchmarks/algorithms/nrese/nrese-server /usr/local/bin/nrese-server
RUN chmod +x /usr/local/bin/nrese-server
ENTRYPOINT ["python", "-u", "run_algorithm.py"]
