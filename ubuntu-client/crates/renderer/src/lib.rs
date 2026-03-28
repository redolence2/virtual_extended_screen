pub mod cursor_renderer;

use anyhow::{Context, Result};
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use sdl2::render::{Canvas, TextureCreator};
use sdl2::video::{Window, WindowContext};
use video_decode::DecodedFrame;
pub use cursor_renderer::CursorRenderer;

/// SDL2 fullscreen renderer with persistent texture cache.
/// Texture is only recreated when frame dimensions change.
pub struct Renderer {
    canvas: Canvas<Window>,
    texture_creator: TextureCreator<WindowContext>,
    width: u32,
    height: u32,
    frame_count: u64,
    cached_yuv: Option<CachedYUV>,
    persistent_tex: Option<PersistentTexture>,
    /// Night Shift warm filter strength: 0.0=off, 1.0=max warm.
    pub warm_strength: f32,
}

struct CachedYUV {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    y_pitch: usize,
    u_pitch: usize,
    v_pitch: usize,
    w: u32,
    h: u32,
}

/// Wraps a Texture with its dimensions for cache invalidation.
struct PersistentTexture {
    // SAFETY: texture_creator outlives this texture (both in Renderer).
    // We use raw pointer to avoid SDL2 lifetime constraints.
    tex_ptr: *mut sdl2::sys::SDL_Texture,
    w: u32,
    h: u32,
}

impl PersistentTexture {
    fn new(tc: &TextureCreator<WindowContext>, w: u32, h: u32) -> Option<Self> {
        let tex = tc.create_texture_streaming(PixelFormatEnum::IYUV, w, h).ok()?;
        let raw = tex.raw();
        std::mem::forget(tex); // prevent Drop; we manage lifetime manually
        Some(Self { tex_ptr: raw, w, h })
    }

    fn update_and_copy(&mut self, canvas: &mut Canvas<Window>, yuv: &CachedYUV, canvas_w: u32, canvas_h: u32) {
        unsafe {
            sdl2::sys::SDL_UpdateYUVTexture(
                self.tex_ptr,
                std::ptr::null(),
                yuv.y.as_ptr(), yuv.y_pitch as i32,
                yuv.u.as_ptr(), yuv.u_pitch as i32,
                yuv.v.as_ptr(), yuv.v_pitch as i32,
            );

            let stream_portrait = self.h > self.w;
            let canvas_portrait = canvas_h > canvas_w;

            if stream_portrait != canvas_portrait {
                // Orientation mismatch: SDL2 fullscreen bypasses xrandr rotation.
                // After -90° rotation, texture dimensions effectively swap (w↔h).
                // Scale so the rotated content fits the canvas (handles 4K stream on 1080p monitor).
                let scale_x = canvas_w as f64 / self.h as f64; // post-rotation width = tex height
                let scale_y = canvas_h as f64 / self.w as f64; // post-rotation height = tex width
                let scale = scale_x.min(scale_y);
                let dst_w = (self.w as f64 * scale) as i32;
                let dst_h = (self.h as f64 * scale) as i32;
                let dx = (canvas_w as i32 - dst_w) / 2;
                let dy = (canvas_h as i32 - dst_h) / 2;
                let dst = sdl2::sys::SDL_Rect {
                    x: dx, y: dy, w: dst_w, h: dst_h,
                };
                sdl2::sys::SDL_RenderCopyEx(
                    canvas.raw(),
                    self.tex_ptr,
                    std::ptr::null(),
                    &dst,
                    -90.0,
                    std::ptr::null(),
                    sdl2::sys::SDL_RendererFlip::SDL_FLIP_NONE,
                );
            } else {
                // Orientations match — render normally
                let dst = sdl2::sys::SDL_Rect {
                    x: 0, y: 0, w: canvas_w as i32, h: canvas_h as i32,
                };
                sdl2::sys::SDL_RenderCopy(
                    canvas.raw(),
                    self.tex_ptr,
                    std::ptr::null(),
                    &dst,
                );
            }
        }
    }
}

impl Drop for PersistentTexture {
    fn drop(&mut self) {
        unsafe { sdl2::sys::SDL_DestroyTexture(self.tex_ptr); }
    }
}

// SAFETY: Renderer is only used from one thread (decode-render).
unsafe impl Send for Renderer {}

impl Renderer {
    pub fn new(display_index: i32, width: u32, height: u32, flash_test: bool) -> Result<Self> {
        let sdl = sdl2::init().map_err(|e| anyhow::anyhow!("SDL init: {}", e))?;
        let video = sdl.video().map_err(|e| anyhow::anyhow!("SDL video: {}", e))?;

        let num_displays = video.num_video_displays()
            .map_err(|e| anyhow::anyhow!("num_displays: {}", e))?;
        log::info!("SDL2 displays: {}", num_displays);
        for i in 0..num_displays {
            if let Ok(name) = video.display_name(i) {
                if let Ok(bounds) = video.display_bounds(i) {
                    log::info!("  Display {}: '{}' at {}x{}+{}+{}", i, name,
                              bounds.width(), bounds.height(), bounds.x(), bounds.y());
                }
            }
        }

        if display_index >= num_displays {
            anyhow::bail!("Display {} not available (have {})", display_index, num_displays);
        }

        let bounds = video.display_bounds(display_index)
            .map_err(|e| anyhow::anyhow!("display_bounds: {}", e))?;

        let window = video.window("RESC Receiver", bounds.width(), bounds.height())
            .position(bounds.x(), bounds.y())
            .fullscreen_desktop()
            .build()
            .context("Failed to create SDL window")?;

        let mut canvas = window.into_canvas()
            .accelerated()
            .present_vsync()
            .build()
            .context("Failed to create SDL canvas")?;

        sdl.mouse().show_cursor(false);

        if flash_test {
            log::info!("Flash test on display {} for 2 seconds...", display_index);
            canvas.set_draw_color(sdl2::pixels::Color::RGB(0, 100, 255));
            canvas.clear();
            canvas.present();
            std::thread::sleep(std::time::Duration::from_secs(2));
        }

        canvas.set_draw_color(sdl2::pixels::Color::RGB(0, 0, 0));
        canvas.clear();
        canvas.present();

        let texture_creator = canvas.texture_creator();
        log::info!("Renderer ready on display {} ({}x{})", display_index, width, height);

        Ok(Self {
            canvas,
            texture_creator,
            width,
            height,
            frame_count: 0,
            cached_yuv: None,
            persistent_tex: None,
            warm_strength: 0.0,
        })
    }

    /// Update cached YUV data from a decoded frame.
    pub fn update_frame(&mut self, frame: &DecodedFrame) -> Result<()> {
        let w = frame.width as usize;
        let h = frame.height as usize;

        let mut y = vec![0u8; w * h];
        let mut u = vec![0u8; (w / 2) * (h / 2)];
        let mut v = vec![0u8; (w / 2) * (h / 2)];

        for row in 0..h {
            let src = row * frame.strides[0];
            let dst = row * w;
            y[dst..dst + w].copy_from_slice(&frame.planes[0][src..src + w]);
        }
        for row in 0..h / 2 {
            let src = row * frame.strides[1];
            let dst = row * (w / 2);
            u[dst..dst + w / 2].copy_from_slice(&frame.planes[1][src..src + w / 2]);
        }
        for row in 0..h / 2 {
            let src = row * frame.strides[2];
            let dst = row * (w / 2);
            v[dst..dst + w / 2].copy_from_slice(&frame.planes[2][src..src + w / 2]);
        }

        // Apply warm filter (Night Shift) by shifting UV chrominance
        if self.warm_strength > 0.0 {
            let s = self.warm_strength;
            let u_shift = (-20.0 * s) as i16; // less blue
            let v_shift = (15.0 * s) as i16;  // more red
            for val in u.iter_mut() {
                *val = (*val as i16 + u_shift).clamp(0, 255) as u8;
            }
            for val in v.iter_mut() {
                *val = (*val as i16 + v_shift).clamp(0, 255) as u8;
            }
        }

        self.cached_yuv = Some(CachedYUV {
            y, u, v,
            y_pitch: w, u_pitch: w / 2, v_pitch: w / 2,
            w: frame.width, h: frame.height,
        });

        // Recreate persistent texture only if dimensions changed
        let need_new_tex = match &self.persistent_tex {
            Some(t) => t.w != frame.width || t.h != frame.height,
            None => true,
        };
        if need_new_tex {
            self.persistent_tex = PersistentTexture::new(
                &self.texture_creator, frame.width, frame.height
            );
            log::info!("Texture cache: created {}x{}", frame.width, frame.height);
        }

        self.frame_count += 1;
        Ok(())
    }

    /// Check if stream/canvas orientation mismatch requires rotation.
    /// Uses stream dimensions (self.width/height) — works before first frame arrives.
    pub fn is_rotated(&self) -> bool {
        let (cw, ch) = self.canvas.output_size().unwrap_or((self.width, self.height));
        let stream_portrait = self.height > self.width;
        let canvas_portrait = ch > cw;
        stream_portrait != canvas_portrait
    }

    /// Compute the rotation scale factor (stream → canvas after rotation).
    fn rotation_scale(&self) -> f64 {
        let (cw, ch) = self.canvas.output_size().unwrap_or((self.width, self.height));
        let scale_x = cw as f64 / self.height as f64;
        let scale_y = ch as f64 / self.width as f64;
        scale_x.min(scale_y)
    }

    /// Render cached video + cursor overlay via persistent texture.
    pub fn present_with_cursor(&mut self, cursor: &CursorRenderer) {
        let rotated = self.is_rotated();

        if let (Some(ref yuv), Some(ref mut tex)) = (&self.cached_yuv, &mut self.persistent_tex) {
            let (cw, ch) = self.canvas.output_size().unwrap_or((self.width, self.height));
            tex.update_and_copy(&mut self.canvas, yuv, cw, ch);
        }

        if cursor.visible && cursor.x >= 0 && cursor.y >= 0 {
            if rotated {
                let scale = self.rotation_scale();
                let (cw, ch) = self.canvas.output_size().unwrap_or((self.width, self.height));
                // After -90° rotation, effective: (sy, stream_w - sx), then scale + center
                let rx = (cursor.y as f64 * scale) as i32 + (cw as i32 - (self.height as f64 * scale) as i32) / 2;
                let ry = ((self.width as i32 - 1 - cursor.x) as f64 * scale) as i32 + (ch as i32 - (self.width as f64 * scale) as i32) / 2;
                let mut rotated_cursor = cursor.clone();
                rotated_cursor.x = rx;
                rotated_cursor.y = ry;
                rotated_cursor.draw(&mut self.canvas);
            } else {
                // Scale cursor for non-rotated resolution mismatch
                let (cw, ch) = self.canvas.output_size().unwrap_or((self.width, self.height));
                if cw != self.width || ch != self.height {
                    let mut scaled_cursor = cursor.clone();
                    scaled_cursor.x = (cursor.x as f64 * cw as f64 / self.width as f64) as i32;
                    scaled_cursor.y = (cursor.y as f64 * ch as f64 / self.height as f64) as i32;
                    scaled_cursor.draw(&mut self.canvas);
                } else {
                    cursor.draw(&mut self.canvas);
                }
            }
        }
        self.canvas.present();
    }

    pub fn present(&mut self) {
        self.canvas.present();
    }

    pub fn render_frame(&mut self, frame: &DecodedFrame) -> Result<()> {
        self.update_frame(frame)
    }

    /// Get the actual canvas output size (physical resolution).
    pub fn canvas_size(&self) -> (u32, u32) {
        self.canvas.output_size().unwrap_or((self.width, self.height))
    }

    pub fn frame_count(&self) -> u64 { self.frame_count }
}
