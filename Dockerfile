FROM rust:1.88-alpine3.22 AS builder

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

FROM alpine:3.22.1

RUN apk add --no-cache tini libgcc ca-certificates curl && \
    addgroup -S app --gid 1000 && \
    adduser -S app -G app --uid 1000

# XXX(epurkhiser): Install a missing Intermediary certificate the cloudflare
# does not seem to serve and expects browsers to use AIA to download the
# intermediary.
#
# Refs https://sentry.zendesk.com/agent/tickets/158451
#
# XXX(epurkhiser): I'm not sure why the ca-certificates package on alpine
# doesn't include this, but maybe with a newer or different distribution we
# wouldn't need this?
RUN curl -sSL https://ssl.com/repo/certs/SSL.com-TLS-T-ECC-R2.pem -o /usr/local/share/ca-certificates/SSl.com-TLS-T-ECC-R2.crt && \
    update-ca-certificates

COPY --from=builder /app/target/release/uptime-checker /usr/local/bin/uptime-checker

USER app

ENV RUST_BACKTRACE=1

ENTRYPOINT ["/sbin/tini", "--", "/usr/local/bin/uptime-checker"]
