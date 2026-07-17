FROM rust:1.88-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --release -p officecli-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates chromium fonts-noto-cjk && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/officecli-server /usr/local/bin/officecli-server
EXPOSE 26315
ENV OFFICECLI_BIND=0.0.0.0:26315
ENTRYPOINT ["/usr/local/bin/officecli-server"]
