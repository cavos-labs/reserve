# One static binary on a distroless base: nothing to patch, nothing to run as
# root, and small enough that the whole service fits on the cheapest VM.
FROM rust:1.93-slim AS build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p reserve-api && strip target/release/reserve-api

FROM gcr.io/distroless/cc-debian12
COPY --from=build /src/target/release/reserve-api /usr/local/bin/reserve-api
# Nothing to persist: every number the service needs is on the chain or in the
# signed quote it handed out.
ENV RESERVE_BIND=0.0.0.0:8080
EXPOSE 8080
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/reserve-api"]
