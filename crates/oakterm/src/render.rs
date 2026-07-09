//! Per-frame render/present for the GUI window: acquires the surface
//! texture, assembles the frame via [`crate::frame`], uploads glyphs to the
//! GPU atlases, and draws.

use oakterm_renderer::pipeline::{BgSection, TextUniforms};
use tracing::error;
use wgpu::CurrentSurfaceTexture;

use crate::frame::{FontState, assemble_frame, assemble_tab_bar, solid_section};
use crate::{App, layout};

/// Border colors are fixed until the theme system (TREK-212) lands.
const PANE_BORDER_RGB: [u8; 3] = [64, 64, 64];
const FOCUSED_BORDER_RGB: [u8; 3] = [92, 148, 255];

impl App {
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
        // area. Computed before the GPU borrow below.
        let Some(fallback) = self.content_rect() else {
            return;
        };
        let render_list = self.layout.visible_panes(self.focused_pane, fallback);

        let Some(gpu) = &mut self.gpu else { return };
        let frame = match gpu.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) | CurrentSurfaceTexture::Suboptimal(frame) => {
                frame
            }
            CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Lost => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => return,
            CurrentSurfaceTexture::Validation => {
                error!("wgpu surface validation error; skipping frame");
                return;
            }
        };

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
            gpu.upload_glyphs(font.atlas(), &assembly.uploads);
            gpu.upload_color_glyphs(font.color_atlas(), &assembly.color_uploads);
            bg_sections = assembly.bg_sections;
            glyph_instances = assembly.glyphs;
        }

        // Pane borders; segments adjacent to the focused pane get
        // the highlight color.
        if let Some(geo) = self.layout.active_geometry() {
            let focused = layout::focused_border_indices(geo, self.focused_pane);
            for (i, border) in geo.borders.iter().enumerate() {
                let rgb = if focused.contains(&i) {
                    FOCUSED_BORDER_RGB
                } else {
                    PANE_BORDER_RGB
                };
                bg_sections.push(solid_section(*border, rgb, viewport));
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
    }
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
    use super::{clear_color, text_uniforms};

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
