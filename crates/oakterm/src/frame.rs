//! Frame assembly: turning font/glyph state and pane geometry into the
//! background and glyph draw lists the GPU pipeline consumes. Owns
//! [`FontState`] (the per-frame glyph atlas cache that assembly mutates) so
//! the cache internals stay private to this module; callers see only a small
//! accessor surface plus the `assemble_*` entry points.

use std::collections::HashMap;

use oakterm_renderer::atlas::AtlasPlane;
use oakterm_renderer::font;
use oakterm_renderer::pipeline::{BgSection, BgUniforms};
use oakterm_renderer::shaper::{FontKey, FontMetrics};
use oakterm_renderer::swash_shaper::SwashShaper;
use oakterm_terminal::grid::MAX_GRID_DIMENSION;
use oakterm_terminal::grid::selection::Selection;

use tracing::{debug, warn};

use crate::pane_view::PaneView;
use crate::render_grid::{self, ClientGrid};
use crate::{layout, tab_bar};

/// Tab bar colors, fixed until the theme system (TREK-212) lands.
const TAB_BAR_BG_RGB: [u8; 3] = [16, 16, 16];
const TAB_ACTIVE_BG_RGB: [u8; 3] = [72, 72, 72];
const TAB_ACTIVE_FG_RGB: [u8; 3] = [255, 255, 255];
const TAB_INACTIVE_BG_RGB: [u8; 3] = [36, 36, 36];
const TAB_INACTIVE_FG_RGB: [u8; 3] = [160, 160, 160];

pub(crate) struct FontState {
    shaper: SwashShaper,
    font_key: FontKey,
    bold_key: Option<FontKey>,
    italic_key: Option<FontKey>,
    bold_italic_key: Option<FontKey>,
    atlas: AtlasPlane,
    color_atlas: AtlasPlane,
    color_keys: std::collections::HashSet<oakterm_renderer::atlas::GlyphCacheKey>,
    font_size: f32,
    metrics: FontMetrics,
}

impl FontState {
    fn font_keys(&self) -> render_grid::FontKeys {
        render_grid::FontKeys {
            regular: self.font_key,
            bold: self.bold_key,
            italic: self.italic_key,
            bold_italic: self.bold_italic_key,
        }
    }

    /// Cell metrics for layout and grid sizing.
    #[must_use]
    pub(crate) fn metrics(&self) -> &FontMetrics {
        &self.metrics
    }

    /// The monochrome glyph atlas, read when uploading it to the GPU.
    #[must_use]
    pub(crate) fn atlas(&self) -> &AtlasPlane {
        &self.atlas
    }

    /// The color glyph atlas, read when uploading it to the GPU.
    #[must_use]
    pub(crate) fn color_atlas(&self) -> &AtlasPlane {
        &self.color_atlas
    }
}

pub(crate) fn try_init_font(
    config: &oakterm_config::ConfigValues,
    font_size: f32,
) -> Result<FontState, String> {
    let db = font::system_font_db();
    let variants = if config.font_family.is_empty() {
        font::load_default_variants(&db, font_size)
            .map_err(|e| format!("no system monospace font: {e}"))?
    } else {
        match font::load_font_variants(&db, &config.font_family, font_size) {
            Ok(result) => result,
            Err(e) => {
                warn!(
                    error = %e,
                    font_family = %config.font_family,
                    "font not found, using system default"
                );
                font::load_default_variants(&db, font_size)
                    .map_err(|e| format!("no system monospace font: {e}"))?
            }
        }
    };

    let (regular_data, metrics) = variants.regular;
    let mut shaper = SwashShaper::new();
    let font_key = shaper
        .load_font(regular_data, font_size)
        .ok_or_else(|| "failed to load font into shaper".to_string())?;

    let bold_key = variants
        .bold
        .and_then(|(data, _)| shaper.load_font(data, font_size));
    let italic_key = variants
        .italic
        .and_then(|(data, _)| shaper.load_font(data, font_size));
    let bold_italic_key = variants
        .bold_italic
        .and_then(|(data, _)| shaper.load_font(data, font_size));

    debug!(
        ?font_key,
        ?bold_key,
        ?italic_key,
        ?bold_italic_key,
        "font variants loaded"
    );

    Ok(FontState {
        shaper,
        font_key,
        bold_key,
        italic_key,
        bold_italic_key,
        atlas: AtlasPlane::new(),
        color_atlas: AtlasPlane::new(),
        color_keys: std::collections::HashSet::new(),
        font_size,
        metrics,
    })
}

/// Split from the GPU upload/draw so frame assembly is testable without
/// a device. `uploads`/`color_uploads` must reach the GPU atlas textures
/// before `glyphs` is drawn.
#[derive(Default)]
pub(crate) struct FrameAssembly {
    pub(crate) bg_sections: Vec<BgSection>,
    pub(crate) glyphs: Vec<oakterm_renderer::pipeline::GlyphVertex>,
    pub(crate) uploads: Vec<render_grid::GlyphUpload>,
    pub(crate) color_uploads: Vec<render_grid::GlyphUpload>,
}

/// Cursor and selection render only in the focused pane; the cursor
/// honors the blink phase.
#[allow(clippy::cast_precision_loss)] // pixel origins fit in f32
pub(crate) fn assemble_frame(
    font: &mut FontState,
    panes: &HashMap<u32, PaneView>,
    render_list: &[(u32, layout::PixelRect)],
    focused_pane: u32,
    blink_visible: bool,
    viewport: (f32, f32),
) -> FrameAssembly {
    let mut assembly = FrameAssembly::default();
    for &(pane_id, rect) in render_list {
        let Some(pane) = panes.get(&pane_id) else {
            continue;
        };
        let grid = pane.grid();
        let is_focused = pane_id == focused_pane;
        let cursor_vis = is_focused
            && grid.cursor_visible
            && (blink_visible || !matches!(grid.cursor_style, 0 | 2 | 4));
        let selection = if is_focused {
            pane.selection.as_ref()
        } else {
            None
        };
        push_grid(
            &mut assembly,
            font,
            grid,
            (rect.x as f32, rect.y as f32),
            cursor_vis,
            selection,
            pane.viewport_offset(),
            viewport,
        );
    }
    assembly
}

/// Append one grid's backgrounds and glyphs to `assembly` at pixel `origin`,
/// caching new glyphs in `font`'s atlases. Shared by the pane and tab-bar
/// paths so the glyph contract both callers share lives in one place.
#[allow(clippy::too_many_arguments)]
fn push_grid(
    assembly: &mut FrameAssembly,
    font: &mut FontState,
    grid: &ClientGrid,
    origin: (f32, f32),
    cursor_vis: bool,
    selection: Option<&Selection>,
    viewport_offset: u32,
    viewport: (f32, f32),
) {
    let keys = font.font_keys();
    let bg = grid.bg_colors(cursor_vis, selection, viewport_offset);
    let (glyphs, uploads, color_uploads) = grid.glyph_instances(
        &font.metrics,
        &keys,
        font.font_size,
        &font.shaper,
        &mut font.atlas,
        &mut font.color_atlas,
        &mut font.color_keys,
        cursor_vis,
        selection,
        viewport_offset,
        origin.0,
        origin.1,
    );
    assembly.bg_sections.push(BgSection::new(
        BgUniforms {
            cols: u32::from(grid.cols),
            rows: u32::from(grid.rows),
            cell_width: font.metrics.cell_width,
            cell_height: font.metrics.cell_height,
            viewport_width: viewport.0,
            viewport_height: viewport.1,
            pad_left: origin.0,
            pad_top: origin.1,
        },
        bg,
    ));
    assembly.glyphs.extend(glyphs);
    assembly.uploads.extend(uploads);
    assembly.color_uploads.extend(color_uploads);
}

/// Append the tab bar to `assembly`: a full-width underlay at the
/// window's top edge plus a one-row synthetic grid of tab labels
/// rendered through the normal glyph path.
pub(crate) fn assemble_tab_bar(
    font: &mut FontState,
    tabs: &tab_bar::TabsState,
    viewport: (f32, f32),
    assembly: &mut FrameAssembly,
) {
    let metrics = font.metrics;
    let bar_h = tab_bar::tab_bar_height(true, Some(&metrics));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let width_px = viewport.0.max(0.0) as u32;
    assembly.bg_sections.push(solid_section(
        layout::PixelRect {
            x: 0,
            y: 0,
            width: width_px,
            height: bar_h,
        },
        TAB_BAR_BG_RGB,
        viewport,
    ));

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cols = ((viewport.0 / metrics.cell_width).max(0.0) as u16).clamp(1, MAX_GRID_DIMENSION);
    let spans = tab_bar::layout_strip(tabs.tabs(), cols);
    let cells = tab_bar::strip_cells(tabs.tabs(), tabs.active_tab(), &spans);
    let mut grid = ClientGrid::new(cols, 1);
    grid.fill_bg(TAB_BAR_BG_RGB);
    for (col, cell) in cells {
        let (fg, bg) = if cell.active {
            (TAB_ACTIVE_FG_RGB, TAB_ACTIVE_BG_RGB)
        } else {
            (TAB_INACTIVE_FG_RGB, TAB_INACTIVE_BG_RGB)
        };
        grid.set_cell(col, 0, cell.ch, fg, bg);
    }

    push_grid(assembly, font, &grid, (0.0, 0.0), false, None, 0, viewport);
}

/// A solid rectangle drawn through the bg pipeline as a 1x1 cell grid
/// whose cell size is the rectangle.
pub(crate) fn solid_section(
    rect: layout::PixelRect,
    rgb: [u8; 3],
    viewport: (f32, f32),
) -> BgSection {
    #[allow(clippy::cast_precision_loss)] // pixel coordinates fit in f32
    BgSection::new(
        BgUniforms {
            cols: 1,
            rows: 1,
            cell_width: rect.width as f32,
            cell_height: rect.height as f32,
            viewport_width: viewport.0,
            viewport_height: viewport.1,
            pad_left: rect.x as f32,
            pad_top: rect.y as f32,
        },
        vec![render_grid::pack_bg_color(rgb)],
    )
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // exact arithmetic on small integers in f64
mod tests {
    use super::{FrameAssembly, assemble_frame, assemble_tab_bar, try_init_font};
    use crate::layout::PixelRect;
    use crate::pane_view::PaneView;
    use crate::render_grid::ClientGrid;
    use crate::tab_bar::TabsState;
    use oakterm_protocol::message::{TabEntry, TabList};
    use std::collections::HashMap;

    #[test]
    fn assemble_frame_emits_one_bg_section_per_pane_at_its_origin() {
        let Ok(mut font) = try_init_font(&oakterm_config::ConfigValues::default(), 14.0) else {
            eprintln!("skipping: no system monospace font available");
            return;
        };
        let mut panes = HashMap::new();
        panes.insert(1, PaneView::new(ClientGrid::new(10, 5)));
        panes.insert(2, PaneView::new(ClientGrid::new(10, 5)));
        let rect = |x| PixelRect {
            x,
            y: 0,
            width: 100,
            height: 100,
        };
        // Pane 9 has no view and must be skipped, not rendered empty.
        let render_list = [(1, rect(0)), (2, rect(120)), (9, rect(240))];

        let assembly = assemble_frame(&mut font, &panes, &render_list, 1, true, (400.0, 300.0));

        assert_eq!(assembly.bg_sections.len(), 2);
        assert_eq!(assembly.bg_sections[0].uniforms.pad_left, 0.0);
        assert_eq!(assembly.bg_sections[1].uniforms.pad_left, 120.0);
    }

    #[test]
    fn assemble_tab_bar_emits_underlay_and_label_grid() {
        let Ok(mut font) = try_init_font(&oakterm_config::ConfigValues::default(), 14.0) else {
            return;
        };
        let mut tabs = TabsState::default();
        tabs.apply(TabList {
            workspace_id: 0,
            workspace_name: "default".to_string(),
            active_tab: 0,
            tabs: vec![
                TabEntry {
                    tab_id: 0,
                    focused_pane: 0,
                    name: "a".to_string(),
                },
                TabEntry {
                    tab_id: 1,
                    focused_pane: 10,
                    name: "b".to_string(),
                },
            ],
        });
        assert!(tabs.bar_visible());

        let mut assembly = FrameAssembly::default();
        assemble_tab_bar(&mut font, &tabs, (400.0, 300.0), &mut assembly);

        // Full-width underlay plus the one-row label grid at the origin.
        assert_eq!(assembly.bg_sections.len(), 2);
        assert_eq!(assembly.bg_sections[1].uniforms.rows, 1);
        assert_eq!(assembly.bg_sections[1].uniforms.pad_left, 0.0);
        assert_eq!(assembly.bg_sections[1].uniforms.pad_top, 0.0);
        assert!(!assembly.glyphs.is_empty());
    }
}
