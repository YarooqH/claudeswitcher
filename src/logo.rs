//! The app logo (`logo-dithered.jpg`) as geometry: an eight-armed asterisk that
//! leaves outlined copies of itself trailing off to the right, as if the account
//! behind it had just been swapped out.
//!
//! Everything here is pure maths so the same shape serves the animated dither in
//! the window (`dither::logo`) and the anti-aliased tray icon (`icon`). Lengths
//! are in units of one arm's half-length, with the asterisk centred on the origin,
//! `x` running right and `y` running down.

use std::f32::consts::{FRAC_1_SQRT_2, TAU};

/// The orange of the artwork.
pub const ORANGE: u32 = 0xc0603f;
/// The near-black the artwork sits on.
pub const INK: u32 = 0x1f2425;

/// Half the width of an arm.
pub const ARM_HALF_WIDTH: f32 = 0.147;
const ARM_CORNER: f32 = 0.078;
/// Distance between the asterisk and its first trailing copy (and between copies).
pub const TRAIL_STEP: f32 = 0.515;

/// Frame the animated asterisk is drawn in.
pub const X_MIN: f32 = -1.0;
pub const Y_MIN: f32 = -1.0;
pub const HEIGHT: f32 = 2.0;

/// Seconds for one copy to slide out and the next to take its place.
pub const PERIOD: f32 = 3.2;

/// Half the thickness of a trail outline, as in the artwork.
const STROKE: f32 = 0.016;

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Signed distance to a rounded bar lying along the x axis, one unit long on each
/// side of the origin.
fn bar(x: f32, y: f32, half_width: f32, corner: f32) -> f32 {
    let qx = x.abs() - (1.0 - corner);
    let qy = y.abs() - (half_width - corner);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - corner
}

/// Signed distance to an asterisk whose arms are `half_width` wide: negative
/// inside, positive outside.
pub fn asterisk_with(x: f32, y: f32, half_width: f32, corner: f32) -> f32 {
    let r = FRAC_1_SQRT_2;
    bar(x, y, half_width, corner)
        .min(bar(y, x, half_width, corner))
        .min(bar((x + y) * r, (y - x) * r, half_width, corner))
        .min(bar((x - y) * r, (x + y) * r, half_width, corner))
}

/// The asterisk as drawn in the artwork.
pub fn asterisk(x: f32, y: f32) -> f32 {
    asterisk_with(x, y, ARM_HALF_WIDTH, ARM_CORNER)
}

/// What one dither cell of the logo shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cell {
    Empty,
    /// Body of the asterisk; the value is the fraction of dots that are lit.
    Solid(f32),
    /// Outline of a trailing copy; the value is how strongly it shows.
    Trail(f32),
}

/// The body thins into a halftone towards the trail, in a slow travelling wave.
fn solid_density(x: f32, y: f32, phase: f32) -> f32 {
    let lean = 0.55 * x + 0.45 * y + 0.1 * (x * 2.4 - TAU * phase).sin();
    1.0 - 0.5 * smoothstep(0.1, 1.0, lean)
}

/// Copies appear out of the asterisk, fade as they travel, and are gone by the
/// third step.
fn trail_strength(step: f32) -> f32 {
    smoothstep(0.03, 0.4, step) * (1.0 - smoothstep(0.4, 2.9, step))
}

/// One cell of the logo at `(x, y)` and `time` seconds. `pixel` is the size of a
/// cell in the same units, so outlines never get thinner than a cell.
///
/// The pose at `time` and at `time + PERIOD` is identical, so the animation loops.
pub fn sample(x: f32, y: f32, time: f32, pixel: f32) -> Cell {
    let phase = (time / PERIOD).rem_euclid(1.0);
    if asterisk(x, y) <= 0.0 {
        return Cell::Solid(solid_density(x, y, phase));
    }

    let stroke = STROKE.max(pixel * 0.62);
    // Nearest copy first: each copy's body hides the outlines behind it.
    for copy in 0..3 {
        let step = phase + copy as f32;
        let distance = asterisk(x - step * TRAIL_STEP, y);
        if distance <= -stroke {
            return Cell::Empty;
        }
        if distance <= stroke {
            // Copies only trail off to the right; keep the asterisk's far side clean.
            let strength = trail_strength(step) * smoothstep(-0.1, 0.4, x);
            return if strength > 0.0 { Cell::Trail(strength) } else { Cell::Empty };
        }
    }
    Cell::Empty
}

#[cfg(test)]
mod tests {
    use super::{asterisk, sample, Cell, PERIOD, TRAIL_STEP};

    #[test]
    fn all_eight_arms_reach_the_same_length() {
        let tip = 0.97;
        let diagonal = tip * std::f32::consts::FRAC_1_SQRT_2;
        for (x, y) in [
            (tip, 0.0),
            (-tip, 0.0),
            (0.0, tip),
            (0.0, -tip),
            (diagonal, diagonal),
            (-diagonal, diagonal),
            (diagonal, -diagonal),
            (-diagonal, -diagonal),
        ] {
            assert!(asterisk(x, y) < 0.0, "({x}, {y}) should be on an arm");
        }
        assert!(asterisk(1.1, 0.0) > 0.0);
    }

    #[test]
    fn the_gaps_between_arms_are_empty() {
        assert!(asterisk(0.5, 0.25) > 0.0);
        assert!(asterisk(-0.25, 0.5) > 0.0);
    }

    #[test]
    fn the_asterisk_is_symmetric() {
        for (x, y) in [(0.3, 0.7), (0.9, 0.1), (0.2, 0.2), (0.55, 0.3)] {
            let reference = asterisk(x, y);
            for (a, b) in [(-x, y), (x, -y), (y, x)] {
                assert!((asterisk(a, b) - reference).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn at_rest_a_copy_one_step_out_shows_only_its_outline() {
        // Tip of the first copy: its outline, strongly drawn.
        let edge = sample(1.0 + TRAIL_STEP, 0.0, 0.0, 0.01);
        assert!(matches!(edge, Cell::Trail(strength) if strength > 0.7), "{edge:?}");
        // Inside that copy, past the asterisk: hidden, as the body is opaque.
        assert_eq!(sample(1.25, 0.0, 0.0, 0.01), Cell::Empty);
        // The second copy is fainter than the first.
        let far = sample(1.0 + 2.0 * TRAIL_STEP, 0.0, 0.0, 0.01);
        let (Cell::Trail(near), Cell::Trail(far)) = (edge, far) else {
            panic!("both copies should be outlined");
        };
        assert!(far < near);
    }

    #[test]
    fn a_new_copy_does_not_halo_the_asterisk_before_it_moves() {
        assert_eq!(sample(1.02, 0.0, 0.0, 0.01), Cell::Empty);
    }

    #[test]
    fn outlines_stay_at_least_a_cell_thick() {
        // A hair-thin outline must still hit a cell centred a little off the edge.
        let coarse = sample(1.0 + TRAIL_STEP + 0.05, 0.0, 0.0, 0.1);
        assert!(matches!(coarse, Cell::Trail(_)), "{coarse:?}");
    }

    #[test]
    fn the_animation_loops_seamlessly() {
        fn same(a: Cell, b: Cell) -> bool {
            match (a, b) {
                (Cell::Empty, Cell::Empty) => true,
                (Cell::Solid(p), Cell::Solid(q)) | (Cell::Trail(p), Cell::Trail(q)) => {
                    (p - q).abs() < 1e-3
                }
                _ => false,
            }
        }
        for time in [0.0, 0.7, 1.9] {
            for yi in -10..=10 {
                for xi in -10..=30 {
                    let (x, y) = (xi as f32 * 0.1, yi as f32 * 0.1);
                    let (now, later) = (sample(x, y, time, 0.03), sample(x, y, time + PERIOD, 0.03));
                    assert!(same(now, later), "({x}, {y}) at {time}s: {now:?} vs {later:?}");
                }
            }
        }
    }
}
