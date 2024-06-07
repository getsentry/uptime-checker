FROM rust:1.74-alpine3.18 as builder

RUN mkdir -p ~/.cargo && \
    echo '[registries.crates-io]' > ~/.cargo/config && \
    echo 'protocol = "sparse"' >> ~/.cargo/config

RUN apk add --no-cache libc-dev
RUN apk add --no-cache pkgconfig openssl-dev

RUN cargo new --bin /app
WORKDIR /app

# Just copy the Cargo.toml files and trigger a build so that we compile our
# dependencies only. This way we avoid layer cache invalidation if our
# dependencies haven't changed, resulting in faster builds.

COPY Cargo.toml .
COPY Cargo.lock .
RUN cargo update
ENV RUSTFLAGS="-Ctarget-feature=-crt-static"
ENV PKG_CONFIG_ALLOW_CROSS=1
RUN cargo build --release && rm -rf src/

# Copy the source code and run the build again. This should only compile the
# app itself as the dependencies were already built above.
COPY . ./
RUN cargo update
RUN rm target/release/deps/uptime_checker* && cargo build --release

FROM alpine:3.20

COPY --from=builder /app/target/release/uptime-checker /usr/local/bin/uptime-checker

RUN apk add --no-cache tini libgcc

RUN addgroup -S app && adduser -S app -G app
USER app

ENTRYPOINT ["/sbin/tini", "--", "/usr/local/bin/uptime-checker"]
