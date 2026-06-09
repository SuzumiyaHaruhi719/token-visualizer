//! Small shared formatting helpers used across the server + desktop shell.

/// Format an integer with thousands separators, e.g. `123456` -> `123,456`.
/// Negative values keep a leading `-` (`-1234` -> `-1,234`).
pub fn format_thousands(n: i64) -> String {
    let neg = n < 0;
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    let bytes = digits.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_thousands_inserts_separators() {
        assert_eq!(format_thousands(0), "0");
        assert_eq!(format_thousands(42), "42");
        assert_eq!(format_thousands(999), "999");
        assert_eq!(format_thousands(1_000), "1,000");
        assert_eq!(format_thousands(123_456), "123,456");
        assert_eq!(format_thousands(12_345_678), "12,345,678");
        assert_eq!(format_thousands(-1_234), "-1,234");
    }
}
