//! Ordered-dither (Bayer) artwork, painted live as GPUI quads.
//!
//! Every picture here is a pure function of cell position and time, so it is
//! cheap to redraw on each animation frame and needs no image assets. Cells are
//! snapped to device pixels so the dots stay crisp at any display scale.

use gpui::{canvas, fill, point, px, rgb, size, Bounds, Canvas, Pixels, Rgba, Window};

use crate::logo;

/// Classic 8×8 Bayer matrix.
const BAYER: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// Faint "dot paper" grid behind the sky.
const PAPER_DOT: u32 = 0x1d1d1d;

fn threshold(x: usize, y: usize) -> f32 {
    (BAYER[y & 7][x & 7] as f32 + 0.5) / 64.0
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn hash(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374_761_393) ^ (y as u32).wrapping_mul(668_265_263);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h ^ (h >> 16)) & 0xffff) as f32 / 65535.0
}

fn value_noise(x: f32, y: f32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (ux, uy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let (xi, yi) = (x0 as i32, y0 as i32);
    let top = lerp(hash(xi, yi), hash(xi + 1, yi), ux);
    let bottom = lerp(hash(xi, yi + 1), hash(xi + 1, yi + 1), ux);
    lerp(top, bottom, uy)
}

fn fbm(x: f32, y: f32) -> f32 {
    0.55 * value_noise(x, y)
        + 0.3 * value_noise(x * 2.03 + 11.7, y * 2.03 - 3.1)
        + 0.15 * value_noise(x * 4.11 - 7.3, y * 4.11 + 5.9)
}

/// Mixes two `0xRRGGBB` colours.
fn mix(a: u32, b: u32, t: f32) -> Rgba {
    let channel = |shift: u32| {
        let ca = ((a >> shift) & 0xff) as f32;
        let cb = ((b >> shift) & 0xff) as f32;
        (lerp(ca, cb, t).round() as u32) << shift
    };
    rgb(channel(16) | channel(8) | channel(0))
}

/// Walks `bounds` in pixel-snapped cells and paints a `dot`-sized square for
/// every cell `shade(col, row, cols, rows)` returns a colour for.
fn paint_grid(
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cell: f32,
    dot: f32,
    mut shade: impl FnMut(usize, usize, usize, usize) -> Option<Rgba>,
) {
    let scale = window.scale_factor();
    let snap = |value: f32| (value * scale).round() / scale;
    let cell = snap(cell).max(1.0 / scale);
    let dot = snap(dot).clamp(1.0 / scale, cell);
    let origin_x = snap(f32::from(bounds.origin.x));
    let origin_y = snap(f32::from(bounds.origin.y));
    let cols = (f32::from(bounds.size.width) / cell).floor() as usize;
    let rows = (f32::from(bounds.size.height) / cell).floor() as usize;

    for row in 0..rows {
        for col in 0..cols {
            if let Some(color) = shade(col, row, cols, rows) {
                window.paint_quad(fill(
                    Bounds::new(
                        point(px(origin_x + col as f32 * cell), px(origin_y + row as f32 * cell)),
                        size(px(dot), px(dot)),
                    ),
                    color,
                ));
            }
        }
    }
}

/// One account's sky: its colour plus a seed so every account gets its own clouds.
#[derive(Debug, Clone, Copy)]
pub struct Sky {
    pub color: u32,
    pub seed: f32,
}

impl Sky {
    pub fn for_account(index: usize, color: u32) -> Self {
        Self {
            color,
            seed: index as f32 * 7.31 + 1.7,
        }
    }
}

/// Everything needed to draw one frame of the hero sky.
#[derive(Debug, Clone, Copy)]
pub struct SkyFrame {
    pub sky: Sky,
    /// The sky being dissolved away, if a switch is in progress.
    pub from: Option<Sky>,
    /// Dissolve progress from `from` to `sky`, 0..=1.
    pub progress: f32,
    /// Animation clock in seconds.
    pub time: f32,
}

/// Brightness of the synthwave sunset at `(u, v)`; `u`/`v` run 0..1 across the
/// canvas and `aspect` is width / height.
fn sky_density(u: f32, v: f32, aspect: f32, time: f32, seed: f32) -> f32 {
    let x = u * aspect;
    let (sun_x, sun_y, radius) = (aspect * 0.78, 1.0, 0.52);
    let dist = ((x - sun_x).powi(2) + (v - sun_y).powi(2)).sqrt();

    // A sun sinking below the bottom edge, with scrolling slits that widen
    // towards the horizon.
    let disc = 1.0 - smoothstep(radius - 0.015, radius + 0.015, dist);
    let mut sun = disc * lerp(1.0, 0.62, ((v - (sun_y - radius)) / radius).clamp(0.0, 1.0));
    let slit_top = sun_y - radius * 0.62;
    if disc > 0.0 && v > slit_top {
        let depth = (v - slit_top) / (sun_y - slit_top);
        let phase = (v * 13.0 - time * 0.45).rem_euclid(1.0);
        if phase < depth * 0.75 {
            sun = 0.0;
        }
    }
    let halo = (1.0 - smoothstep(radius, radius + 0.3, dist)) * 0.2;

    // Drifting clouds, thinning towards the horizon; they silhouette the sun.
    let n = fbm(x * 1.5 + time * 0.06 + seed, v * 2.4 + seed * 0.61);
    let clouds = smoothstep(0.56, 0.8, n) * (1.0 - smoothstep(0.35, 0.75, v));
    let lit = if sun > 0.0 {
        sun * (1.0 - clouds * 0.75)
    } else {
        halo.max(clouds * 0.58)
    };

    // Keep the left side calm so the account name reads cleanly.
    lit * smoothstep(0.22, 0.72, u)
}

/// The animated dithered sunset behind the active account.
pub fn sky(frame: SkyFrame) -> Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            if width < 1.0 || height < 1.0 {
                return;
            }
            let aspect = width / height;
            let twinkle = (frame.time * 1.6).floor() as i32;

            paint_grid(bounds, window, 3.0, 2.0, |col, row, cols, rows| {
                let u = (col as f32 + 0.5) / cols as f32;
                let v = (row as f32 + 0.5) / rows as f32;

                // Bayer-ordered dissolve that sweeps left to right.
                let reveal = (frame.progress * 1.6 - u * 0.6).clamp(0.0, 1.0);
                let order = threshold(col + 3, row + 5);
                if reveal > 0.0 && reveal < 1.0 && (order - reveal).abs() < 0.035 {
                    // Kept dim over the text column, bright where the sky lives.
                    return Some(mix(0x2a2a2a, frame.sky.color, smoothstep(0.25, 0.75, u)));
                }
                let sky = if order < reveal {
                    frame.sky
                } else if let Some(from) = frame.from {
                    from
                } else {
                    // Revealing from nothing: bare paper ahead of the sweep.
                    return (col % 4 == 0 && row % 4 == 0).then(|| rgb(PAPER_DOT));
                };

                // Two-bit ordered dither: off, colour, highlight.
                let density = sky_density(u, v, aspect, frame.time, sky.seed).powf(1.3);
                let scaled = density * 2.0;
                let base = scaled.floor();
                let level = base as u8 + u8::from(scaled - base > threshold(col, row));
                match level {
                    0 => {
                        let star = hash(col as i32, row as i32 + 977);
                        if v < 0.55 && u > 0.32 && star > 0.993 {
                            let bright = hash(col as i32 ^ twinkle, row as i32) > 0.45;
                            Some(rgb(if bright { 0xd8d4cc } else { 0x5a5852 }))
                        } else if col % 4 == 0 && row % 4 == 0 {
                            Some(rgb(PAPER_DOT))
                        } else {
                            None
                        }
                    }
                    1 => Some(rgb(sky.color)),
                    _ => Some(mix(sky.color, 0xffffff, 0.55)),
                }
            });
        },
    )
}

/// A small dithered emblem for an account; the pattern varies with the index so
/// accounts are recognisable by shape as well as colour.
pub fn tile(index: usize, color: u32) -> Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            paint_grid(bounds, window, 2.0, 2.0, |col, row, cols, rows| {
                let u = (col as f32 + 0.5) / cols as f32;
                let v = (row as f32 + 0.5) / rows as f32;
                let (du, dv) = (u - 0.5, v - 0.5);
                let radial = (du * du + dv * dv).sqrt();
                let density = match index % 5 {
                    0 => 1.05 - (u + v) * 0.5,
                    1 => 1.0 - radial * 1.7,
                    2 => {
                        let slit = v > 0.55 && (v * 7.0).fract() < (v - 0.55) * 1.8;
                        if radial < 0.42 && !slit { 1.0 - v * 0.45 } else { 0.0 }
                    }
                    3 => 0.5 + 0.5 * (radial * 19.0).cos(),
                    _ => 1.0 - v,
                };
                (density.clamp(0.0, 1.0) > threshold(col, row)).then(|| rgb(color))
            });
        },
    )
}

/// The app logo's asterisk, dithered and in motion. It is drawn as large as fits
/// while keeping its proportions, and `cell` is the size of one dot. The trailing
/// outlines of the artwork are left out: at this size they would turn to noise.
pub fn logo(time: f32, cell: f32) -> Canvas<()> {
    // Breathing room so the arms never touch the edge of the canvas.
    const MARGIN: f32 = 0.06;
    let frame = logo::HEIGHT + 2.0 * MARGIN;

    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            paint_grid(bounds, window, cell, cell, |col, row, cols, rows| {
                let scale = (cols as f32 / frame).min(rows as f32 / frame);
                let left = (cols as f32 - frame * scale) / 2.0;
                let top = (rows as f32 - frame * scale) / 2.0;
                let x = logo::X_MIN - MARGIN + (col as f32 + 0.5 - left) / scale;
                let y = logo::Y_MIN - MARGIN + (row as f32 + 0.5 - top) / scale;

                let lit = match logo::sample(x, y, time, 1.0 / scale) {
                    logo::Cell::Empty => return None,
                    logo::Cell::Solid(density) => density > threshold(col, row),
                    logo::Cell::Trail(_) => return None,
                };
                lit.then(|| rgb(logo::ORANGE))
            });
        },
    )
}

/// A dithered progress sweep for long-running work.
pub fn scanner(color: u32, time: f32) -> Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            paint_grid(bounds, window, 2.0, 2.0, |col, row, cols, _| {
                let u = col as f32 / cols as f32;
                let wave = 0.5 + 0.5 * (u * 9.0 - time * 5.0).sin();
                let edges = smoothstep(0.0, 0.15, u) * smoothstep(1.0, 0.85, u);
                (wave * edges > threshold(col, row)).then(|| rgb(color))
            });
        },
    )
}

/// Diagonal hatching used to flag a callout.
pub fn hatch(color: u32) -> Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            paint_grid(bounds, window, 2.0, 2.0, |col, row, _, _| {
                ((col + row) % 3 == 0).then(|| rgb(color))
            });
        },
    )
}

/// A 1px dotted line along the canvas' long axis.
pub fn dotted_rule(color: u32) -> Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let horizontal = bounds.size.width >= bounds.size.height;
            paint_grid(bounds, window, 1.0, 1.0, |col, row, _, _| {
                let along = if horizontal { col } else { row };
                (along % 3 == 0).then(|| rgb(color))
            });
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// 10×10 pixel-art caption glyphs.
pub fn glyph(kind: Glyph, color: u32) -> Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            paint_grid(bounds, window, 1.0, 1.0, |x, y, _, _| {
                let on = match kind {
                    Glyph::Minimize => y == 5,
                    Glyph::Maximize => (x == 0 || x == 9 || y == 0 || y == 9) && x < 10 && y < 10,
                    Glyph::Restore => {
                        let front = (x == 0 || x == 7 || y == 2 || y == 9) && x <= 7 && y >= 2 && y <= 9;
                        let back = (y == 0 && x >= 2 && x <= 9) || (x == 9 && y <= 7);
                        front || back
                    }
                    Glyph::Close => x < 10 && y < 10 && (x == y || x + y == 9),
                };
                on.then(|| rgb(color))
            });
        },
    )
}
