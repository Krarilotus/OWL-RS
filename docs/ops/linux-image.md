# Checked Linux image

The `Linux image` workflow builds the existing root Dockerfile for **Linux amd64**,
using the pinned Rust toolchain, locked dependencies, two Cargo jobs and the
`portable` CPU profile. It checks the built image before exporting or publishing it.
It does not run anything on the office benchmark machine.

The revision label records the exact checked-out source commit. The receipt records
the immutable Docker image ID, fixture and configuration hashes, effective server
budgets and checks that passed. Base image tags can change; builds are repeatable,
but byte-for-byte reproducibility is not claimed. Identify a built image by its ID
or registry digest, rather than trusting a mutable tag.

## Build and check locally

From a clean checkout of the source revision you want to deploy:

```bash
revision=$(git rev-parse HEAD)
docker build --platform linux/amd64 --build-arg CARGO_BUILD_JOBS=2 \
  --build-arg NRESE_TARGET_CPU=portable --build-arg NRESE_SOURCE_REVISION="$revision" \
  -t nrese-checked:"$revision" .
python3 scripts/smoke-image.py --image nrese-checked:"$revision" \
  --revision "$revision" --output artifacts/image-receipt.json
```

On Windows, use Python and pass `--docker-context desktop-linux` to the checker,
and `--context desktop-linux` before Docker's `build` subcommand. The checker uses
Docker volumes rather than host bind mounts, so it needs no Linux host paths.

The checker creates its own randomly named volume and containers, with a 2 GiB
hard memory ceiling, no swap, one CPU and one Rayon thread. The server runs as the
image's unprivileged user, with a read-only root filesystem and a random loopback
port. Tini forwards Docker stop signals to the server and reaps children; the
checker rejects forced SIGKILL and OOM shutdowns. Cleanup removes only resources
created by this invocation.

It validates the configuration, serially loads a four-quad Turtle fixture including
an RDF 1.2 triple term and PROV-O source, checks GET/POST ASK and a provenance SELECT,
checks effective budgets and disabled capabilities, rejects Update and Graph Store
writes, then stops and starts its own instance and checks that the data and revision
survive. A receipt is emitted only after all checks pass.

This is an image acceptance smoke test, not a full conformance certificate or a
production capacity measurement. A deployment still needs its own dataset,
licence/privacy checks, memory/concurrency measurements and network policy.

## Retrieve a CI image

Relevant pull requests trigger the build/check job without registry publication.
The uploaded `nrese-linux-amd64-<commit>-<attempt>` artifact contains the image,
receipt and checksums. Choose the successful run for the desired source commit:

```bash
gh run download RUN_ID --repo Krarilotus/OWL-RS --name ARTIFACT_NAME --dir artifacts/download
cd artifacts/download
sha256sum --check SHA256SUMS
gzip -dc nrese-linux-amd64.tar.gz | docker load
```

Compare the loaded image's `docker image inspect ... --format '{{.Id}}'` with
`image_id` in `image-receipt.json`. Keep the receipt with the deployment evidence.
CI image artifacts expire after seven days.

## Publish a checked registry image

Once the workflow is on the repository's default branch, an operator can dispatch
it against a chosen branch or tag. Publication is explicitly opted in:

```bash
gh workflow run linux-image.yml --repo Krarilotus/OWL-RS --ref SOURCE_REF -f publish=true
```

The publish job loads the exact archive that passed the checks, verifies its
checksums and image ID, and publishes `ghcr.io/krarilotus/nrese:sha-<full-commit>`.
Only that job has `packages: write`; pull requests cannot publish. The job summary
and registry artifact contain `ghcr.io/krarilotus/nrese@sha256:...`: use that digest
as a downstream `NRESE_IMAGE` build argument. No `latest` tag is published.
The registry package's access settings determine whether deployment hosts need
authentication; publication does not change those settings.

The image retains the established `/usr/local/bin/nrese-server` binary location,
Debian bookworm runtime and UID 10001, for downstream wrappers. The general
Dockerfile still defaults to `x86-64-v3`; use the portable profile above for this
checked artifact. Existing benchmark CPU choices remain available.
