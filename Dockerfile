# ================================================
# ReImage Rust - Multi-stage Docker Build
# ================================================
# Three stages:
#   1. chef    — prepares the cargo-chef "recipe" of the dep graph
#   2. builder — caches dep compilation independently of source changes
#   3. runtime — minimal alpine image with only the binary + fonts
# Build args:
#   WITH_CJK=true     — include Noto CJK font set (~100 MB). Leave unset for
#                       Latin-only deployments to keep the image small.

# ---------- Stage 1: cargo-chef recipe ----------
FROM rust:1.85-alpine AS chef
RUN apk add --no-cache musl-dev pkgconfig openssl-dev openssl-libs-static \
    && cargo install cargo-chef --locked --version ^0.1
WORKDIR /app

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

# ---------- Stage 2: build ----------
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Build & cache deps as a separate layer — only invalidated when the dep
# graph actually changes, not on every src tweak.
RUN cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release \
    && test "$(stat -c%s target/release/reimage)" -gt 1000000 \
       || (echo "ERROR: built binary is suspiciously small" && exit 1)

# ---------- Stage 3: runtime ----------
FROM alpine:3.19
ARG WITH_CJK=false

# Core runtime deps + Latin font coverage. CJK is opt-in via WITH_CJK=true
# because the noto-cjk package set is ~100 MB and most deployments don't
# render CJK glyphs.
RUN apk add --no-cache \
        ca-certificates \
        fontconfig \
        font-noto \
        font-noto-emoji \
        ttf-dejavu \
        ttf-liberation \
        libgcc \
    && if [ "$WITH_CJK" = "true" ]; then apk add --no-cache font-noto-cjk; fi

# Non-root user
RUN addgroup -g 1001 -S appgroup \
    && adduser -u 1001 -S appuser -G appgroup

WORKDIR /app
COPY --from=builder /app/target/release/reimage /app/reimage
RUN chown -R appuser:appgroup /app
USER appuser

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://localhost:8080/health || exit 1

ENV PORT=8080
ENV NODE_ENV=production
ENV RUST_LOG=warn
ENV AGENT_REJECT_UNAUTHORIZED=true
ENV ENABLE_REQUEST_LOGGING=true

CMD ["./reimage"]
