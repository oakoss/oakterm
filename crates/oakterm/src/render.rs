//! Per-frame render/present for the GUI window: acquires the surface
//! texture, assembles the frame via [`crate::frame`], uploads glyphs to the
//! GPU atlases, and draws.

use oakterm_renderer::pipeline::{BgSection, TextUniforms};
use tracing::error;
use wgpu::CurrentSurfaceTexture;

use crate::frame::{assemble_frame, assemble_tab_bar, solid_section};
use crate::{App, layout};

/// Border colors are fixed until the theme system (TREK-212) lands.
const PANE_BORDER_RGB: [u8; 3] = [64, 64, 64];
const FOCUSED_BORDER_RGB: [u8; 3] = [92, 148, 255];

impl App {
    /// Draw and present one frame. Retries initial sizing while startup is
    /// still settling, then assembles backgrounds and glyphs for the visible
    /// panes (plus the tab bar and split borders) and submits the draw.
    // Cohesive frame sequence (acquire → assemble → upload → draw → present);
    // kept as one function, matching the sibling winit event handlers.
    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
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

        let (atlas_w, atlas_h) = self
            .font
            .as_ref()
            .map_or((256u32, 256u32), |f| f.atlas().size());
        let text_uniforms = TextUniforms {
            cell_width: self.font.as_ref().map_or(8.0, |f| f.metrics().cell_width),
            cell_height: self.font.as_ref().map_or(16.0, |f| f.metrics().cell_height),
            viewport_width: gpu.config.width as f32,
            viewport_height: gpu.config.height as f32,
            atlas_width: atlas_w as f32,
            atlas_height: atlas_h as f32,
            #[allow(clippy::cast_possible_truncation)] // gamma is small (0-5)
            text_gamma: self.config.text_gamma as f32,
            color_atlas_width: self
                .font
                .as_ref()
                .map_or(256.0, |f| f.color_atlas().size().0 as f32),
            color_atlas_height: self
                .font
                .as_ref()
                .map_or(256.0, |f| f.color_atlas().size().1 as f32),
            pad: 0.0,
        };

        let clear_color = self
            .panes
            .get(&self.focused_pane)
            .map_or(wgpu::Color::BLACK, |v| {
                let [r, g, b] = v.grid().bg_color;
                wgpu::Color {
                    r: f64::from(r) / 255.0,
                    g: f64::from(g) / 255.0,
                    b: f64::from(b) / 255.0,
                    a: 1.0,
                }
            });

        gpu.pipeline.render(
            &gpu.device,
            &gpu.queue,
            &view,
            &bg_sections,
            &text_uniforms,
            &glyph_instances,
            &gpu.atlas_view,
            &gpu.atlas_sampler,
            &gpu.color_atlas_view,
            clear_color,
        );

        if let Some(w) = &self.window {
            w.pre_present_notify();
        }
        frame.present();
    }
}
