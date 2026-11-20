# Room server, the web UI it serves on the same port (--web-dir), and the upload and generation API
# (--data-dir): python, bubblewrap and the generator checkout. GEN_REPO_URL and GEN_REPO_REF are required build args (docs/container.md).
# The rust-toolchain.toml channel is `stable`, so the builder image tracks stable too.
# The web UI: built from the lockfile only, then compressed once here so the server never has to.
# Only /web/dist reaches the runtime image (no bun, no node_modules). The manifest and lockfile are
# copied first so the dependency layer is cached until they change.
FROM oven/bun:1.4.5-slim AS web
WORKDIR /web
COPY web/package.json web/bun.lock ./
RUN bun install --frozen-lockfile
COPY web/ ./
# The server answers /api through an operator proxy (docs/container.md), so the build calls it.
ENV CONSTELLATION_WEB_ADMIN_API=1
RUN bun --bun run build && bun scripts/precompress.mjs

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# Empty on a release build; any other build shows `<version>+<short sha>` at GET /version.
ARG BUILD_SHA=""
ENV CONSTELLATION_BUILD_SHA=$BUILD_SHA
# Cache mounts keep the registry and target between builds; the binary is copied out of the mount.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p server --bin constellation-server \
    && cp target/release/constellation-server /constellation-server

# The generator checkout and a venv with what the sidecar and generator need. The venv is built on
# the runtime's own distro so its python matches.
FROM debian:bookworm-slim AS gen
ARG GEN_REPO_URL
ARG GEN_REPO_REF
WORKDIR /opt/gen/ap
RUN if [ -z "$GEN_REPO_URL" ] || [ -z "$GEN_REPO_REF" ]; then \
      echo "build args GEN_REPO_URL and GEN_REPO_REF are required (see docs/container.md)" >&2; exit 1; fi \
    && apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git python3 python3-venv python3-dev build-essential \
    && rm -rf /var/lib/apt/lists/* \
    && git init -q . \
    && git fetch -q --depth 1 "$GEN_REPO_URL" "$GEN_REPO_REF" && git checkout -q FETCH_HEAD \
    && mkdir -p custom_worlds && rm -rf .git \
    && grep -viE '^(kivy|pyshortcuts|#|[[:space:]]*$)' requirements.txt > /tmp/requirements.txt \
    && python3 -m venv /opt/gen/venv \
    && /opt/gen/venv/bin/pip install --no-cache-dir -r /tmp/requirements.txt

# sqlx uses rustls with the system root certificates, so ca-certificates is the only runtime dependency.
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates python3 bubblewrap \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home constellation \
    && mkdir -p /data/rooms && chown -R constellation /data
COPY --from=build /constellation-server /usr/local/bin/constellation-server
COPY --from=gen /opt/gen /opt/gen
COPY --from=web /web/dist /opt/constellation/web
COPY tools/ap-gen /opt/constellation/tools/ap-gen
COPY tools/ap-import /opt/constellation/tools/ap-import
ENV CONSTELLATION_AP_DIR=/opt/gen/ap \
    CONSTELLATION_AP_PYTHON=/opt/gen/venv/bin/python \
    CONSTELLATION_AP_GEN_SCRIPT=/opt/constellation/tools/ap-gen/ap_gen.py
COPY --chmod=755 docker/entrypoint.sh /usr/local/bin/entrypoint.sh
USER 10001
EXPOSE 38281
# No curl in slim: bash opens the socket itself and checks for a 200.
HEALTHCHECK --interval=15s --timeout=3s --start-period=20s --retries=3 \
    CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/38281 && printf 'GET /healthz HTTP/1.0\\r\\n\\r\\n' >&3 && head -n1 <&3 | grep -q ' 200 '"]
# The wrapper forwards signals and maps exit 3 (fenced) to 0 so the container is not restarted.
ENTRYPOINT ["entrypoint.sh"]
CMD ["--host", "0.0.0.0", "--port", "38281", "--rooms", "/data/rooms", "--web-dir", "/opt/constellation/web"]
