use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

/// Renders a simple cursor sprite on top of the video frame.
/// MVP: Arrow cursor as a simple white triangle with black outline.
#[derive(Clone)]
pub struct CursorRenderer {
    pub x: i32,
    pub y: i32,
    pub shape_id: u8,
    pub visible: bool,
    /// When true, rotate the arrow shape to compensate for physical monitor rotation.
    pub rotated: bool,
}

impl CursorRenderer {
    pub fn new() -> Self {
        Self { x: 0, y: 0, shape_id: 0, visible: false, rotated: false }
    }

    pub fn update(&mut self, x: i32, y: i32, shape_id: u8) {
        self.x = x;
        self.y = y;
        self.shape_id = shape_id;
        self.visible = true;
    }

    /// Draw cursor on the canvas. Call after rendering the video frame.
    pub fn draw(&self, canvas: &mut Canvas<Window>) {
        if !self.visible { return; }

        let x = self.x;
        let y = self.y;

        // Draw arrow cursor: black outline + white fill
        // Simple 12x18 arrow shape using filled rects (no complex polygon needed)
        let outline = Color::RGB(0, 0, 0);
        let fill = Color::RGB(255, 255, 255);

        // Outer (black) - slightly larger
        canvas.set_draw_color(outline);
        draw_arrow(canvas, x, y, 1, self.rotated);

        // Inner (white)
        canvas.set_draw_color(fill);
        draw_arrow(canvas, x + 1, y + 1, 0, self.rotated);
    }
}

/// Draw a simple arrow cursor using horizontal lines.
/// Each row is (y_offset, x_start, width).
fn draw_arrow(canvas: &mut sdl2::render::Canvas<Window>, base_x: i32, base_y: i32, pad: i32, rotated: bool) {
    let rows: &[(i32, i32, u32)] = &[
        (0, 0, 1),
        (1, 0, 2),
        (2, 0, 3),
        (3, 0, 4),
        (4, 0, 5),
        (5, 0, 6),
        (6, 0, 7),
        (7, 0, 8),
        (8, 0, 9),
        (9, 0, 10),
        (10, 0, 11),
        (11, 0, 6),
        (12, 0, 4),
        (12, 5, 3),
        (13, 1, 3),
        (13, 6, 2),
        (14, 2, 2),
        (14, 7, 2),
        (15, 3, 1),
        (15, 8, 1),
    ];

    for &(dy, dx, w) in rows {
        if rotated {
            // Rotate arrow shape 90° CW on canvas to compensate for physical monitor rotation.
            let _ = canvas.fill_rect(Rect::new(
                base_x + dy - pad,
                base_y - dx - w as i32 + 1 - pad,
                1 + pad as u32 * 2,
                w + pad as u32 * 2,
            ));
        } else {
            let _ = canvas.fill_rect(Rect::new(
                base_x + dx - pad,
                base_y + dy - pad,
                w + pad as u32 * 2,
                1 + pad as u32 * 2,
            ));
        }
    }
}
