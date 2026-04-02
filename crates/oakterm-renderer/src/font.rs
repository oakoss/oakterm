//! Font loading and cell metric computation.
//!
//! Uses `fontdb` for system font discovery and `ttf-parser` for metric
//! extraction. The fallback strategy (Nerd Font bundling, procedural
//! box drawing) is documented in TREK-18 but implemented in later tasks.

use crate::shaper::FontMetrics;
use std::io;

/// Preferred monospace font families, tried in order.
const PREFERRED_FAMILIES: &[&str] = &[
    "JetBrains Mono",
    "Fira Code",
    "Cascadia Code",
    "SF Mono",
    "Menlo",
    "Consolas",
    "DejaVu Sans Mono",
    "Liberation Mono",
];

/// Create a `fontdb::Database` with system fonts loaded.
/// Reuse the returned database to avoid repeated filesystem scans (~50-200ms).
#[must_use]
pub fn system_font_db() -> fontdb::Database {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    db
}

/// Load the system's default monospace font and compute cell metrics
/// at the given font size (in points).
///
/// # Errors
/// Returns an error if no monospace font can be found or parsed.
pub fn load_default_metrics(
    db: &fontdb::Database,
    font_size: f32,
) -> io::Result<(FontMetrics, Vec<u8>)> {
    // Try preferred named families first.
    for family in PREFERRED_FAMILIES {
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight: fontdb::Weight::NORMAL,
            stretch: fontdb::Stretch::Normal,
            style: fontdb::Style::Normal,
        };

        if let Some(id) = db.query(&query) {
            if let Some((data, metrics)) = load_face(db, id, font_size) {
                return Ok((metrics, data));
            }
        }
    }

    // Fall back to the system's generic monospace family.
    let query = fontdb::Query {
        families: &[fontdb::Family::Monospace],
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    };

    if let Some(id) = db.query(&query) {
        if let Some((data, metrics)) = load_face(db, id, font_size) {
            return Ok((metrics, data));
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no monospace font found on system",
    ))
}

/// Load a specific font by family name.
///
/// # Errors
/// Returns an error if the font is not found or cannot be parsed.
pub fn load_font_by_name(
    db: &fontdb::Database,
    name: &str,
    font_size: f32,
) -> io::Result<(FontMetrics, Vec<u8>)> {
    let query = fontdb::Query {
        families: &[fontdb::Family::Name(name)],
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    };

    let id = db.query(&query).ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, format!("font not found: {name}"))
    })?;

    load_face(db, id, font_size)
        .map(|(data, metrics)| (metrics, data))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("failed to parse font: {name}"),
            )
        })
}

/// Font data for all style variants of a family.
pub struct FontVariants {
    /// Regular (normal weight, upright) face.
    pub regular: (Vec<u8>, FontMetrics),
    /// Bold face, if available.
    pub bold: Option<(Vec<u8>, FontMetrics)>,
    /// Italic face, if available.
    pub italic: Option<(Vec<u8>, FontMetrics)>,
    /// Bold-italic face, if available.
    pub bold_italic: Option<(Vec<u8>, FontMetrics)>,
}

/// Load all style variants of the system's default monospace font.
///
/// # Errors
/// Returns an error if no regular monospace font can be found.
pub fn load_default_variants(db: &fontdb::Database, font_size: f32) -> io::Result<FontVariants> {
    // Find a regular face first.
    let (regular_family, regular) = {
        // Try preferred named families first.
        let mut found: Option<(String, (Vec<u8>, FontMetrics))> = None;
        for family in PREFERRED_FAMILIES {
            if let Some(result) = load_face_with_query(
                db,
                family,
                fontdb::Weight::NORMAL,
                fontdb::Style::Normal,
                font_size,
            ) {
                found = Some(((*family).to_string(), result));
                break;
            }
        }
        if found.is_none() {
            // Fall back to generic monospace.
            let query = fontdb::Query {
                families: &[fontdb::Family::Monospace],
                weight: fontdb::Weight::NORMAL,
                stretch: fontdb::Stretch::Normal,
                style: fontdb::Style::Normal,
            };
            if let Some(id) = db.query(&query) {
                if let Some(face_info) = db.face(id) {
                    let family_name = face_info
                        .families
                        .first()
                        .map_or_else(String::new, |(name, _)| name.clone());
                    if let Some(result) = load_face(db, id, font_size) {
                        found = Some((family_name, result));
                    }
                }
            }
        }
        found.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no monospace font found on system")
        })?
    };

    let bold = load_face_with_query(
        db,
        &regular_family,
        fontdb::Weight::BOLD,
        fontdb::Style::Normal,
        font_size,
    );
    let italic = load_face_with_query(
        db,
        &regular_family,
        fontdb::Weight::NORMAL,
        fontdb::Style::Italic,
        font_size,
    )
    .or_else(|| {
        // Some fonts expose italic as Oblique.
        load_face_with_query(
            db,
            &regular_family,
            fontdb::Weight::NORMAL,
            fontdb::Style::Oblique,
            font_size,
        )
    });
    let bold_italic = load_face_with_query(
        db,
        &regular_family,
        fontdb::Weight::BOLD,
        fontdb::Style::Italic,
        font_size,
    )
    .or_else(|| {
        load_face_with_query(
            db,
            &regular_family,
            fontdb::Weight::BOLD,
            fontdb::Style::Oblique,
            font_size,
        )
    });

    Ok(FontVariants {
        regular,
        bold,
        italic,
        bold_italic,
    })
}

/// Load all style variants of a named font family.
///
/// # Errors
/// Returns an error if the regular face is not found.
pub fn load_font_variants(
    db: &fontdb::Database,
    family: &str,
    font_size: f32,
) -> io::Result<FontVariants> {
    let regular = load_face_with_query(
        db,
        family,
        fontdb::Weight::NORMAL,
        fontdb::Style::Normal,
        font_size,
    )
    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("font not found: {family}")))?;
    let bold = load_face_with_query(
        db,
        family,
        fontdb::Weight::BOLD,
        fontdb::Style::Normal,
        font_size,
    );
    let italic = load_face_with_query(
        db,
        family,
        fontdb::Weight::NORMAL,
        fontdb::Style::Italic,
        font_size,
    )
    .or_else(|| {
        load_face_with_query(
            db,
            family,
            fontdb::Weight::NORMAL,
            fontdb::Style::Oblique,
            font_size,
        )
    });
    let bold_italic = load_face_with_query(
        db,
        family,
        fontdb::Weight::BOLD,
        fontdb::Style::Italic,
        font_size,
    )
    .or_else(|| {
        load_face_with_query(
            db,
            family,
            fontdb::Weight::BOLD,
            fontdb::Style::Oblique,
            font_size,
        )
    });

    Ok(FontVariants {
        regular,
        bold,
        italic,
        bold_italic,
    })
}

/// Query fontdb for a specific weight/style combination of a family.
fn load_face_with_query(
    db: &fontdb::Database,
    family: &str,
    weight: fontdb::Weight,
    style: fontdb::Style,
    font_size: f32,
) -> Option<(Vec<u8>, FontMetrics)> {
    let query = fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        weight,
        stretch: fontdb::Stretch::Normal,
        style,
    };
    let id = db.query(&query)?;
    // Verify the match actually has the requested properties.
    // fontdb uses CSS best-match, which can return regular for a bold query.
    let face_info = db.face(id)?;
    if face_info.weight != weight || face_info.style != style {
        return None;
    }
    load_face(db, id, font_size)
}

fn load_face(
    db: &fontdb::Database,
    id: fontdb::ID,
    font_size: f32,
) -> Option<(Vec<u8>, FontMetrics)> {
    let face_info = db.face(id)?;
    let index = face_info.index;

    let mut font_data = None;
    db.with_face_data(id, |data, _| {
        font_data = Some(data.to_vec());
    });
    let data = font_data?;

    let face = ttf_parser::Face::parse(&data, index).ok()?;
    let metrics = compute_metrics_from_face(&face, font_size);

    Some((data, metrics))
}

/// Compute cell metrics from a parsed font face. Public for use by shapers.
#[must_use]
pub fn compute_metrics_from_face(face: &ttf_parser::Face<'_>, font_size: f32) -> FontMetrics {
    let units_per_em = f32::from(face.units_per_em());
    let scale = font_size / units_per_em;

    let ascender = f32::from(face.ascender()) * scale;
    let descender = f32::from(face.descender()) * scale;

    let cell_width = glyph_advance(face, 'M')
        .or_else(|| glyph_advance(face, ' '))
        .unwrap_or(units_per_em * 0.6)
        * scale;

    let cell_height = ascender - descender;

    let underline_position = face
        .underline_metrics()
        .map_or(descender * 0.5, |m| f32::from(m.position) * scale);

    FontMetrics {
        cell_width,
        cell_height,
        baseline: ascender,
        underline_position,
    }
}

fn glyph_advance(face: &ttf_parser::Face<'_>, c: char) -> Option<f32> {
    let glyph_id = face.glyph_index(c)?;
    let advance = face.glyph_hor_advance(glyph_id)?;
    Some(f32::from(advance))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_system_font() {
        let db = system_font_db();
        let result = load_default_metrics(&db, 14.0);
        if let Ok((metrics, data)) = result {
            assert!(metrics.cell_width > 0.0);
            assert!(metrics.cell_height > 0.0);
            assert!(metrics.baseline > 0.0);
            assert!(!data.is_empty());
        } else {
            eprintln!("no system monospace font found — skipping");
        }
    }

    #[test]
    fn metrics_scale_with_size() {
        let db = system_font_db();
        let r12 = load_default_metrics(&db, 12.0);
        let r24 = load_default_metrics(&db, 24.0);

        if let (Ok((m12, _)), Ok((m24, _))) = (r12, r24) {
            let ratio = m24.cell_height / m12.cell_height;
            assert!(
                (1.8..=2.2).contains(&ratio),
                "height ratio should be ~2.0, got {ratio:.2}"
            );
        }
    }

    #[test]
    fn nonexistent_font_returns_error() {
        let db = system_font_db();
        let result = load_font_by_name(&db, "NonExistent Font That Does Not Exist", 14.0);
        assert!(result.is_err());
    }
}
