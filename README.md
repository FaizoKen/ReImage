# ReImage Rust

High-performance image proxy and transformation service built with Rust.

Fetches remote images, applies transformations (resize, rounded corners, overlays, text), and serves optimized WebP output with built-in caching.

## Features

- **Image Proxying** - Fetch and serve remote images through a single endpoint
- **Resize** - Constrain images by max width/height while preserving aspect ratio
- **Rounded Corners** - Apply configurable border radius
- **Image Overlays** - Composite multiple images with positioning, resizing, and rounding
- **Text Overlays** - Render text onto images with configurable font, size, color, and alignment
- **WebP Output** - All output is encoded as WebP with configurable quality
- **Multi-Layer Caching** - In-memory caches for source images, overlays, masks, and output
- **SSRF Protection** - DNS rebinding prevention, private IP blocking, domain allowlists/blocklists
- **Authentication** - API key and HMAC-signed URL support
- **Rate Limiting** - Per-IP or per-API-key rate limiting
- **CORS & Referer Checks** - Configurable origin and referer validation
- **SVG Rendering** - Server-side SVG-to-raster via resvg with system font support
- **Docker Ready** - Multi-stage Alpine build with non-root user

## Quick Start

### Run Locally

```bash
# Clone and build
cargo build --release

# Run with defaults (port 8080)
./target/release/reimage-rust

# Or with custom config
PORT=3000 WEBP_QUALITY=90 cargo run --release
```

### Run with Docker

```bash
docker build -t reimage-rust .
docker run -p 8080:8080 reimage-rust
```

## API

### `GET /image`

Fetch, transform, and serve an image as WebP.

| Parameter | Type | Description |
|-----------|------|-------------|
| `src` | string | **Required.** Source image URL |
| `maxw` | int | Max width (px) |
| `maxh` | int | Max height (px) |
| `rad` | int | Border radius (px) |
| `overlay[]` | string | Overlay image URL(s) |
| `ox[]` | int | Overlay X offset(s) |
| `oy[]` | int | Overlay Y offset(s) |
| `omaxw[]` | int | Overlay max width(s) |
| `omaxh[]` | int | Overlay max height(s) |
| `orad[]` | int | Overlay border radius(es) |
| `text[]` | string | Text overlay content(s) |
| `tx[]` | int | Text X offset(s) |
| `ty[]` | int | Text Y offset(s) |
| `ts[]` | int | Text font size(s) |
| `tc[]` | string | Text color(s), e.g. `#ff0000` |
| `tf[]` | string | Text font family(ies) |
| `tmaxw[]` | int | Text max width(s) |
| `tmaxh[]` | int | Text max height(s) |
| `ta[]` | string | Text alignment(s): `left`, `center`, `right` |

**Example:**

```
GET /image?src=https://example.com/photo.jpg&maxw=800&rad=16
```

### `GET /health`

Returns `200 OK` when the service is running.

## Configuration

All settings are configured via environment variables. Copy `.env.example` to `.env` to get started.

See [.env.example](.env.example) for the full list of options with descriptions.

### Key Settings

| Variable | Default | Description |
|----------|---------|-------------|
| `PORT` | `8080` | Server port |
| `WEBP_QUALITY` | `80` | WebP output quality (0-100) |
| `MAX_DIMENSION` | `8000` | Max allowed image dimension |
| `MAX_DOWNLOAD_SIZE_MB` | `10` | Max source image size |
| `REQUIRE_AUTH` | `false` | Require API key or HMAC |
| `REQUIRE_HTTPS` | `false` | Only allow HTTPS sources |
| `RATE_LIMIT_PER_MINUTE` | `100` | Rate limit per IP |
| `OUTPUT_CACHE_SIZE_MB` | `750` | Output cache size |

## License

[MIT](LICENSE)
