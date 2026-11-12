FROM rust:1-slim-trixie AS build

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
RUN cargo build --release --locked \
    && mkdir /cache

FROM gcr.io/distroless/cc-debian13:nonroot

COPY --from=build /src/target/release/mcp-server /mcp-server
COPY --from=build --chown=nonroot:nonroot /cache /cache

ENV CACHE_DIR=/cache
EXPOSE 8080
VOLUME /cache
HEALTHCHECK --interval=30s --timeout=5s --start-period=5m CMD ["/mcp-server", "healthcheck"]
ENTRYPOINT ["/mcp-server"]
