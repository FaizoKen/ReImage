use axum::{response::IntoResponse, Json};
use serde_json::json;

/// Health check endpoint
/// Returns worker PID, uptime, and memory stats
pub async fn health() -> impl IntoResponse {
    let uptime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Get process memory info (platform-dependent)
    let (used_mb, total_mb) = get_memory_usage();

    Json(json!({
        "status": "ok",
        "worker": std::process::id(),
        "uptime": uptime,
        "memory": {
            "used": used_mb,
            "total": total_mb
        }
    }))
}

#[cfg(target_os = "linux")]
fn get_memory_usage() -> (u64, u64) {
    // Read from /proc/self/statm on Linux
    if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
        let parts: Vec<&str> = statm.split_whitespace().collect();
        if parts.len() >= 2 {
            let page_size = 4096u64; // Common page size
            let total_pages: u64 = parts[0].parse().unwrap_or(0);
            let resident_pages: u64 = parts[1].parse().unwrap_or(0);
            return (
                (resident_pages * page_size) / (1024 * 1024),
                (total_pages * page_size) / (1024 * 1024),
            );
        }
    }
    (0, 0)
}

#[cfg(not(target_os = "linux"))]
fn get_memory_usage() -> (u64, u64) {
    // Fallback for non-Linux systems
    (0, 0)
}
