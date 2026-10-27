//! The text and number forms the ingest key is built from.
//!
//! `natural_key` -> raw_hash hashes each row's fields in these exact forms:
//! the shortest round-trip float text (`3.0`, `1e-05`), whitespace trimmed
//! by a wide definition of whitespace, lines split on every line break
//! (form feeds included). The forms are fixed by the keys already stored: a
//! float formatted differently ("3" vs "3.0", "1e-5" vs "1e-05") is a
//! different raw_hash, which silently orphans the owner's txn_overrides and
//! re-inserts history on the next overlapping export. tests/ingest_numfmt.rs
//! pins the number formats over ~200k values.

/// Whitespace for one char: Unicode White_Space plus the ASCII separators
/// \x1c-\x1f (which `char::is_whitespace` leaves out).
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `s` without leading and trailing [`is_space`] chars.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// Words: runs of [`is_space`] separate, ends are dropped.
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|p| !p.is_empty()).collect()
}

/// Lines, broken on every line boundary, not just \n: \r, \r\n, \v, \f
/// (which pdftotext emits between pages, so a statement's first row on a new
/// page is its own line), \x1c-\x1e, NEL and the Unicode separators. A
/// trailing break adds no empty line.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if brk {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = it.peek() {
                    it.next();
                    end = j + 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Newline normalization: \r\n and lone \r both become \n.
pub fn universal_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Shortest round-trip digits and decimal exponent of a finite, nonzero
/// float: value = 0.DIGITS x 10^decpt.
fn shortest_digits(x: f64) -> (String, i32) {
    // Rust's `{:e}` is the shortest representation that round-trips, closest
    // to the exact value
    let split = |e: &str| -> (String, i32) {
        let (mant, exp) = e.split_once('e').expect("LowerExp always has an exponent");
        let digits: String = mant.chars().filter(|c| *c != '.').collect();
        (digits, exp.parse::<i32>().expect("integer exponent") + 1)
    };
    let (digits, decpt) = split(&format!("{:e}", x.abs()));
    // When two strings of that length both round-trip and sit exactly as far
    // from the true value (x = ...536.25 -> ".2" or ".3"), the stored keys
    // carry the even digit, and Rust's shortest formatter does not pick it.
    // The exact (correctly rounded, half-even) formatting at the same length
    // is the one whenever it still round-trips.
    let exact = format!("{:.*e}", digits.len() - 1, x.abs());
    if exact.parse::<f64>().ok() == Some(x.abs()) {
        return split(&exact);
    }
    (digits, decpt)
}

/// The shortest round-trip text of a float, always with a decimal point or
/// an exponent: "3.0", "0.1", "1e-05", "1.5e+16", "nan", "inf".
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let sign = if x.is_sign_negative() { "-" } else { "" };
    if x == 0.0 {
        return format!("{sign}0.0");
    }
    let (digits, decpt) = shortest_digits(x);
    let n = digits.len() as i32;
    // scientific outside 1e-4 <= |x| < 1e16
    if decpt <= -4 || decpt > 16 {
        let exp = decpt - 1;
        let mut m = digits[..1].to_string();
        if n > 1 {
            m.push('.');
            m.push_str(&digits[1..]);
        }
        let esign = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{m}e{esign}{:02}", exp.abs());
    }
    if decpt <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-decpt) as usize))
    } else if decpt >= n {
        format!("{sign}{digits}{}.0", "0".repeat((decpt - n) as usize))
    } else {
        let (a, b) = digits.split_at(decpt as usize);
        format!("{sign}{a}.{b}")
    }
}

/// General format (`%g`): 6 significant digits, trailing zeros dropped, scientific when
/// the exponent is < -4 or >= 6.
pub fn format_g(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let e = format!("{:.5e}", x);
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let trim = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if !(-4..6).contains(&exp) {
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{}e{esign}{:02}", trim(mant), exp.abs())
    } else {
        let prec = (5 - exp).max(0) as usize;
        trim(&format!("{:.*}", prec, x))
    }
}

/// Fixed point with `prec` decimals and comma thousands separators.
pub fn format_thousands(x: f64, prec: usize) -> String {
    let s = format!("{:.*}", prec, x);
    let (sign, body) = match s.strip_prefix('-') {
        Some(b) => ("-", b),
        None => ("", s.as_str()),
    };
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (body, None),
    };
    if !int.bytes().all(|b| b.is_ascii_digit()) {
        return s; // inf / nan
    }
    let mut grouped = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    match frac {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

/// The first `n` chars of `s` (code points, not bytes).
pub fn prefix_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// A path in normal form: `//` and `.` components collapsed, no trailing
/// slash. ingested_files.path holds this form, so the stat fast path must
/// build the identical string.
pub fn path_str(p: &std::path::Path) -> String {
    use std::path::Component;
    let mut parts: Vec<String> = Vec::new();
    let mut absolute = false;
    for c in p.components() {
        match c {
            Component::RootDir => absolute = true,
            Component::CurDir => {}
            Component::Prefix(x) => parts.push(x.as_os_str().to_string_lossy().into_owned()),
            Component::ParentDir => parts.push("..".into()),
            Component::Normal(x) => parts.push(x.to_string_lossy().into_owned()),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".into()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_basics() {
        for (x, want) in [
            (3.0, "3.0"),
            (-3.0, "-3.0"),
            (0.1, "0.1"),
            (1e-5, "1e-05"),
            (1e-4, "0.0001"),
            (0.00012, "0.00012"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (1234567890123456.7, "1234567890123456.8"),
            (12345678901234567.0, "1.2345678901234568e+16"),
            (0.1 + 0.2, "0.30000000000000004"),
            (-0.0, "-0.0"),
            (1.5e-7, "1.5e-07"),
            (2.5e300, "2.5e+300"),
            (5e-324, "5e-324"),
            (-10000.0, "-10000.0"),
            (1234.56, "1234.56"),
            (f64::NAN, "nan"),
            (f64::INFINITY, "inf"),
        ] {
            assert_eq!(float_repr(x), want, "{x:e}");
        }
    }

    #[test]
    fn g_and_thousands() {
        assert_eq!(format_g(0.6125), "0.6125");
        assert_eq!(format_g(1.0), "1");
        assert_eq!(format_g(0.5), "0.5");
        assert_eq!(format_g(1234567.0), "1.23457e+06");
        assert_eq!(format_g(0.0001), "0.0001");
        assert_eq!(format_g(0.00001), "1e-05");
        assert_eq!(format_thousands(-1234567.891, 2), "-1,234,567.89");
        assert_eq!(format_thousands(12.5, 2), "12.50");
        assert_eq!(format_thousands(999.999, 2), "1,000.00");
        assert_eq!(format_thousands(123456.0, 0), "123,456");
    }

    #[test]
    fn lines_and_space() {
        assert_eq!(splitlines("a\r\nb\rc\x0cd\n\ne"), vec!["a", "b", "c", "d", "", "e"]);
        assert_eq!(splitlines("a\n"), vec!["a"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(strip("\x1c a \u{a0}"), "a");
        assert_eq!(split_ws("  a  b\tc "), vec!["a", "b", "c"]);
        assert_eq!(path_str(std::path::Path::new("/a//b/./c/")), "/a/b/c");
    }
}
