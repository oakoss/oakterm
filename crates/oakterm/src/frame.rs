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

use tracing::{debug, info, warn};

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
        .load_font(regular_data, 0, font_size)
        .ok_or_else(|| "failed to load font into shaper".to_string())?;

    let bold_key = variants
        .bold
        .and_then(|(data, _)| shaper.load_font(data, 0, font_size));
    let italic_key = variants
        .italic
        .and_then(|(data, _)| shaper.load_font(data, 0, font_size));
    let bold_italic_key = variants
        .bold_italic
        .and_then(|(data, _)| shaper.load_font(data, 0, font_size));

    let fallback = shaper.install_fallbacks(&db, font_size);
    if fallback.loaded == 0 {
        info!("no emoji/symbol fallback font found; emoji will render as tofu");
    }

    debug!(
        ?font_key,
        ?bold_key,
        ?italic_key,
        ?bold_italic_key,
        fallback = fallback.loaded,
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

impl FrameAssembly {
    /// Drop glyphs under the pixel rect at `origin` with `size`, grown by
    /// one cell of bearing slop on every side. Backgrounds and text are
    /// separate passes with no z-order, so an overlay panel must remove
    /// the glyphs beneath it before pushing its own.
    pub(crate) fn occlude(&mut self, origin: (f32, f32), size: (f32, f32), cell: (f32, f32)) {
        let (x0, y0) = (origin.0 - cell.0, origin.1 - cell.1);
        let (x1, y1) = (origin.0 + size.0 + cell.0, origin.1 + size.1 + cell.1);
        self.glyphs
            .retain(|g| !(g.pos[0] >= x0 && g.pos[0] < x1 && g.pos[1] >= y0 && g.pos[1] < y1));
    }
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

/// Palette colors, fixed until the theme system (TREK-212) lands.
const PALETTE_BG_RGB: [u8; 3] = [24, 24, 24];
const PALETTE_BORDER_RGB: [u8; 3] = [64, 64, 64];
const PALETTE_FG_RGB: [u8; 3] = [220, 220, 220];
const PALETTE_DIM_FG_RGB: [u8; 3] = [140, 140, 140];
const PALETTE_MATCH_FG_RGB: [u8; 3] = [120, 200, 255];
const PALETTE_SELECTED_BG_RGB: [u8; 3] = [72, 72, 72];

const PALETTE_MAX_COLS: u16 = 60;

/// Append the command palette overlay: a bordered panel centered near the
/// top with the query row, then the visible result window (or the
/// no-matches message).
///
/// Backgrounds and text are separate passes with no z-order, so pane
/// glyphs already in `assembly` would paint through the panel; glyphs
/// under the panel (grown by one cell of bearing slop) are dropped here.
/// Call after every other assembly step so the panel composites on top.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn assemble_palette(
    font: &mut FontState,
    palette: &crate::palette::PaletteState,
    viewport: (f32, f32),
    top_px: u32,
    assembly: &mut FrameAssembly,
) {
    let metrics = font.metrics;
    let cell_w = metrics.cell_width;
    let cell_h = metrics.cell_height;
    let view_cols = ((viewport.0 / cell_w).max(0.0) as u16).clamp(1, MAX_GRID_DIMENSION);
    let cols = view_cols.min(PALETTE_MAX_COLS);
    let results = palette.results();
    let visible = results.len().clamp(1, crate::palette::MAX_VISIBLE_RESULTS);
    let rows = (1 + visible) as u16;

    // Cell-aligned horizontal center; one-row gap below the bar keeps the
    // occlusion margin off the tab-bar glyphs.
    let x_px = f32::from((view_cols - cols) / 2) * cell_w;
    #[allow(clippy::cast_precision_loss)]
    let y_px = top_px as f32 + cell_h;
    let w_px = f32::from(cols) * cell_w;
    let h_px = f32::from(rows) * cell_h;

    assembly.occlude((x_px, y_px), (w_px, h_px), (cell_w, cell_h));

    assembly.bg_sections.push(solid_section(
        layout::PixelRect {
            x: (x_px as u32).saturating_sub(1),
            y: (y_px as u32).saturating_sub(1),
            width: w_px as u32 + 2,
            height: h_px as u32 + 2,
        },
        PALETTE_BORDER_RGB,
        viewport,
    ));

    let mut grid = ClientGrid::new(cols, rows);
    grid.fill_bg(PALETTE_BG_RGB);

    // Query row, tail-anchored so the newest characters stay visible.
    let query: Vec<char> = palette.query().chars().collect();
    let avail = usize::from(cols.saturating_sub(3));
    let start = query.len().saturating_sub(avail);
    let mut col = 1u16;
    for &c in &query[start..] {
        grid.set_cell(col, 0, c, PALETTE_FG_RGB, PALETTE_BG_RGB);
        col += 1;
    }
    if col < cols {
        grid.set_cell(col, 0, '▏', PALETTE_DIM_FG_RGB, PALETTE_BG_RGB);
    }

    if results.is_empty() {
        for (i, c) in "No matching actions".chars().enumerate() {
            let col = 1 + i as u16;
            if col >= cols {
                break;
            }
            grid.set_cell(col, 1, c, PALETTE_DIM_FG_RGB, PALETTE_BG_RGB);
        }
    } else {
        let selected = palette.selected_index();
        let first = palette.window_start();
        for (i, result) in results.iter().enumerate().skip(first).take(visible) {
            let row = (i - first + 1) as u16;
            render_palette_row(&mut grid, row, result, i == selected, cols);
        }
    }

    push_grid(
        assembly,
        font,
        &grid,
        (x_px, y_px),
        false,
        None,
        0,
        viewport,
    );
}

/// Render one result row: selection background, label with match-position
/// highlighting, right-aligned keybind hint.
#[allow(clippy::cast_possible_truncation)]
fn render_palette_row(
    grid: &mut ClientGrid,
    row: u16,
    result: &crate::palette::PaletteResult,
    selected: bool,
    cols: u16,
) {
    let bg = if selected {
        for c in 0..cols {
            grid.set_cell(c, row, ' ', PALETTE_FG_RGB, PALETTE_SELECTED_BG_RGB);
        }
        PALETTE_SELECTED_BG_RGB
    } else {
        PALETTE_BG_RGB
    };

    let hint_cols = result
        .keybind
        .as_deref()
        .map_or(0, |h| h.chars().count() + 2);
    let label_avail = usize::from(cols).saturating_sub(2 + hint_cols);
    for (ci, ch) in result.label.chars().take(label_avail).enumerate() {
        let fg = if result.match_positions.contains(&ci) {
            PALETTE_MATCH_FG_RGB
        } else {
            PALETTE_FG_RGB
        };
        grid.set_cell(1 + ci as u16, row, ch, fg, bg);
    }
    if let Some(hint) = result.keybind.as_deref() {
        let len = hint.chars().count() as u16;
        if len + 1 < cols {
            let start_col = cols - 1 - len;
            for (ci, ch) in hint.chars().enumerate() {
                grid.set_cell(start_col + ci as u16, row, ch, PALETTE_DIM_FG_RGB, bg);
            }
        }
    }
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
    use super::{FrameAssembly, assemble_frame, assemble_palette, assemble_tab_bar, try_init_font};
    use crate::layout::PixelRect;
    use crate::pane_view::PaneView;
    use crate::render_grid::ClientGrid;
    use crate::tab_bar::TabsState;
    use oakterm_protocol::message::{TabEntry, TabList};
    use std::collections::HashMap;

    fn glyph_at(pos: [f32; 2]) -> oakterm_renderer::pipeline::GlyphVertex {
        oakterm_renderer::pipeline::GlyphVertex {
            pos,
            size: [8.0, 16.0],
            uv_origin: [0.0, 0.0],
            fg_color: [1.0, 1.0, 1.0, 1.0],
            bg_luminance: 0.0,
            is_color: 0.0,
            pad: [0.0, 0.0],
        }
    }

    #[test]
    fn occlude_drops_only_glyphs_within_the_grown_rect() {
        let mut assembly = FrameAssembly {
            glyphs: vec![
                glyph_at([100.0, 100.0]), // inside the rect
                glyph_at([92.5, 92.5]),   // inside the one-cell slop margin
                glyph_at([50.0, 100.0]),  // left of the margin
                glyph_at([100.0, 300.0]), // below the margin
            ],
            ..Default::default()
        };
        // Grown bounds: x in [92, 188), y in [84, 156).
        assembly.occlude((100.0, 100.0), (80.0, 40.0), (8.0, 16.0));
        let remaining: Vec<[f32; 2]> = assembly.glyphs.iter().map(|g| g.pos).collect();
        assert_eq!(remaining, vec![[50.0, 100.0], [100.0, 300.0]]);
    }

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
    fn emoji_cell_routes_to_the_color_atlas_via_fallback() {
        use oakterm_renderer::shaper::{PixelFormat, TextRun, TextShaper};
        use oakterm_renderer::{font, swash_shaper::SwashShaper};

        // Probe whether this host has a COLOR glyph for the crab. A
        // monochrome-only fallback (e.g. Noto Emoji, not Noto Color Emoji)
        // legitimately routes to the alpha atlas, so gate the color assertion
        // on real color coverage, not merely on a fallback existing.
        let db = font::system_font_db();
        let mut probe = SwashShaper::new();
        let Ok((_m, data)) = font::load_default_metrics(&db, 14.0) else {
            return;
        };
        let Some(primary) = probe.load_font(data, 0, 14.0) else {
            return;
        };
        probe.install_fallbacks(&db, 14.0);
        let crab = probe.shape(&TextRun {
            text: "🦀",
            font: primary,
            size: 14.0,
        })[0]
            .glyph;
        if crab.font == primary || probe.rasterize(crab, 14.0).format != PixelFormat::Rgba32 {
            eprintln!("skipping: no color emoji glyph for the crab on this host");
            return;
        }

        let Ok(mut font_state) = try_init_font(&oakterm_config::ConfigValues::default(), 14.0)
        else {
            return;
        };
        let mut grid = ClientGrid::new(10, 5);
        grid.set_cell(0, 0, '🦀', [255, 255, 255], [0, 0, 0]);
        let mut panes = HashMap::new();
        panes.insert(1, PaneView::new(grid));
        let render_list = [(
            1,
            PixelRect {
                x: 0,
                y: 0,
                width: 200,
                height: 100,
            },
        )];

        let assembly = assemble_frame(
            &mut font_state,
            &panes,
            &render_list,
            1,
            true,
            (400.0, 300.0),
        );

        assert!(
            !assembly.color_uploads.is_empty(),
            "emoji must produce a color-atlas upload through the fallback face"
        );
    }

    #[test]
    fn assemble_palette_occludes_glyphs_beneath_it_and_keeps_the_rest() {
        use oakterm_config::{ActionContext, ActionRegistry, KeybindRegistry};

        let Ok(mut font) = try_init_font(&oakterm_config::ConfigValues::default(), 14.0) else {
            return;
        };
        let mut palette = crate::palette::PaletteState::new();
        palette.open(
            &ActionRegistry::core(&KeybindRegistry::new()),
            ActionContext {
                pane_count: 1,
                tab_count: 1,
                ..Default::default()
            },
        );

        // Fractional coordinates can't collide with real palette glyphs.
        let mut assembly = FrameAssembly {
            glyphs: vec![glyph_at([400.3, 55.7]), glyph_at([10.3, 550.7])],
            ..Default::default()
        };

        assemble_palette(&mut font, &palette, (800.0, 600.0), 0, &mut assembly);

        // Border underlay + panel grid.
        assert_eq!(assembly.bg_sections.len(), 2);
        let survives = |pos: [f32; 2]| assembly.glyphs.iter().any(|g| g.pos == pos);
        assert!(
            !survives([400.3, 55.7]),
            "glyph beneath the panel must be dropped"
        );
        assert!(
            survives([10.3, 550.7]),
            "glyph outside the panel must survive"
        );
        // The panel contributed its own text (query cursor, result labels).
        assert!(assembly.glyphs.len() > 1);
    }

    #[test]
    fn assemble_palette_scroll_window_tracks_the_stateful_offset() {
        use oakterm_config::{ActionContext, ActionRegistry, KeybindRegistry};

        let Ok(mut font) = try_init_font(&oakterm_config::ConfigValues::default(), 14.0) else {
            return;
        };
        let reg = ActionRegistry::core(&KeybindRegistry::new());
        let ctx = ActionContext {
            pane_count: 2,
            tab_count: 2,
            can_focus_left: true,
            can_focus_right: true,
            can_focus_up: true,
            can_focus_down: true,
        };
        let mut palette = crate::palette::PaletteState::new();
        palette.open(&reg, ctx); // 14 results, 10 visible

        let mut selected_row = |palette: &crate::palette::PaletteState| {
            let mut assembly = FrameAssembly::default();
            assemble_palette(&mut font, palette, (800.0, 600.0), 0, &mut assembly);
            let panel = &assembly.bg_sections[1];
            assert_eq!(panel.uniforms.rows, 11, "query row + 10 result rows");
            let cols = panel.uniforms.cols as usize;
            let selected_bg = crate::render_grid::pack_bg_color(super::PALETTE_SELECTED_BG_RGB);
            (0..panel.uniforms.rows as usize).find(|row| panel.colors[row * cols] == selected_bg)
        };

        assert_eq!(selected_row(&palette), Some(1), "selection starts at top");
        for _ in 0..9 {
            palette.move_down();
        }
        assert_eq!(selected_row(&palette), Some(10), "bottom of the window");
        // Crossing the edge scrolls the list under a bottom-pinned cursor.
        for _ in 0..3 {
            palette.move_down();
        }
        assert_eq!(selected_row(&palette), Some(10));
        // Moving up walks the cursor within the window, not the list.
        for _ in 0..5 {
            palette.move_up();
        }
        assert_eq!(selected_row(&palette), Some(5));
    }

    #[test]
    fn assemble_palette_handles_edge_inputs_without_panicking() {
        use oakterm_config::{ActionContext, ActionRegistry, KeybindRegistry};

        let Ok(mut font) = try_init_font(&oakterm_config::ConfigValues::default(), 14.0) else {
            return;
        };
        let reg = ActionRegistry::core(&KeybindRegistry::new());
        let ctx = ActionContext {
            pane_count: 1,
            tab_count: 1,
            ..Default::default()
        };
        let mut palette = crate::palette::PaletteState::new();
        palette.open(&reg, ctx);

        // Zero results: the panel keeps a message row (query + 1).
        for c in "close".chars() {
            palette.input_char(c, &reg, ctx);
        }
        assert!(palette.results().is_empty());
        let mut assembly = FrameAssembly::default();
        assemble_palette(&mut font, &palette, (800.0, 600.0), 0, &mut assembly);
        assert_eq!(assembly.bg_sections[1].uniforms.rows, 2);

        // A nonzero tab-bar offset shifts the panel one row below it.
        let mut assembly = FrameAssembly::default();
        assemble_palette(&mut font, &palette, (800.0, 600.0), 17, &mut assembly);
        let cell_h = font.metrics().cell_height;
        assert_eq!(assembly.bg_sections[1].uniforms.pad_top, 17.0 + cell_h);

        // A query longer than the panel width tail-anchors instead of
        // overflowing.
        for c in
            "a very long query that cannot possibly fit in sixty columns of panel width".chars()
        {
            palette.input_char(c, &reg, ctx);
        }
        let mut assembly = FrameAssembly::default();
        assemble_palette(&mut font, &palette, (800.0, 600.0), 0, &mut assembly);

        // A viewport narrower than one cell still assembles.
        let mut assembly = FrameAssembly::default();
        assemble_palette(&mut font, &palette, (20.0, 600.0), 0, &mut assembly);
        assert_eq!(assembly.bg_sections.len(), 2);
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
