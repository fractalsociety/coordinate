FROM rust:1-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY templates ./templates
COPY tests ./tests
RUN cargo build --release

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl sqlite3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
COPY --from=builder /app/target/release/squad /usr/local/bin/squad

EXPOSE 8787

CMD ["sh", "-lc", "squad init && exec squad serve --bind 0.0.0.0:${COORDINATE_PORT:-8787}"]
