//! Procedurally generated tray icon — no asset files, no `image` dependency.
//!
//! It is the app logo (an asterisk with a faint trail) on a dark tile, with a pip
//! in the active account's colour so the profile is obvious at a glance. It is
//! rasterised at the shell's real small-icon size (so it is crisp at any DPI)
//! using 4x supersampling for anti-aliasing.

use tray_icon::Icon;
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};

use crate::logo;

const PALETTE: [[u8; 3]; 6] = [
    [0x4C, 0x8D, 0xFF], // blue
    [0xE2, 0x55, 0x8B], // rose
    [0x3F, 0xB9, 0x50], // green
    [0xE3, 0xA0, 0x08], // amber
    [0xA8, 0x55, 0xF7], // violet
    [0x14, 0xB8, 0xA6], // teal
];

/// Arms are drawn bolder than in the artwork, which would vanish at 16 px.
const ARM_HALF_WIDTH: f32 = 0.2;
const ARM_CORNER: f32 = 0.1;
/// How large the asterisk and its first copy are drawn, as a fraction of the tile.
const LOGO_SPAN: f32 = 0.84;
/// Brightness of the trailing copy next to the solid asterisk.
const TRAIL_GLOW: f32 = 0.3;

pub fn accent(index: usize) -> [u8; 3] {
    PALETTE[index % PALETTE.len()]
}

/// Builds an icon at the current shell small-icon size.
pub fn for_profile(index: usize) -> Icon {
    make(accent(index))
}

pub fn make(accent: [u8; 3]) -> Icon {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
    Icon::from_rgba(render(size, accent), size, size).expect("generated RGBA is well-formed")
}

fn rgb_bytes(color: u32) -> [f32; 3] {
    [
        ((color >> 16) & 0xff) as f32,
        ((color >> 8) & 0xff) as f32,
        (color & 0xff) as f32,
    ]
}

fn blend(base: [f32; 3], over: [f32; 3], amount: f32) -> [f32; 3] {
    [
        base[0] + (over[0] - base[0]) * amount,
        base[1] + (over[1] - base[1]) * amount,
        base[2] + (over[2] - base[2]) * amount,
    ]
}

/// RGBA pixels of a `size` x `size` icon.
fn render(size: u32, accent: [u8; 3]) -> Vec<u8> {
    const SUPERSAMPLE: u32 = 4;
    let samples = (SUPERSAMPLE * SUPERSAMPLE) as f32;
    let ink = rgb_bytes(logo::INK);
    let orange = rgb_bytes(logo::ORANGE);
    let accent = [accent[0] as f32, accent[1] as f32, accent[2] as f32];

    // The asterisk plus one trailing copy, centred in the tile.
    let scale = LOGO_SPAN / (2.0 + logo::TRAIL_STEP);
    let centre_x = (logo::TRAIL_STEP) / 2.0;

    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let mut covered = 0f32;
            let mut color = [0f32; 3];
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let px = (x as f32 + (sx as f32 + 0.5) / SUPERSAMPLE as f32) / size as f32;
                    let py = (y as f32 + (sy as f32 + 0.5) / SUPERSAMPLE as f32) / size as f32;
                    if !inside_badge(px, py) {
                        continue;
                    }
                    covered += 1.0;

                    let lx = (px - 0.5) / scale + centre_x;
                    let ly = (py - 0.5) / scale;
                    let shape = |dx: f32| logo::asterisk_with(lx - dx, ly, ARM_HALF_WIDTH, ARM_CORNER) <= 0.0;
                    let sample = if inside_pip(px, py) {
                        accent
                    } else if inside_pip_ring(px, py) {
                        ink
                    } else if shape(0.0) {
                        orange
                    } else if shape(logo::TRAIL_STEP) {
                        blend(ink, orange, TRAIL_GLOW)
                    } else {
                        ink
                    };
                    for channel in 0..3 {
                        color[channel] += sample[channel];
                    }
                }
            }
            let index = ((y * size + x) * 4) as usize;
            if covered > 0.0 {
                for channel in 0..3 {
                    rgba[index + channel] = (color[channel] / covered).round() as u8;
                }
            }
            rgba[index + 3] = (covered / samples * 255.0).round() as u8;
        }
    }
    rgba
}

const PIP_CENTRE: (f32, f32) = (0.76, 0.76);
const PIP_RADIUS: f32 = 0.15;
const PIP_RING: f32 = 0.215;

fn inside_pip(x: f32, y: f32) -> bool {
    (x - PIP_CENTRE.0).hypot(y - PIP_CENTRE.1) <= PIP_RADIUS
}

/// Dark halo that keeps the pip legible on top of the logo.
fn inside_pip_ring(x: f32, y: f32) -> bool {
    (x - PIP_CENTRE.0).hypot(y - PIP_CENTRE.1) <= PIP_RING
}

/// Rounded square, like a modern app tile.
fn inside_badge(x: f32, y: f32) -> bool {
    let pad = 0.05;
    let radius = 0.24;
    let half = 0.5 - pad;
    let qx = (x - 0.5).abs() - (half - radius);
    let qy = (y - 0.5).abs() - (half - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius <= 0.0
}

#[cfg(test)]
mod tests {
    use super::{accent, render};

    fn pixel(rgba: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * size + x) * 4) as usize;
        [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
    }

    #[test]
    fn the_icon_is_the_logo_on_a_rounded_tile() {
        let size = 32;
        let rgba = render(size, accent(0));
        assert_eq!(rgba.len(), (size * size * 4) as usize);

        // Transparent outside the rounded tile.
        assert_eq!(pixel(&rgba, size, 0, 0)[3], 0);
        // The middle of the asterisk is the brand orange, fully opaque.
        let [r, g, b, a] = pixel(&rgba, size, 12, 16);
        assert_eq!(a, 255);
        assert!(r > 150 && g > 60 && b < 100 && r > g && g > b, "{r},{g},{b}");
    }

    #[test]
    fn each_account_gets_its_own_pip_colour() {
        let size = 32;
        let at = |index| {
            let rgba = render(size, accent(index));
            let [r, g, b, _] = pixel(&rgba, size, 24, 24);
            [r, g, b]
        };
        assert_eq!(at(0), accent(0));
        assert_eq!(at(1), accent(1));
        assert_ne!(at(0), at(1));
    }
}
