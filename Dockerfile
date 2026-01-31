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

# Build dependencies only (cached layer)
RUN cargo build --release && \
    rm -rf src target/release/deps/reimage*

# Copy actual source code
COPY src ./src

# Build the application
RUN cargo build --release

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

# Expose port
EXPOSE 80

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://localhost:80/health || exit 1

# Set environment
ENV PORT=80
ENV NODE_ENV=production
ENV RUST_LOG=warn

# Run the binary
CMD ["./reimage"]
