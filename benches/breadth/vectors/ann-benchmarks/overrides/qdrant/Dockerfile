# Upstream's Qdrant image (ann_benchmarks/algorithms/qdrant/Dockerfile at 2e081ad), changed
# only where it no longer builds: on Qdrant's current base (Debian trixie) pip can't upgrade
# Debian's own pip, setuptools and wheel, so the requirements install beside them instead.
ARG QDRANT_VERSION=1.19.2
ARG QDRANT_CLIENT_VERSION=1.19.1
FROM qdrant/qdrant:v${QDRANT_VERSION}

ARG QDRANT_CLIENT_VERSION
WORKDIR /home/app
COPY requirements.txt .

RUN apt-get update \
 && apt-get install -y python3-pip build-essential python3-h5py \
 && python3 -m pip install --break-system-packages --ignore-installed -r requirements.txt \
 && python3 -m pip install --break-system-packages qdrant-client==${QDRANT_CLIENT_VERSION}

COPY run_algorithm.py .

RUN printf '#!/bin/bash\n\
cd /qdrant && ./qdrant &\n\
sleep 5\n\
python3 -u run_algorithm.py "$@"' > entrypoint.sh \
 && chmod u+x entrypoint.sh

ENTRYPOINT ["/home/app/entrypoint.sh"]
