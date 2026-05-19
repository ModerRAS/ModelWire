# =============================================================================
# Stage 1: Build
# =============================================================================
FROM rust:1.79-bookworm AS builder

WORKDIR /build

# Install cross-platform build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace files
COPY Cargo.toml ./
COPY Cargo.lock ./
COPY modelwire-core/ ./modelwire-core/
COPY modelwire-adapters/ ./modelwire-adapters/
COPY modelwire-db/ ./modelwire-db/
COPY modelwire-archive/ ./modelwire-archive/
COPY modelwire-server/ ./modelwire-server/
COPY modelwire-webui/ ./modelwire-webui/

# Cache cargo registry and build dependencies
RUN mkdir -p ~/.cargo/registry/cache ~/.cargo/registry/index
RUN cargo fetch --locked 2>/dev/null || true

# Build the server binary with release profile
# Use LTO and single codegen unit for smaller binary size
RUN cargo build --release --package modelwire-server \
    --locked \
    --manifest-path Cargo.toml

# =============================================================================
# Stage 2: Runtime
# =============================================================================
FROM debian:bookworm-slim AS runtime

# Install minimal runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    tini \
    curl \
    && rm -rf /var/lib/apt/lists/* \
    && apt-get clean \
    && rm -rf /var/cache/apt/archives/*

# Create non-root user for security (required by section 28.1.10)
RUN groupadd --gid 1000 modelwire && \
    useradd --uid 1000 --gid modelwire --shell /bin/false --create-home modelwire

WORKDIR /app

# Copy binary from builder
COPY --from=builder /build/target/release/modelwire-server /app/modelwire
COPY --from=builder /build/modelwire-webui/dist /app/modelwire-webui/dist

# Create data directories with proper permissions
RUN mkdir -p /app/data /app/data/archives && \
    chown -R modelwire:modelwire /app

# Set ownership
RUN chown -R modelwire:modelwire /app

# Use tini as init system for proper signal handling
ENTRYPOINT ["/usr/bin/tini", "--"]

# Run as non-root user (security requirement)
USER modelwire

# Expose default port
EXPOSE 8787

# Health check - verify the service is running without exposing secrets
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8787/healthz || exit 1

# Default command - serve with modelwire.toml in /app
CMD ["./modelwire", "--config", "modelwire.toml", "serve"]
