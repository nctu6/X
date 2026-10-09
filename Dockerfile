FROM rust:1.99-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/xfeed /usr/local/bin/xfeed
COPY config.yml /etc/xfeed/config.yml
RUN mkdir -p /var/lib/xfeed && chown 65534:65534 /var/lib/xfeed
WORKDIR /var/lib/xfeed
USER 65534:65534
VOLUME ["/var/lib/xfeed"]
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/xfeed"]
CMD ["/etc/xfeed/config.yml"]
