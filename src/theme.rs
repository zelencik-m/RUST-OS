//! The look: two VGA palettes and the color roles that use them.
//!
//! VGA text mode has 16 colors, but which 16 is up to us: [`load`] writes
//! them into the DAC. So each phase gets its own palette:
//!
//! - [`BOOT`] (boot screen): a 14-step gradient from near-black navy through
//!   teal and mint to white, plus amber and a glow white. Drawing with
//!   neighbouring slots ([`shade`]) gives real gradients.
//! - [`TERMINAL`] (the console after boot): cool slate/cyan/mint. Programs
//!   use ANSI colors, which land on these slots (`ESC[1;34m`, bright blue,
//!   is the cyan accent). Magenta and pink, which nothing needs, are two
//!   extra dark shades for gradients (`ESC[35m`, `ESC[95m`).

use crate::vga_buffer::{self, Color};

pub type Palette = [(u8, u8, u8); 16];

/// The console after boot. Slot comments: VGA name, then the role.
pub const TERMINAL: Palette = [
    (6, 10, 14),     // 0  Black       background
    (16, 30, 42),    // 1  Blue        bars, panels
    (44, 150, 124),  // 2  Green       mid teal-green
    (52, 160, 172),  // 3  Cyan        mid cyan
    (200, 72, 62),   // 4  Red
    (22, 44, 58),    // 5  Magenta     gradient shade (dark)
    (190, 122, 34),  // 6  Brown       dim amber
    (168, 212, 206), // 7  LightGray   text: pale mint-gray
    (58, 86, 98),    // 8  DarkGray    dim: frames, hints
    (92, 206, 224),  // 9  LightBlue   accent: cyan
    (112, 234, 178), // 10 LightGreen  bright mint ("OK", prompt)
    (186, 236, 242), // 11 LightCyan   pale cyan
    (255, 112, 92),  // 12 LightRed    errors
    (34, 66, 84),    // 13 Pink        gradient shade (mid)
    (255, 186, 72),  // 14 Yellow      amber: warnings
    (236, 248, 250), // 15 White       highlight
];

/// The boot screen: slots 0–13 are one gradient, dark to light.
pub const BOOT: Palette = [
    (4, 6, 12),      // 0  background
    (8, 14, 28),     // 1
    (10, 22, 44),    // 2
    (12, 32, 62),    // 3
    (14, 44, 80),    // 4
    (16, 60, 98),    // 5
    (18, 78, 112),   // 6
    (22, 98, 124),   // 7
    (28, 120, 134),  // 8
    (40, 142, 142),  // 9
    (60, 164, 150),  // 10
    (90, 186, 164),  // 11
    (130, 208, 186), // 12
    (184, 232, 216), // 13
    (255, 176, 64),  // 14 amber
    (242, 250, 255), // 15 glow
];

/// Steps in the [`BOOT`] gradient (slots `0..GRADIENT_STEPS`).
pub const GRADIENT_STEPS: u8 = 14;
/// [`BOOT`] slots outside the gradient.
pub const BOOT_AMBER: Color = Color::Yellow;
pub const BOOT_GLOW: Color = Color::White;

// Roles in the TERMINAL palette (status bar, panic screen).
/// Background.
pub const BG: Color = Color::Black;
/// Bars and panel backgrounds.
pub const PANEL: Color = Color::Blue;
/// A lighter panel shade, for gradients.
pub const PANEL_HIGH: Color = Color::Pink;
/// Normal text.
pub const TEXT: Color = Color::LightGray;
/// Frames, hints.
pub const DIM: Color = Color::DarkGray;
/// Accent: titles, tags.
pub const ACCENT: Color = Color::LightBlue;
/// "OK", success.
pub const BRIGHT: Color = Color::LightGreen;
/// Warnings.
pub const AMBER: Color = Color::Yellow;

/// The 16 colors in slot order.
const SLOTS: [Color; 16] = [
    Color::Black,
    Color::Blue,
    Color::Green,
    Color::Cyan,
    Color::Red,
    Color::Magenta,
    Color::Brown,
    Color::LightGray,
    Color::DarkGray,
    Color::LightBlue,
    Color::LightGreen,
    Color::LightCyan,
    Color::LightRed,
    Color::Pink,
    Color::Yellow,
    Color::White,
];

/// The color in palette slot `slot` (0–15).
pub fn slot(slot: u8) -> Color {
    SLOTS[(slot & 15) as usize]
}

/// Step `step` of the [`BOOT`] gradient, clamped to 0..=13.
pub fn shade(step: i32) -> Color {
    slot(step.clamp(0, GRADIENT_STEPS as i32 - 1) as u8)
}

/// The terminal palette at full brightness.
pub fn apply() {
    load(&TERMINAL, FULL);
}

/// Full brightness for [`load`].
pub const FULL: u8 = 16;

/// Load `palette` at `level` sixteenths of its brightness (0: all black).
pub fn load(palette: &Palette, level: u8) {
    let level = level.min(FULL) as u16;
    let mut scaled = [(0u8, 0u8, 0u8); 16];
    for (out, &(r, g, b)) in scaled.iter_mut().zip(palette.iter()) {
        let scale = |c: u8| (c as u16 * level / FULL as u16) as u8;
        *out = (scale(r), scale(g), scale(b));
    }
    vga_buffer::set_palette(&scaled);
}
