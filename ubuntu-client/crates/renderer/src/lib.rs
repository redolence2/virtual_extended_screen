use anyhow::{Context, Result};
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use sdl2::render::{Canvas, TextureCreator};
use sdl2::video::{Window, WindowContext};
use video_decode::DecodedFrame;

/// SDL2 fullscreen renderer for displaying decoded video frames.
pub struct Renderer {
    canvas: Canvas<Window>,
    texture_creator: TextureCreator<WindowContext>,
    width: u32,
    height: u32,
    frame_count: u64,
}

impl Renderer {
    /// Create a fullscreen window on the specified display index.
    /// Runs a 2-second flash test (colored screen) to confirm correct monitor.
    pub fn new(display_index: i32, width: u32, height: u32, flash_test: bool) -> Result<Self> {
        let sdl = sdl2::init().map_err(|e| anyhow::anyhow!("SDL init: {}", e))?;
        let video = sdl.video().map_err(|e| anyhow::anyhow!("SDL video: {}", e))?;

        // Log available displays
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

        // Get target display bounds
        let bounds = video.display_bounds(display_index)
            .map_err(|e| anyhow::anyhow!("display_bounds: {}", e))?;

        // Create window on the target display
        let window = video.window("RESC Receiver", bounds.width(), bounds.height())
            .position(bounds.x(), bounds.y())
            .borderless()
            .build()
            .context("Failed to create SDL window")?;

        let mut canvas = window.into_canvas()
            .accelerated()
            .present_vsync()
            .build()
            .context("Failed to create SDL canvas")?;

        // Hide system cursor
        sdl.mouse().show_cursor(false);

        // Flash test: show colored screen for 2s
        if flash_test {
            log::info!("Flash test on display {} for 2 seconds...", display_index);
            canvas.set_draw_color(sdl2::pixels::Color::RGB(0, 100, 255));
            canvas.clear();
            canvas.present();
            std::thread::sleep(std::time::Duration::from_secs(2));
        }

        // Clear to black
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
        })
    }

    /// Render a decoded YUV420P frame.
    pub fn render_frame(&mut self, frame: &DecodedFrame) -> Result<()> {
        let mut texture = self.texture_creator
            .create_texture_streaming(PixelFormatEnum::IYUV, frame.width, frame.height)
            .context("Failed to create YUV texture")?;

        texture.with_lock(None, |buffer: &mut [u8], pitch: usize| {
            let h = frame.height as usize;
            let half_h = h / 2;

            // Y plane
            for row in 0..h {
                let src_start = row * frame.strides[0];
                let src_end = src_start + frame.width as usize;
                let dst_start = row * pitch;
                buffer[dst_start..dst_start + frame.width as usize]
                    .copy_from_slice(&frame.planes[0][src_start..src_end]);
            }

            // U plane
            let u_offset = pitch * h;
            let u_pitch = pitch / 2;
            for row in 0..half_h {
                let src_start = row * frame.strides[1];
                let src_end = src_start + (frame.width as usize / 2);
                let dst_start = u_offset + row * u_pitch;
                buffer[dst_start..dst_start + (frame.width as usize / 2)]
                    .copy_from_slice(&frame.planes[1][src_start..src_end]);
            }

            // V plane
            let v_offset = u_offset + u_pitch * half_h;
            for row in 0..half_h {
                let src_start = row * frame.strides[2];
                let src_end = src_start + (frame.width as usize / 2);
                let dst_start = v_offset + row * u_pitch;
                buffer[dst_start..dst_start + (frame.width as usize / 2)]
                    .copy_from_slice(&frame.planes[2][src_start..src_end]);
            }
        }).map_err(|e| anyhow::anyhow!("texture lock: {}", e))?;

        let dst = Rect::new(0, 0, self.width, self.height);
        self.canvas.copy(&texture, None, Some(dst))
            .map_err(|e| anyhow::anyhow!("canvas copy: {}", e))?;
        self.canvas.present();

        self.frame_count += 1;
        Ok(())
    }

    pub fn frame_count(&self) -> u64 { self.frame_count }
}
