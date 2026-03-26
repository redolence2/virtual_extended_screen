pub mod cursor_renderer;

use anyhow::{Context, Result};
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use sdl2::render::{Canvas, Texture, TextureCreator};
use sdl2::video::{Window, WindowContext};
use video_decode::DecodedFrame;
pub use cursor_renderer::CursorRenderer;

/// SDL2 fullscreen renderer for displaying decoded video frames + cursor.
pub struct Renderer {
    canvas: Canvas<Window>,
    texture_creator: TextureCreator<WindowContext>,
    /// Persistent video texture (updated when new frame arrives)
    video_texture: Option<Texture>,
    width: u32,
    height: u32,
    frame_count: u64,
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
            video_texture: None,
            width,
            height,
            frame_count: 0,
        })
    }

    /// Update the video texture with a new decoded YUV420P frame.
    pub fn update_frame(&mut self, frame: &DecodedFrame) -> Result<()> {
        // Create or recreate texture if dimensions changed
        let need_new = match &self.video_texture {
            None => true,
            Some(_) => false, // reuse existing
        };

        if need_new {
            let tex = self.texture_creator
                .create_texture_streaming(PixelFormatEnum::IYUV, frame.width, frame.height)
                .context("Failed to create YUV texture")?;
            self.video_texture = Some(tex);
        }

        let texture = self.video_texture.as_mut().unwrap();

        texture.with_lock(None, |buffer: &mut [u8], pitch: usize| {
            let h = frame.height as usize;
            let half_h = h / 2;

            for row in 0..h {
                let src_start = row * frame.strides[0];
                let src_end = src_start + frame.width as usize;
                let dst_start = row * pitch;
                buffer[dst_start..dst_start + frame.width as usize]
                    .copy_from_slice(&frame.planes[0][src_start..src_end]);
            }

            let u_offset = pitch * h;
            let u_pitch = pitch / 2;
            for row in 0..half_h {
                let src_start = row * frame.strides[1];
                let src_end = src_start + (frame.width as usize / 2);
                let dst_start = u_offset + row * u_pitch;
                buffer[dst_start..dst_start + (frame.width as usize / 2)]
                    .copy_from_slice(&frame.planes[1][src_start..src_end]);
            }

            let v_offset = u_offset + u_pitch * half_h;
            for row in 0..half_h {
                let src_start = row * frame.strides[2];
                let src_end = src_start + (frame.width as usize / 2);
                let dst_start = v_offset + row * u_pitch;
                buffer[dst_start..dst_start + (frame.width as usize / 2)]
                    .copy_from_slice(&frame.planes[2][src_start..src_end]);
            }
        }).map_err(|e| anyhow::anyhow!("texture lock: {}", e))?;

        self.frame_count += 1;
        Ok(())
    }

    /// Render the current video texture + cursor overlay, then present.
    /// Can be called at high frequency (120Hz) — just re-composites existing texture + cursor.
    pub fn present_with_cursor(&mut self, cursor: &CursorRenderer) {
        // Draw video texture
        if let Some(ref texture) = self.video_texture {
            let dst = Rect::new(0, 0, self.width, self.height);
            let _ = self.canvas.copy(texture, None, Some(dst));
        }

        // Draw cursor on top
        cursor.draw(&mut self.canvas);

        self.canvas.present();
    }

    /// Render video texture without cursor.
    pub fn present(&mut self) {
        if let Some(ref texture) = self.video_texture {
            let dst = Rect::new(0, 0, self.width, self.height);
            let _ = self.canvas.copy(texture, None, Some(dst));
        }
        self.canvas.present();
    }

    // Legacy method kept for compatibility
    pub fn render_frame(&mut self, frame: &DecodedFrame) -> Result<()> {
        self.update_frame(frame)
    }

    pub fn frame_count(&self) -> u64 { self.frame_count }
}
