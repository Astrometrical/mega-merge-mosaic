//! Number formatting for the session window: canvas size with megapixels.

/// `n` with a space between each group of three digits ("18 310").
fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// A canvas size with megapixels: `"9 255 × 18 310 (169 MP)"`. Below
/// 10 MP one decimal is shown.
pub fn format_canvas(width: u64, height: u64) -> String {
    let px = width as u128 * height as u128;
    let mp = if px == 0 {
        "0".to_string()
    } else if px < 10_000_000 {
        format!("{:.1}", px as f64 / 1e6)
    } else {
        group_digits(((px + 500_000) / 1_000_000) as u64)
    };
    format!(
        "{} × {} ({mp} MP)",
        group_digits(width),
        group_digits(height)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn megapixel_formatting() {
        assert_eq!(format_canvas(9255, 18310), "9 255 × 18 310 (169 MP)");
        assert_eq!(format_canvas(70_000, 48_000), "70 000 × 48 000 (3 360 MP)");
        assert_eq!(format_canvas(600, 420), "600 × 420 (0.3 MP)");
        assert_eq!(format_canvas(0, 0), "0 × 0 (0 MP)");
    }
}
