# syntax=docker/dockerfile:1
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY engine ./engine
RUN cargo build --release --locked -p powerdb

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --home /data powerdb && mkdir /data && chown powerdb /data
COPY --from=build /src/target/release/powerdb /usr/local/bin/powerdb
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
ENV POWERDB_DATA=/data POWERDB_ADDR=0.0.0.0:8090
VOLUME /data
EXPOSE 8090
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s CMD ["powerdb", "status", "--addr", "127.0.0.1:8090"]
# starts as root only to fix the ownership of a mounted data folder, then runs the server as the unprivileged user
ENTRYPOINT ["docker-entrypoint.sh", "powerdb"]
CMD ["serve"]
