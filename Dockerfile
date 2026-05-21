# ================================================
# ReImage Rust - Multi-stage Docker Build
# ================================================

# Stage 1: Build
FROM rust:1.85-alpine AS builder

# Install build dependencies
RUN apk add --no-cache \
    musl-dev \
    pkgconfig \
    openssl-dev \
    openssl-libs-static

# Create app directory
WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Create dummy src to cache dependencies
RUN mkdir src && \
    echo "fn main() {}" > src/main.rs

# Build dependencies only (cached layer). The cleanup pattern must match the
# real crate name `reimage` (see Cargo.toml) or the stub artifacts persist and
# cargo will reuse the empty-main binary on the next build.
RUN cargo build --release && \
    rm -rf src target/release/reimage target/release/deps/reimage-*

# Copy actual source code
COPY src ./src

# Force a clean build of the application (touch main.rs so cargo definitely
# rebuilds, and verify the resulting binary is not a stub).
RUN touch src/main.rs && \
    cargo build --release && \
    test "$(stat -c%s target/release/reimage)" -gt 1000000 \
      || (echo "ERROR: built binary is suspiciously small — stub leak?" && exit 1)

# Stage 2: Runtime
FROM alpine:3.19

# Install runtime dependencies
RUN apk add --no-cache \
    ca-certificates \
    fontconfig \
    font-noto \
    font-noto-cjk \
    font-noto-emoji \
    ttf-dejavu \
    ttf-liberation \
    libgcc

# Create non-root user
RUN addgroup -g 1001 -S appgroup && \
    adduser -u 1001 -S appuser -G appgroup

# Create app directory
WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/reimage /app/reimage

# Set ownership
RUN chown -R appuser:appgroup /app

# Switch to non-root user
USER appuser

# Expose port (non-privileged port for security)
EXPOSE 8080

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://localhost:8080/health || exit 1

# Set environment
ENV PORT=8080
ENV NODE_ENV=production
ENV RUST_LOG=warn
# Security defaults
ENV AGENT_REJECT_UNAUTHORIZED=true
ENV ENABLE_REQUEST_LOGGING=true

# Run the binary
CMD ["./reimage"]
