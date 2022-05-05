FROM rust:1.58 as binary_build
WORKDIR /

RUN USER=root cargo new --bin bld
WORKDIR /bld

COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml

RUN cargo build --release
RUN rm src/*.rs

COPY ./src ./src
COPY migrations/ ./migrations
COPY icon.ico ./icon.ico
COPY index.html ./index.html

RUN rm target/release/deps/back*
RUN cargo build --release

RUN cp /bld/target/release/back /calories-bin
RUN chmod 0755 /calories-bin

FROM debian:bullseye-20211011-slim

COPY --from=binary_build /calories-bin .

EXPOSE 80

RUN mkdir storage

CMD until ./calories-bin; do echo "Try again"; done