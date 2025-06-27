FROM rust:1.85-alpine3.20 as builder

# Install system dependencies
RUN apk add --no-cache \
    libc-dev \
    cmake \
    make \
    g++ \
    pkgconfig \
    openssl-dev \
    protoc

# Configure cargo
RUN mkdir -p ~/.cargo && \
    echo '[registries.crates-io]' > ~/.cargo/config && \
    echo 'protocol = "sparse"' >> ~/.cargo/config

WORKDIR /app

# Copy only the files needed for dependency caching
COPY Cargo.toml Cargo.lock ./
COPY redis-test-macro redis-test-macro/

# Create a dummy main.rs to build dependencies
RUN mkdir src && \
    echo "fn main() {}" > src/main.rs && \
    # Build dependencies only
    export RUSTFLAGS="-Ctarget-feature=-crt-static" && \
    export PKG_CONFIG_ALLOW_CROSS=1 && \
    cargo build --release && \
    rm -rf src/

# Set environment variables for the final build
ENV RUSTFLAGS="-Ctarget-feature=-crt-static"
ENV PKG_CONFIG_ALLOW_CROSS=1
ARG UPTIME_CHECKER_GIT_REVISION
ENV UPTIME_CHECKER_GIT_REVISION=$UPTIME_CHECKER_GIT_REVISION

# Copy the actual source code and build
COPY . .
RUN cargo build --release

FROM alpine:3.20

COPY --from=builder /app/target/release/uptime-checker /usr/local/bin/uptime-checker

RUN apk add --no-cache tini libgcc curl && \
    addgroup -S app --gid 1000 && \
    adduser -S app -G app --uid 1000

USER app

ENTRYPOINT ["/sbin/tini", "--", "/usr/local/bin/uptime-checker"]
