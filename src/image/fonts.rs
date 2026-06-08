//! Shared font database and resolution.
//!
//! A single `fontdb::Database` is loaded once at startup and shared (behind an
//! `Arc`) by both the SVG *renderer* (resvg/usvg) and the text *measurer*
//! (`ttf-parser`). Centralizing it here keeps the two in agreement: text wraps
//! using the metrics of the same face that will actually be rendered.

use once_cell::sync::Lazy;
use resvg::usvg::fontdb;
use std::sync::Arc;

/// Process-wide font state, initialized lazily on first text/SVG request.
pub static FONTS: Lazy<Fonts> = Lazy::new(Fonts::load);

pub struct Fonts {
    /// The shared font database. `Arc`-wrapped so each render clones a pointer
    /// rather than the (large) database.
    pub db: Arc<fontdb::Database>,
    /// A sans-serif family name that is actually installed (best effort). Used
    /// as the usvg default and appended to every text element's font-family
    /// chain so unresolved families fall back to a real sans-serif instead of
    /// usvg's built-in "Times New Roman" (a serif).
    pub default_family: String,
}

impl Fonts {
    fn load() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();

        // Pick concrete installed families for the generic CSS keywords so that
        // `sans-serif` / `serif` / `monospace` resolve to something present on
        // the host (e.g. the Liberation/DejaVu/Noto sets shipped in the Docker
        // image) rather than usvg's hardcoded Windows-centric defaults.
        let default_family = pick_family(
            &db,
            &[
                "Arial",
                "Helvetica",
                "Liberation Sans",
                "DejaVu Sans",
                "Noto Sans",
                "Segoe UI",
                "Roboto",
                "Verdana",
                "Tahoma",
            ],
        )
        .or_else(|| first_family(&db))
        .unwrap_or_else(|| "sans-serif".to_string());

        db.set_sans_serif_family(default_family.clone());
        if let Some(f) = pick_family(
            &db,
            &[
                "Liberation Serif",
                "DejaVu Serif",
                "Noto Serif",
                "Times New Roman",
                "Georgia",
            ],
        ) {
            db.set_serif_family(f);
        }
        if let Some(f) = pick_family(
            &db,
            &[
                "Liberation Mono",
                "DejaVu Sans Mono",
                "Noto Sans Mono",
                "Courier New",
                "Consolas",
            ],
        ) {
            db.set_monospace_family(f);
        }

        Fonts {
            db: Arc::new(db),
            default_family,
        }
    }

    /// Resolve the best face id for a requested family + weight, falling back
    /// through the installed default sans-serif and finally any available face.
    /// Returns `None` only when the database has no fonts at all.
    pub fn resolve_face(&self, family: &str, weight: fontdb::Weight) -> Option<fontdb::ID> {
        use fontdb::{Family, Query, Stretch, Style};

        let families = [
            Family::Name(family),
            Family::Name(&self.default_family),
            Family::SansSerif,
        ];
        let query = Query {
            families: &families,
            weight,
            stretch: Stretch::Normal,
            style: Style::Normal,
        };

        self.db
            .query(&query)
            .or_else(|| self.db.faces().next().map(|f| f.id))
    }
}

/// First family in `prefs` that exists in the database (case-insensitive).
fn pick_family(db: &fontdb::Database, prefs: &[&str]) -> Option<String> {
    prefs
        .iter()
        .find(|name| family_exists(db, name))
        .map(|name| name.to_string())
}

fn family_exists(db: &fontdb::Database, name: &str) -> bool {
    db.faces().any(|face| {
        face.families
            .iter()
            .any(|(fam, _)| fam.eq_ignore_ascii_case(name))
    })
}

/// The family name of the first loaded face, if any.
fn first_family(db: &fontdb::Database) -> Option<String> {
    db.faces()
        .next()
        .and_then(|f| f.families.first().map(|(name, _)| name.clone()))
}
