# Room server only: no web UI, no admin/generation API (that needs python and bwrap).
# The rust-toolchain.toml channel is `stable`, so the builder image tracks stable too.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# Cache mounts keep the registry and target between builds; the binary is copied out of the mount.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p server --bin constellation-server \
    && cp target/release/constellation-server /constellation-server

# sqlx uses rustls with the system root certificates, so ca-certificates is the only runtime dependency.
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home constellation \
    && mkdir -p /data/rooms && chown -R constellation /data
COPY --from=build /constellation-server /usr/local/bin/constellation-server
USER 10001
EXPOSE 38281
# No curl in slim: bash opens the socket itself and checks for a 200.
HEALTHCHECK --interval=15s --timeout=3s --start-period=20s --retries=3 \
    CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/38281 && printf 'GET /healthz HTTP/1.0\\r\\n\\r\\n' >&3 && head -n1 <&3 | grep -q ' 200 '"]
ENTRYPOINT ["constellation-server"]
CMD ["--host", "0.0.0.0", "--port", "38281", "--rooms", "/data/rooms"]
