pub mod health;
pub mod image;
pub mod static_routes;

pub use health::health;
pub use image::{handle_image, AppState, ImageQuery};
pub use static_routes::{favicon, robots_txt};
