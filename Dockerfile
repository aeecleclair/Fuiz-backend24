FROM rust:1.76-bookworm as build

COPY . .
RUN cargo build --release --all-features

FROM debian:stable as base
COPY --from=build target/release/fuiz /
ENTRYPOINT ["/fuiz"]