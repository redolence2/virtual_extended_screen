use protocol::constants::*;
use std::net::UdpSocket;

/// Input ownership state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputOwnership {
    LocalControl,        // Mac drives cursor, Ubuntu input ignored
    RemoteControlGrabbed, // Ubuntu drives cursor, input forwarded to Mac
    RemoteControlReleased, // Stream continues, Ubuntu input not forwarded
}

/// Captures SDL2 input events and sends them to the Mac host.
/// Mouse/scroll over UDP, keyboard over TCP (via callback).
pub struct InputCapture {
    udp_socket: Option<UdpSocket>,
    host_addr: String,
    input_port: u16,
    pub ownership: InputOwnership,
    seq: u32,
    stream_width: u32,
    stream_height: u32,
    /// When true, canvas is landscape but stream is portrait (xrandr rotation).
    /// Mouse coords are transformed from canvas space to stream space.
    pub rotated: bool,
    /// Canvas dimensions (physical) — needed for scaling when stream != canvas resolution.
    pub canvas_width: u32,
    pub canvas_height: u32,
    // Grab hotkey: Ctrl+Alt+G to grab, Ctrl+Alt+Escape to release
    pub grab_pending: bool,
    pub release_pending: bool,
}

/// Key event to be sent over TCP.
pub struct KeyEventOut {
    pub hid_usage: u16,
    pub logical_keysym: u32,
    pub is_down: bool,
    pub modifiers: u32,
}

impl InputCapture {
    pub fn new(host: &str, input_port: u16, stream_width: u32, stream_height: u32) -> Self {
        let socket = UdpSocket::bind("0.0.0.0:0").ok();
        if let Some(ref s) = socket {
            let _ = s.connect(format!("{}:{}", host, input_port));
            log::info!("Input sender → {}:{}", host, input_port);
        }

        Self {
            udp_socket: socket,
            host_addr: host.to_string(),
            input_port,
            ownership: InputOwnership::LocalControl,
            seq: 0,
            stream_width,
            stream_height,
            rotated: false,
            canvas_width: stream_width,
            canvas_height: stream_height,
            grab_pending: false,
            release_pending: false,
        }
    }

    /// Process an SDL2 keyboard event. Returns Some(KeyEventOut) if it should be sent over TCP.
    /// Handles grab/release hotkeys BEFORE forwarding.
    pub fn process_key(&mut self, scancode: u32, keysym: u32, is_down: bool, modifiers: u16) -> Option<KeyEventOut> {
        // Check for grab hotkey: Ctrl+Alt+G
        let ctrl = modifiers & 0x00C0 != 0; // SDL KMOD_CTRL
        let alt = modifiers & 0x0300 != 0;   // SDL KMOD_ALT
        let is_g = scancode == 10; // SDL_SCANCODE_G
        let is_escape = scancode == 41; // SDL_SCANCODE_ESCAPE

        if ctrl && alt && is_g && is_down {
            self.grab_pending = true;
            return None; // consume hotkey
        }
        if ctrl && alt && is_escape && is_down {
            self.release_pending = true;
            return None; // consume hotkey
        }

        // Only forward if grabbed
        if self.ownership != InputOwnership::RemoteControlGrabbed {
            return None;
        }

        // Convert SDL scancode → HID usage (SDL scancodes are close to HID for common keys)
        let hid_usage = sdl_scancode_to_hid(scancode);

        Some(KeyEventOut {
            hid_usage,
            logical_keysym: keysym,
            is_down,
            modifiers: modifiers as u32,
        })
    }

    /// Transform canvas coords to stream coords (handles rotation + scaling).
    fn to_stream_coords(&self, x: i32, y: i32) -> (i32, i32) {
        if self.rotated {
            // Inverse of rendering: canvas → unscale → un-rotate → stream
            let cw = self.canvas_width as f64;
            let ch = self.canvas_height as f64;
            let sw = self.stream_width as f64;
            let sh = self.stream_height as f64;
            let scale = (cw / sh).min(ch / sw);
            let offset_x = (cw - sh * scale) / 2.0;
            let offset_y = (ch - sw * scale) / 2.0;
            // Un-scale: canvas → rotated stream coords
            let ry = (x as f64 - offset_x) / scale;
            let rx = (y as f64 - offset_y) / scale;
            // Un-rotate: rotated (ry, rx) → stream (stream_w - 1 - rx, ry)
            let sx = sw - 1.0 - rx;
            let sy = ry;
            (sx.round() as i32, sy.round() as i32)
        } else {
            // Scale from canvas to stream coords
            let sx = (x as f64 * self.stream_width as f64 / self.canvas_width as f64) as i32;
            let sy = (y as f64 * self.stream_height as f64 / self.canvas_height as f64) as i32;
            (sx, sy)
        }
    }

    /// Send mouse move event over UDP.
    pub fn send_mouse_move(&mut self, x: i32, y: i32) {
        if self.ownership != InputOwnership::RemoteControlGrabbed { return; }
        let (sx, sy) = self.to_stream_coords(x, y);
        self.send_input_event(0, sx, sy, 0, 0, 0);
    }

    /// Send mouse button down over UDP.
    pub fn send_mouse_down(&mut self, x: i32, y: i32, button: u8) {
        if self.ownership != InputOwnership::RemoteControlGrabbed { return; }
        let (sx, sy) = self.to_stream_coords(x, y);
        self.send_input_event(1, sx, sy, button, 0, 0);
    }

    /// Send mouse button up over UDP.
    pub fn send_mouse_up(&mut self, x: i32, y: i32, button: u8) {
        if self.ownership != InputOwnership::RemoteControlGrabbed { return; }
        let (sx, sy) = self.to_stream_coords(x, y);
        self.send_input_event(2, sx, sy, button, 0, 0);
    }

    /// Send scroll event over UDP.
    pub fn send_scroll(&mut self, dx: i16, dy: i16) {
        if self.ownership != InputOwnership::RemoteControlGrabbed { return; }
        if self.rotated {
            // Swap scroll axes: canvas horizontal → stream vertical and vice versa
            self.send_input_event(3, 0, 0, 0, -dy, dx);
        } else {
            self.send_input_event(3, 0, 0, 0, dx, dy);
        }
    }

    pub fn grab(&mut self) {
        self.ownership = InputOwnership::RemoteControlGrabbed;
        log::info!("Input: GRABBED (Ctrl+Alt+Escape to release)");
    }

    pub fn release(&mut self) {
        self.ownership = InputOwnership::RemoteControlReleased;
        log::info!("Input: RELEASED");
    }

    fn send_input_event(&mut self, event_type: u8, x: i32, y: i32, button: u8, scroll_dx: i16, scroll_dy: i16) {
        let Some(ref socket) = self.udp_socket else { return };

        self.seq = self.seq.wrapping_add(1);

        // Build packet: PacketPrefix(6) + InputEvent(22) = 28 bytes
        let mut packet = [0u8; INPUT_TOTAL_PACKET_BYTES];

        // PacketPrefix
        packet[0..4].copy_from_slice(&MAGIC);
        packet[4] = PROTOCOL_VERSION;
        packet[5] = PACKET_TYPE_INPUT_EVENT;

        let off = PACKET_PREFIX_BYTES;
        // seq: u32
        packet[off..off+4].copy_from_slice(&self.seq.to_le_bytes());
        // event_type: u8
        packet[off+4] = event_type;
        // x_px: i32
        packet[off+5..off+9].copy_from_slice(&x.to_le_bytes());
        // y_px: i32
        packet[off+9..off+13].copy_from_slice(&y.to_le_bytes());
        // button: u8
        packet[off+13] = button;
        // scroll_dx: i16
        packet[off+14..off+16].copy_from_slice(&scroll_dx.to_le_bytes());
        // scroll_dy: i16
        packet[off+16..off+18].copy_from_slice(&scroll_dy.to_le_bytes());
        // modifiers: u32 (unused in MVP)
        packet[off+18..off+22].copy_from_slice(&0u32.to_le_bytes());

        let _ = socket.send(&packet);
    }
}

/// Convert SDL2 scancode to USB HID Usage ID.
/// SDL scancodes are based on USB HID for most keys.
fn sdl_scancode_to_hid(scancode: u32) -> u16 {
    // SDL scancodes 4-255 map directly to HID usage codes for most keys
    if scancode <= 255 {
        scancode as u16
    } else {
        0 // unknown
    }
}
