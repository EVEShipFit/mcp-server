FROM node:lts-slim AS sde

ARG SDE_VERSION=latest
WORKDIR /sde
RUN npm pack @eveshipfit/sde@${SDE_VERSION} \
    && tar xzf eveshipfit-sde-*.tgz package/dist/sde.dat package/dist/names.dat \
    && mv package/dist/sde.dat package/dist/names.dat . \
    && rm -rf package eveshipfit-sde-*.tgz /root/.npm

FROM rust:1-slim-trixie AS build

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian13:nonroot

COPY --from=sde /sde /sde
COPY --from=build /src/target/release/mcp-server /mcp-server

ENV SDE_DIR=/sde
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s CMD ["/mcp-server", "healthcheck"]
ENTRYPOINT ["/mcp-server"]
