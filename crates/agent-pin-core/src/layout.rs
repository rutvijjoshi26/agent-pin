//! Window placement arithmetic for the overlay, kept pure so it is tested
//! here rather than only in the app crate's CI build.

/// Where a window whose top is at `win_y` goes when it wants to be `want_h`
/// tall on a work area spanning `area_y .. area_y + area_h`, all in the
/// same (physical) pixels. Returns `(y, h)`: the height is capped at the
/// work area's, and the window moves up just enough to keep its bottom on
/// the work area (never above the area's top). A non-positive `area_h`
/// means the area is unknown: nothing changes.
pub fn fit_rect(win_y: i32, want_h: i32, area_y: i32, area_h: i32) -> (i32, i32) {
    if area_h <= 0 {
        return (win_y, want_h);
    }
    let h = want_h.min(area_h);
    let bottom = area_y.saturating_add(area_h);
    let y = win_y.min(bottom.saturating_sub(h)).max(area_y);
    (y, h)
}

#[cfg(test)]
mod tests {
    use super::fit_rect;

    #[test]
    fn fits_unchanged_when_on_screen() {
        assert_eq!(fit_rect(100, 300, 0, 1000), (100, 300));
        assert_eq!(fit_rect(700, 300, 0, 1000), (700, 300));
    }

    #[test]
    fn moves_up_to_keep_the_bottom_on_screen() {
        assert_eq!(fit_rect(800, 300, 0, 1000), (700, 300));
        assert_eq!(fit_rect(900, 300, 25, 975), (700, 300));
    }

    #[test]
    fn caps_height_at_the_work_area() {
        assert_eq!(fit_rect(100, 1400, 0, 1000), (0, 1000));
        assert_eq!(fit_rect(100, 1400, 25, 975), (25, 975));
        assert_eq!(fit_rect(25, 975, 25, 975), (25, 975));
    }

    #[test]
    fn never_above_the_area_top() {
        assert_eq!(fit_rect(-50, 300, 0, 1000), (0, 300));
    }

    #[test]
    fn second_monitor_offsets() {
        // A monitor below the primary: work area y from 1080.
        assert_eq!(fit_rect(1900, 400, 1080, 1040), (1720, 400));
        // A monitor above: negative coordinates.
        assert_eq!(fit_rect(-200, 500, -1080, 1040), (-540, 500));
    }

    #[test]
    fn unknown_area_changes_nothing() {
        assert_eq!(fit_rect(900, 1400, 0, 0), (900, 1400));
        assert_eq!(fit_rect(900, 1400, 0, -5), (900, 1400));
    }
}
