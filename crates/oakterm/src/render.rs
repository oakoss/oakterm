//! Per-frame render/present for the GUI window: acquires the surface
//! texture, assembles the frame via [`crate::frame`], uploads glyphs to the
//! GPU atlases, and draws.

use std::time::{Duration, Instant};

use oakterm_renderer::pipeline::{BgSection, TextUniforms};
use tracing::error;
use wgpu::CurrentSurfaceTexture;

use crate::frame::{
    FontState, FrameAssembly, assemble_frame, assemble_palette, assemble_status_bar,
    assemble_tab_bar, solid_section,
};
use crate::pane_view::PaneView;
use crate::{App, layout, status_bar, tab_bar};

/// Border colors are fixed until the theme system (TREK-212) lands.
const PANE_BORDER_RGB: [u8; 3] = [64, 64, 64];
const FOCUSED_BORDER_RGB: [u8; 3] = [92, 148, 255];

impl App {
    /// Acquire the next surface texture, reconfiguring and retrying on a
    /// lost/outdated swapchain. `None` skips this frame.
    fn acquire_frame(&mut self) -> Option<wgpu::SurfaceTexture> {
        let gpu = self.gpu.as_mut()?;
        match gpu.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) | CurrentSurfaceTexture::Suboptimal(frame) => {
                Some(frame)
            }
            CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Lost => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                None
            }
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => None,
            CurrentSurfaceTexture::Validation => {
                error!("wgpu surface validation error; skipping frame");
                None
            }
        }
    }

    /// Draw and present one frame. Retries initial sizing while startup is
    /// still settling, then assembles backgrounds and glyphs for the visible
    /// panes (plus the tab bar and split borders) and submits the draw.
    #[allow(clippy::cast_precision_loss)] // viewport dimensions fit in f32
    pub(crate) fn redraw(&mut self) {
        if !self.initial_resize_sent {
            // Retries on the next RedrawRequested while font, view,
            // or daemon are still initializing.
            self.try_send_initial_resize();
        }

        // In single-pane mode the focused pane fills the content
        // area. Computed before the GPU borrow below, as is the top
        // chrome height the palette overlay offsets from.
        let Some(fallback) = self.content_rect() else {
            return;
        };
        let render_list = self.layout.visible_panes(self.focused_pane, fallback);
        let (top_chrome, _) = self.chrome_px();

        let Some(frame) = self.acquire_frame() else {
            return;
        };
        let Some(gpu) = &mut self.gpu else { return };

        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(gpu.config.format),
            ..Default::default()
        });

        let viewport = (gpu.config.width as f32, gpu.config.height as f32);
        let mut bg_sections: Vec<BgSection> = Vec::new();
        let mut glyph_instances: Vec<oakterm_renderer::pipeline::GlyphVertex> = Vec::new();
        if let Some(font) = &mut self.font {
            let mut assembly = assemble_frame(
                font,
                &self.panes,
                &render_list,
                self.focused_pane,
                self.blink_visible,
                viewport,
            );
            if self.tabs.bar_visible() {
                assemble_tab_bar(font, &self.tabs, viewport, &mut assembly);
            }
            if self.config.status_bar {
                let window_height = gpu.config.height;
                assemble_status_bar_chrome(
                    font,
                    &self.config,
                    &self.tabs,
                    self.panes.get(&self.focused_pane),
                    window_height,
                    viewport,
                    &mut assembly,
                );
            }
            gpu.upload_glyphs(font.atlas(), &assembly.uploads);
            gpu.upload_color_glyphs(font.color_atlas(), &assembly.color_uploads);
            bg_sections = assembly.bg_sections;
            glyph_instances = assembly.glyphs;
        }

        push_pane_borders(
            self.layout.active_geometry(),
            self.focused_pane,
            viewport,
            &mut bg_sections,
        );

        // The palette assembles after everything else so its panel covers
        // pane content and split borders; it also drops the pane glyphs
        // beneath it (text has no z-order).
        if self.palette.is_visible() {
            if let Some(font) = &mut self.font {
                let mut overlay = FrameAssembly {
                    glyphs: std::mem::take(&mut glyph_instances),
                    ..Default::default()
                };
                // Full top chrome, so the palette clears a top status bar.
                assemble_palette(font, &self.palette, viewport, top_chrome, &mut overlay);
                gpu.upload_glyphs(font.atlas(), &overlay.uploads);
                gpu.upload_color_glyphs(font.color_atlas(), &overlay.color_uploads);
                bg_sections.extend(overlay.bg_sections);
                glyph_instances = overlay.glyphs;
            }
        }

        let uniforms = text_uniforms(self.font.as_ref(), self.config.text_gamma, viewport);
        let clear = self
            .panes
            .get(&self.focused_pane)
            .map_or(wgpu::Color::BLACK, |v| clear_color(v.grid().bg_color));

        gpu.pipeline.render(
            &gpu.device,
            &gpu.queue,
            &view,
            &bg_sections,
            &uniforms,
            &glyph_instances,
            &gpu.atlas_view,
            &gpu.atlas_sampler,
            &gpu.color_atlas_view,
            clear,
        );

        if let Some(w) = &self.window {
            w.pre_present_notify();
        }
        frame.present();

        // Re-arm the minute repaint after each frame that shows a clock;
        // firing clears the deadline so drift never accumulates.
        if self.config.status_bar && self.clock_deadline.is_none() {
            let deadline =
                Instant::now() + Duration::from_secs(status_bar::seconds_to_next_minute());
            tracing::trace!(?deadline, "status bar clock repaint armed");
            self.clock_deadline = Some(deadline);
        }
    }
}

/// Pane borders; segments adjacent to the focused pane get the
/// highlight color.
fn push_pane_borders(
    geo: Option<&layout::LayoutGeometry>,
    focused_pane: u32,
    viewport: (f32, f32),
    bg_sections: &mut Vec<BgSection>,
) {
    let Some(geo) = geo else { return };
    let focused = layout::focused_border_indices(geo, focused_pane);
    for (i, border) in geo.borders.iter().enumerate() {
        let rgb = if focused.contains(&i) {
            FOCUSED_BORDER_RGB
        } else {
            PANE_BORDER_RGB
        };
        bg_sections.push(solid_section(*border, rgb, viewport));
    }
}

/// Assemble the status bar at its configured edge: bottom of the window,
/// or directly below the tab bar for `status_bar_position = "top"`.
fn assemble_status_bar_chrome(
    font: &mut FontState,
    config: &oakterm_config::ConfigValues,
    tabs: &tab_bar::TabsState,
    focused: Option<&PaneView>,
    window_height: u32,
    viewport: (f32, f32),
    assembly: &mut FrameAssembly,
) {
    let metrics = *font.metrics();
    let bar_h = status_bar::status_bar_height(true, Some(&metrics));
    let y = match config.status_bar_position {
        oakterm_config::StatusBarPosition::Bottom => window_height.saturating_sub(bar_h),
        oakterm_config::StatusBarPosition::Top => {
            tab_bar::tab_bar_height(tabs.bar_visible(), Some(&metrics))
        }
    };
    let clock = status_bar::clock_text();
    let content = status_bar::StatusContent {
        // The one sign a pane is in copy mode until the cursor and
        // selection render (TREK-114): without it the view freezes and
        // the keyboard changes meaning with nothing saying so.
        mode: focused
            .is_some_and(PaneView::is_copy_mode)
            .then_some("COPY"),
        workspace: tabs.workspace_name(),
        tabs: tabs.tabs(),
        active_tab: tabs.active_tab(),
        pane_title: focused.map_or("", |v| v.title.as_str()),
        clock: &clock,
    };
    assemble_status_bar(font, &content, viewport, y, assembly);
}

/// GPU text-shading uniforms from the active font, or safe placeholder
/// defaults before a font (and thus any atlas) exists.
#[allow(clippy::cast_precision_loss)] // atlas/cell dims fit in f32
fn text_uniforms(font: Option<&FontState>, text_gamma: f64, viewport: (f32, f32)) -> TextUniforms {
    let metrics = font.map(FontState::metrics);
    let (atlas_w, atlas_h) = font.map_or((256u32, 256u32), |f| f.atlas().size());
    let (color_w, color_h) = font.map_or((256u32, 256u32), |f| f.color_atlas().size());
    TextUniforms {
        cell_width: metrics.map_or(8.0, |m| m.cell_width),
        cell_height: metrics.map_or(16.0, |m| m.cell_height),
        viewport_width: viewport.0,
        viewport_height: viewport.1,
        atlas_width: atlas_w as f32,
        atlas_height: atlas_h as f32,
        #[allow(clippy::cast_possible_truncation)] // gamma is small (0-5)
        text_gamma: text_gamma as f32,
        color_atlas_width: color_w as f32,
        color_atlas_height: color_h as f32,
        pad: 0.0,
    }
}

fn clear_color(grid_bg: [u8; 3]) -> wgpu::Color {
    let [r, g, b] = grid_bg;
    wgpu::Color {
        r: f64::from(r) / 255.0,
        g: f64::from(g) / 255.0,
        b: f64::from(b) / 255.0,
        a: 1.0,
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss)] // exact defaults, /255, and u32→f32 dims
mod tests {
    use super::{PaneView, assemble_status_bar_chrome, clear_color, text_uniforms};
    use crate::frame::FrameAssembly;
    use crate::render_grid::ClientGrid;

    /// Copy mode's only on-screen sign until the cursor and selection
    /// render (TREK-114). The status bar layer is tested on its own, so
    /// what this pins is the wiring: copy mode reaches the mode slot,
    /// and normal mode leaves it empty.
    #[test]
    fn copy_mode_puts_its_indicator_in_the_status_bar() {
        let Ok(mut font) =
            crate::frame::try_init_font(&oakterm_config::ConfigValues::default(), 14.0)
        else {
            return;
        };
        let config = oakterm_config::ConfigValues::default();
        let tabs = crate::tab_bar::TabsState::default();
        let viewport = (1600.0, 600.0);

        let mut pane = PaneView::new(ClientGrid::new(80, 24));
        pane.title = "~/project".to_string();

        let glyphs = |font: &mut crate::frame::FontState, pane: &PaneView| {
            let mut assembly = FrameAssembly::default();
            assemble_status_bar_chrome(
                font,
                &config,
                &tabs,
                Some(pane),
                600,
                viewport,
                &mut assembly,
            );
            assembly.glyphs.len()
        };

        let normal = glyphs(&mut font, &pane);
        pane.enter_copy_mode();
        let copy = glyphs(&mut font, &pane);

        assert_eq!(copy - normal, "[COPY]".len(), "the indicator's six cells");
    }

    #[test]
    fn text_uniforms_uses_safe_defaults_without_a_font() {
        let u = text_uniforms(None, 1.5, (800.0, 600.0));
        assert_eq!(u.cell_width, 8.0);
        assert_eq!(u.cell_height, 16.0);
        assert_eq!(u.viewport_width, 800.0);
        assert_eq!(u.viewport_height, 600.0);
        assert_eq!(u.atlas_width, 256.0);
        assert_eq!(u.color_atlas_height, 256.0);
        assert_eq!(u.text_gamma, 1.5);
    }

    #[test]
    fn text_uniforms_reads_dimensions_from_the_active_font() {
        // Guards the six adjacent `_ as f32` mappings against a width/height
        // or atlas/color-atlas transposition the None-branch test can't catch.
        let Ok(font) = crate::frame::try_init_font(&oakterm_config::ConfigValues::default(), 14.0)
        else {
            return;
        };
        let u = text_uniforms(Some(&font), 2.0, (800.0, 600.0));
        assert_eq!(u.cell_width, font.metrics().cell_width);
        assert_eq!(u.cell_height, font.metrics().cell_height);
        assert_eq!(u.atlas_width, font.atlas().size().0 as f32);
        assert_eq!(u.atlas_height, font.atlas().size().1 as f32);
        assert_eq!(u.color_atlas_width, font.color_atlas().size().0 as f32);
        assert_eq!(u.color_atlas_height, font.color_atlas().size().1 as f32);
        assert_eq!(u.text_gamma, 2.0);
    }

    #[test]
    fn clear_color_normalizes_channels() {
        let c = clear_color([255, 128, 0]);
        assert_eq!(c.r, 1.0);
        assert_eq!(c.g, f64::from(128u8) / 255.0);
        assert_eq!(c.b, 0.0);
        assert_eq!(c.a, 1.0);
    }
}
