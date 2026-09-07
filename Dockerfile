FROM rust:1.85-slim AS builder

RUN apt-get update && apt-get install -y protobuf-compiler && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock ./

# Create a dummy main.rs to build dependencies only
RUN mkdir src && echo 'fn main() {}' > src/main.rs

# Build dependencies (this layer is cached unless Cargo.toml/Cargo.lock change)
RUN cargo build --release && rm -rf src

# Copy actual source code
COPY . .

# Touch main.rs so cargo knows it changed
RUN touch src/main.rs

# Build the application
RUN cargo build --release

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/fortune-402 /usr/local/bin/fortune-402
EXPOSE 3402

# Probes /health on LISTEN_ADDR (default 0.0.0.0:3402) over loopback. The probe
# is a separate process that reads the same environment variables as the server.
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD ["fortune-402", "health-check"]

CMD ["fortune-402"]
