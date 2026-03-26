pub mod cursor_renderer;

use anyhow::{Context, Result};
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use sdl2::render::{Canvas, TextureCreator};
use sdl2::video::{Window, WindowContext};
use video_decode::DecodedFrame;
pub use cursor_renderer::CursorRenderer;

/// SDL2 fullscreen renderer for decoded video + cursor overlay.
pub struct Renderer {
    canvas: Canvas<Window>,
    texture_creator: TextureCreator<WindowContext>,
    width: u32,
    height: u32,
    frame_count: u64,
    /// Cached last frame for cursor-only re-renders
    last_frame: Option<CachedFrame>,
}

struct CachedFrame {
    width: u32,
    height: u32,
    yuv_data: Vec<u8>,   // packed Y+U+V for IYUV texture
    pitch: usize,
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
            anyhow::bail!("Display index {} not available (have {})", display_index, num_displays);
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
            last_frame: None,
        })
    }

    /// Update with a new decoded YUV420P frame. Caches for cursor re-renders.
    pub fn update_frame(&mut self, frame: &DecodedFrame) -> Result<()> {
        let w = frame.width as usize;
        let h = frame.height as usize;
        let pitch = w; // IYUV: Y pitch = width, U/V pitch = width/2

        // Pack Y+U+V into single buffer for IYUV format
        let y_size = pitch * h;
        let uv_pitch = w / 2;
        let uv_size = uv_pitch * (h / 2);
        let total = y_size + uv_size * 2;
        let mut yuv_data = vec![0u8; total];

        // Y
        for row in 0..h {
            let src = row * frame.strides[0];
            let dst = row * pitch;
            yuv_data[dst..dst + w].copy_from_slice(&frame.planes[0][src..src + w]);
        }
        // U
        for row in 0..h / 2 {
            let src = row * frame.strides[1];
            let dst = y_size + row * uv_pitch;
            yuv_data[dst..dst + w / 2].copy_from_slice(&frame.planes[1][src..src + w / 2]);
        }
        // V
        for row in 0..h / 2 {
            let src = row * frame.strides[2];
            let dst = y_size + uv_size + row * uv_pitch;
            yuv_data[dst..dst + w / 2].copy_from_slice(&frame.planes[2][src..src + w / 2]);
        }

        self.last_frame = Some(CachedFrame {
            width: frame.width,
            height: frame.height,
            yuv_data,
            pitch,
        });
        self.frame_count += 1;
        Ok(())
    }

    /// Render cached video frame + cursor, then present.
    /// Re-uploads YUV data to texture each call (GPU upload is fast).
    pub fn present_with_cursor(&mut self, cursor: &CursorRenderer) {
        if let Some(ref cached) = self.last_frame {
            let y_size = cached.pitch * cached.height as usize;
            let uv_size = (cached.pitch / 2) * (cached.height as usize / 2);

            if let Ok(mut tex) = self.texture_creator.create_texture_streaming(
                PixelFormatEnum::IYUV, cached.width, cached.height
            ) {
                let _ = tex.update_yuv(
                    None,
                    &cached.yuv_data[..y_size],
                    cached.pitch,
                    &cached.yuv_data[y_size..y_size + uv_size],
                    cached.pitch / 2,
                    &cached.yuv_data[y_size + uv_size..],
                    cached.pitch / 2,
                );
                let dst = Rect::new(0, 0, self.width, self.height);
                let _ = self.canvas.copy(&tex, None, Some(dst));
            }
        }

        // Only draw cursor if visible (x >= 0)
        if cursor.visible && cursor.x >= 0 && cursor.y >= 0 {
            cursor.draw(&mut self.canvas);
        }
        self.canvas.present();
    }

    pub fn present(&mut self) {
        self.canvas.present();
    }

    // Legacy compatibility
    pub fn render_frame(&mut self, frame: &DecodedFrame) -> Result<()> {
        self.update_frame(frame)
    }

    pub fn frame_count(&self) -> u64 { self.frame_count }
}
