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
    /// Persistent texture — recreated only on dimension change (Item 8 from review)
    persistent_tex: Option<PersistentTexture>,
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

    fn update_and_copy(&mut self, canvas: &mut Canvas<Window>, yuv: &CachedYUV, dst: Rect) {
        unsafe {
            // Update YUV planes directly via SDL2 C API
            sdl2::sys::SDL_UpdateYUVTexture(
                self.tex_ptr,
                std::ptr::null(), // entire texture
                yuv.y.as_ptr(), yuv.y_pitch as i32,
                yuv.u.as_ptr(), yuv.u_pitch as i32,
                yuv.v.as_ptr(), yuv.v_pitch as i32,
            );
            sdl2::sys::SDL_RenderCopy(
                canvas.raw(),
                self.tex_ptr,
                std::ptr::null(), // src: entire texture
                &sdl2::sys::SDL_Rect { x: dst.x(), y: dst.y(), w: dst.width() as i32, h: dst.height() as i32 },
            );
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

    /// Render cached video + cursor overlay via persistent texture.
    pub fn present_with_cursor(&mut self, cursor: &CursorRenderer) {
        if let (Some(ref yuv), Some(ref mut tex)) = (&self.cached_yuv, &mut self.persistent_tex) {
            let (cw, ch) = self.canvas.output_size().unwrap_or((self.width, self.height));
            let dst = Rect::new(0, 0, cw, ch);
            tex.update_and_copy(&mut self.canvas, yuv, dst);
        }

        if cursor.visible && cursor.x >= 0 && cursor.y >= 0 {
            cursor.draw(&mut self.canvas);
        }
        self.canvas.present();
    }

    pub fn present(&mut self) {
        self.canvas.present();
    }

    pub fn render_frame(&mut self, frame: &DecodedFrame) -> Result<()> {
        self.update_frame(frame)
    }

    pub fn frame_count(&self) -> u64 { self.frame_count }
}
