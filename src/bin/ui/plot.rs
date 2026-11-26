//! Drawing the plot lines: a list of values becomes an SVG path, which the
//! `PlotLine` elements in window.slint display.

// Size of the coordinate system the plot lines are drawn in. Must match
// `viewbox-width` / `viewbox-height` of `PlotLine` in window.slint.
const PLOT_WIDTH: f64 = 1000.0;
const PLOT_HEIGHT: f64 = 100.0;

/// Turn a series of values into an SVG path such as "M 0 100 L 1.7 96 ...".
///
/// `impl Iterator<Item = f64>` means "anything that yields f64 values",
/// so the caller can pass any of the plotted series without first copying
/// them into a separate list.
///
/// `max_value` is the value drawn at the top edge of the plot, and `slots`
/// the number of samples a full plot holds. A NaN value (no reading)
/// leaves a gap: the next real value starts a new line.
pub fn build_path(values: impl Iterator<Item = f64>, max_value: f64, slots: usize) -> String {
    let mut path = String::new();
    // Is the pen on the paper, i.e. does the next point continue a line?
    let mut drawing = false;

    // `enumerate` pairs each value with its index: (0, v0), (1, v1), ...
    for (index, value) in values.enumerate() {
        if value.is_nan() {
            drawing = false;
            continue;
        }

        // Oldest sample at the left edge; a full history spans the width.
        let x = index as f64 / (slots - 1) as f64 * PLOT_WIDTH;

        // Screen y grows downwards, so flip it. Keep a small margin so a
        // line at exactly 0 or max is not cut in half by the plot edge.
        let fraction = (value / max_value).clamp(0.0, 1.0);
        let y = PLOT_HEIGHT - (2.0 + fraction * (PLOT_HEIGHT - 4.0));

        // "M" moves the pen to the start of a line, "L" draws a line to
        // each following point.
        let command = if drawing { 'L' } else { 'M' };
        path.push_str(&format!("{command} {x:.1} {y:.1} "));
        drawing = true;
    }

    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::HISTORY_LEN;

    #[test]
    fn empty_history_gives_empty_path() {
        assert_eq!(build_path(std::iter::empty(), 1.0, HISTORY_LEN), "");
    }

    #[test]
    fn path_starts_with_move_then_draws_lines() {
        // A Vec can be turned into an iterator with `into_iter()`.
        let path = build_path(vec![0.0, 1.0].into_iter(), 1.0, HISTORY_LEN);
        // Zero is at the bottom (y near 100), the maximum at the top.
        assert!(path.starts_with("M 0.0 98.0 L "));
        assert!(path.trim_end().ends_with(" 2.0"));
    }

    #[test]
    fn values_outside_the_range_stay_inside_the_plot() {
        let path = build_path(vec![-5.0, 99.0].into_iter(), 1.0, HISTORY_LEN);
        assert!(path.starts_with("M 0.0 98.0 "));
        assert!(path.trim_end().ends_with(" 2.0"));
    }

    #[test]
    fn missing_readings_leave_a_gap() {
        let path = build_path(vec![0.0, f64::NAN, 1.0].into_iter(), 1.0, HISTORY_LEN);
        // Two separate lines, each starting with "M", and no "NaN" text.
        assert_eq!(path.matches('M').count(), 2);
        assert!(!path.contains("NaN"));
    }
}
