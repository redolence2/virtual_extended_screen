pub mod cursor_renderer;

use anyhow::{Context, Result};
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use sdl2::render::{Canvas, TextureCreator};
use sdl2::video::{Window, WindowContext};
use video_decode::{DecodedFrame, FrameFormat};
pub use cursor_renderer::CursorRenderer;

/// SDL2 fullscreen renderer with a RESIDENT texture: planes are uploaded
/// exactly once per new video frame (`update_frame`); cursor-only redraws
/// re-blit the resident texture (renderer-cleanup review §3 — the old path
/// re-ran the full 4K SDL_UpdateYUVTexture on every present).
pub struct Renderer {
    canvas: Canvas<Window>,
    texture_creator: TextureCreator<WindowContext>,
    width: u32,
    height: u32,
    frame_count: u64,
    persistent_tex: Option<PersistentTexture>,
    /// Reused UV scratch for the warm filter (allocated once per size; only
    /// touched when `warm_strength > 0` — the warm==0 path uploads the
    /// decoded planes directly with zero renderer-side copies).
    warm_scratch: Vec<u8>,
    // Stage timers (vsync-model falsification follow-up): name the owner of
    // the decode→present segment by measurement. Logged every 300 uploads.
    upload_us_sum: u64,
    upload_n: u64,
    present_us_sum: u64,
    present_n: u64,
    /// Night Shift warm filter strength: 0.0=off, 1.0=max warm.
    pub warm_strength: f32,
}

/// Wraps a Texture with its dimensions/format for cache invalidation.
struct PersistentTexture {
    // SAFETY: texture_creator outlives this texture (both in Renderer).
    // We use raw pointer to avoid SDL2 lifetime constraints.
    tex_ptr: *mut sdl2::sys::SDL_Texture,
    w: u32,
    h: u32,
    fmt: PixelFormatEnum,
}

impl PersistentTexture {
    fn new(tc: &TextureCreator<WindowContext>, w: u32, h: u32, fmt: PixelFormatEnum) -> Option<Self> {
        let tex = tc.create_texture_streaming(fmt, w, h).ok()?;
        let raw = tex.raw();
        std::mem::forget(tex); // prevent Drop; we manage lifetime manually
        Some(Self { tex_ptr: raw, w, h, fmt })
    }

    /// Blit the resident texture to the canvas (rotation-aware). No upload
    /// happens here — that is `Renderer::update_frame`'s job, once per frame.
    fn blit(&self, canvas: &mut Canvas<Window>, canvas_w: u32, canvas_h: u32) {
        unsafe {
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

        let mut builder = window.into_canvas().accelerated();
        // Diagnostic switch (vsync-model falsification, ladder-day record):
        // RESC_NO_VSYNC=1 drops present_vsync so presents never wait for
        // vblank. Tearing expected — measurement-only, never the default.
        let no_vsync = std::env::var("RESC_NO_VSYNC").map(|v| v == "1").unwrap_or(false);
        if no_vsync {
            log::warn!("DIAGNOSTIC: present_vsync DISABLED (RESC_NO_VSYNC=1) — tearing expected");
        } else {
            builder = builder.present_vsync();
        }
        let mut canvas = builder
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
            persistent_tex: None,
            warm_scratch: Vec::new(),
            upload_us_sum: 0,
            upload_n: 0,
            present_us_sum: 0,
            present_n: 0,
            warm_strength: 0.0,
        })
    }

    /// Upload one new decoded frame into the resident texture. This is the
    /// ONLY place planes are uploaded; cursor-only redraws re-blit without
    /// touching pixel data. warm==0 uploads the decoder's planes directly
    /// (zero renderer-side copies); warm>0 shifts chroma through the reused
    /// scratch buffer (equivalent UV adjustment to the old baked-in filter).
    pub fn update_frame(&mut self, frame: &DecodedFrame) -> Result<()> {
        let fmt = match frame.format {
            FrameFormat::Nv12 => PixelFormatEnum::NV12,
            FrameFormat::I420 => PixelFormatEnum::IYUV,
        };
        let need_new_tex = match &self.persistent_tex {
            Some(t) => t.w != frame.width || t.h != frame.height || t.fmt != fmt,
            None => true,
        };
        if need_new_tex {
            self.persistent_tex = PersistentTexture::new(
                &self.texture_creator, frame.width, frame.height, fmt
            );
            log::info!("Texture cache: created {}x{} {:?}", frame.width, frame.height, fmt);
        }
        let tex_ptr = match &self.persistent_tex {
            Some(t) => t.tex_ptr,
            None => return Err(anyhow::anyhow!("texture creation failed")),
        };

        let s = self.warm_strength;
        let u_shift = (-20.0 * s) as i16; // less blue
        let v_shift = (15.0 * s) as i16;  // more red

        let upload_start = std::time::Instant::now();
        let ret = match frame.format {
            FrameFormat::Nv12 => {
                let uv_ptr: *const u8 = if s > 0.0 {
                    let src = &frame.planes[1];
                    self.warm_scratch.resize(src.len(), 0);
                    self.warm_scratch.copy_from_slice(src);
                    for pair in self.warm_scratch.chunks_exact_mut(2) {
                        pair[0] = (pair[0] as i16 + u_shift).clamp(0, 255) as u8;
                        pair[1] = (pair[1] as i16 + v_shift).clamp(0, 255) as u8;
                    }
                    self.warm_scratch.as_ptr()
                } else {
                    frame.planes[1].as_ptr()
                };
                unsafe {
                    sdl2::sys::SDL_UpdateNVTexture(
                        tex_ptr,
                        std::ptr::null(),
                        frame.planes[0].as_ptr(), frame.strides[0] as i32,
                        uv_ptr, frame.strides[1] as i32,
                    )
                }
            }
            FrameFormat::I420 => {
                let (u_ptr, v_ptr): (*const u8, *const u8) = if s > 0.0 {
                    let ulen = frame.planes[1].len();
                    let vlen = frame.planes[2].len();
                    self.warm_scratch.resize(ulen + vlen, 0);
                    self.warm_scratch[..ulen].copy_from_slice(&frame.planes[1]);
                    self.warm_scratch[ulen..].copy_from_slice(&frame.planes[2]);
                    for b in self.warm_scratch[..ulen].iter_mut() {
                        *b = (*b as i16 + u_shift).clamp(0, 255) as u8;
                    }
                    for b in self.warm_scratch[ulen..].iter_mut() {
                        *b = (*b as i16 + v_shift).clamp(0, 255) as u8;
                    }
                    (
                        self.warm_scratch.as_ptr(),
                        self.warm_scratch[ulen..].as_ptr(),
                    )
                } else {
                    (frame.planes[1].as_ptr(), frame.planes[2].as_ptr())
                };
                unsafe {
                    sdl2::sys::SDL_UpdateYUVTexture(
                        tex_ptr,
                        std::ptr::null(),
                        frame.planes[0].as_ptr(), frame.strides[0] as i32,
                        u_ptr, frame.strides[1] as i32,
                        v_ptr, frame.strides[2] as i32,
                    )
                }
            }
        };
        if ret != 0 {
            return Err(anyhow::anyhow!("SDL texture update failed: {}", sdl2::get_error()));
        }
        self.upload_us_sum += upload_start.elapsed().as_micros() as u64;
        self.upload_n += 1;
        if self.upload_n % 300 == 0 && self.upload_n > 0 && self.present_n > 0 {
            log::info!(
                "Render stages: upload avg {:.1}ms (n={}), blit+present avg {:.1}ms (n={})",
                self.upload_us_sum as f64 / self.upload_n as f64 / 1000.0,
                self.upload_n,
                self.present_us_sum as f64 / self.present_n as f64 / 1000.0,
                self.present_n
            );
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
        let present_start = std::time::Instant::now();
        let rotated = self.is_rotated();

        let (cw, ch) = self.canvas.output_size().unwrap_or((self.width, self.height));
        if let Some(ref tex) = self.persistent_tex {
            tex.blit(&mut self.canvas, cw, ch);
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
        self.present_us_sum += present_start.elapsed().as_micros() as u64;
        self.present_n += 1;
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
