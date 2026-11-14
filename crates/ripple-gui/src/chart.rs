//! Geometry of the fan curve chart, without GPUI types so it can be tested.
//!
//! The chart spans `T_MIN..=T_MAX` °C across and 0..=100 % up. A curve is five duty values at
//! `FAN_ANCHORS`; the engine holds the first value below the first anchor and the last one above the
//! last, and runs both fans flat out from `FAN_PANIC_C`.

use ripple_hw::control::{FAN_ANCHORS, FAN_PANIC_C};

pub const T_MIN: f32 = 30.;
pub const T_MAX: f32 = 100.;

/// How far across the plot (0..=1) `temp` °C is.
pub fn frac_x(temp: f32) -> f32 {
    ((temp - T_MIN) / (T_MAX - T_MIN)).clamp(0., 1.)
}

/// The outline of a curve as `(°C, duty %)`, left edge to right edge, the way the engine acts on it.
pub fn outline(curve: &[u8; 5]) -> Vec<(f32, f32)> {
    let panic = FAN_PANIC_C as f32;
    let mut v = vec![(T_MIN, f32::from(curve[0]))];
    v.extend((0..5).map(|i| (f32::from(FAN_ANCHORS[i]), f32::from(curve[i]))));
    v.extend([(panic, f32::from(curve[4])), (panic, 100.), (T_MAX, 100.)]);
    v
}

/// The anchor whose temperature is nearest to a pointer `frac` (0..=1) of the way across the plot.
pub fn nearest_anchor(frac: f32) -> usize {
    let t = T_MIN + frac.clamp(0., 1.) * (T_MAX - T_MIN);
    let away = |i: usize| (f32::from(FAN_ANCHORS[i]) - t).abs();
    (0..FAN_ANCHORS.len())
        .min_by(|a, b| away(*a).total_cmp(&away(*b)))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plot_spans_the_chart_range() {
        assert_eq!(frac_x(T_MIN), 0.);
        assert_eq!(frac_x(T_MAX), 1.);
        assert_eq!(frac_x(65.), 0.5);
        // outside the range stays on the edge
        assert_eq!((frac_x(0.), frac_x(150.)), (0., 1.));
    }

    #[test]
    fn the_outline_follows_the_engine() {
        let curve = [20, 30, 45, 70, 100];
        let o = outline(&curve);
        // flat to the left of the first anchor, then every anchor in order
        assert_eq!(o[0], (T_MIN, 20.));
        for (i, anchor) in FAN_ANCHORS.iter().enumerate() {
            assert_eq!(o[1 + i], (f32::from(*anchor), f32::from(curve[i])));
        }
        // flat to the panic temperature, then both fans flat out
        let panic = FAN_PANIC_C as f32;
        assert_eq!(o[6], (panic, 100.));
        assert_eq!(o[7], (panic, 100.));
        assert_eq!(o[8], (T_MAX, 100.));
        // never goes back in temperature
        assert!(o.windows(2).all(|w| w[0].0 <= w[1].0));
    }

    #[test]
    fn a_pointer_grabs_the_nearest_column() {
        let at = |t: f32| nearest_anchor(frac_x(t));
        assert_eq!(at(30.), 0);
        assert_eq!(at(49.), 0);
        assert_eq!(at(51.), 1);
        assert_eq!(at(65.), 2);
        assert_eq!(at(84.), 4);
        assert_eq!(at(100.), 4);
    }
}
