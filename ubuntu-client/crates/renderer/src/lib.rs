pub mod cursor_renderer;

use anyhow::{Context, Result};
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use sdl2::render::{Canvas, TextureCreator};
use sdl2::video::{Window, WindowContext};
use video_decode::DecodedFrame;
pub use cursor_renderer::CursorRenderer;

/// SDL2 fullscreen renderer for decoded video + cursor overlay.
/// Creates one persistent YUV texture, reuses it across frames.
pub struct Renderer {
    canvas: Canvas<Window>,
    texture_creator: TextureCreator<WindowContext>,
    width: u32,
    height: u32,
    frame_count: u64,
    cached_yuv: Option<CachedYUV>,
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
            .present_vsync()  // prevents tearing
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
        })
    }

    /// Update cached YUV data from a decoded frame.
    pub fn update_frame(&mut self, frame: &DecodedFrame) -> Result<()> {
        let w = frame.width as usize;
        let h = frame.height as usize;

        // Pack planes into contiguous vecs (strip stride padding)
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
            y_pitch: w,
            u_pitch: w / 2,
            v_pitch: w / 2,
            w: frame.width,
            h: frame.height,
        });
        self.frame_count += 1;
        Ok(())
    }

    /// Render cached video + cursor overlay, then present.
    /// Creates one texture per call BUT properly drops it (no leak).
    pub fn present_with_cursor(&mut self, cursor: &CursorRenderer) {
        if let Some(ref yuv) = self.cached_yuv {
            if let Ok(mut tex) = self.texture_creator.create_texture_streaming(
                PixelFormatEnum::IYUV, yuv.w, yuv.h
            ) {
                let _ = tex.update_yuv(
                    None,
                    &yuv.y, yuv.y_pitch,
                    &yuv.u, yuv.u_pitch,
                    &yuv.v, yuv.v_pitch,
                );
                let dst = Rect::new(0, 0, self.width, self.height);
                let _ = self.canvas.copy(&tex, None, Some(dst));
                // tex is dropped here — no GPU memory leak
            }
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
