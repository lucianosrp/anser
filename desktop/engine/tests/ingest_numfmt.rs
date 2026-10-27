//! The natural key embeds each amount's shortest round-trip text ("3.0",
//! "1e-05", "-1250.5"); one differently formatted float is a different
//! raw_hash, which orphans overrides and re-inserts history. This pins
//! float_repr (and the CLI's format_g / format_thousands) over ~200k values:
//! random bit patterns, money-like cents, crypto-like 8-decimal amounts and
//! every scientific-notation boundary. The expected digest was recorded when
//! the formatter was verified against an independent reference
//! implementation; a change here changes stored keys, so never just re-bless
//! it.

use finance_engine::sources::textfmt;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
}

fn values() -> Vec<f64> {
    let mut r = Rng(0x9E3779B97F4A7C15);
    let mut v = Vec::new();
    for _ in 0..60_000 {
        let x = f64::from_bits(r.next());
        if x.is_finite() {
            v.push(x);
        }
    }
    for _ in 0..60_000 {
        // cents, as bank exports carry them, parsed like the exports are
        let c = (r.next() % 100_000_000) as i64 - 50_000_000;
        v.push(format!("{}.{:02}", c / 100, (c % 100).abs()).parse().unwrap());
        v.push(c as f64 / 100.0);
    }
    for _ in 0..30_000 {
        let s = (r.next() % 10_000_000_000) as f64 / 1e8;
        v.push(s);
        v.push(-s);
        v.push(0.0 - s);
    }
    for e in -330..=310 {
        for m in [1.0, 1.5, 9.999999999999999, 2.2250738585072014] {
            v.push(m * 10f64.powi(e));
        }
    }
    v.extend([0.0, -0.0, 1e16, 9999999999999998.0, 1e-4, 9.999999999999999e-5, 5e-324, f64::MAX, f64::MIN_POSITIVE]);
    v
}

/// FNV-1a 64 of every "repr\tg\t,.2f\n" line, in order.
const DIGEST: u64 = 0xfba90ffdf04f35ee;

#[test]
fn number_formats_are_pinned() {
    let vals = values();
    let mut h: u64 = 0xcbf29ce484222325;
    for x in &vals {
        let t = if x.abs() < 1e300 { textfmt::format_thousands(*x, 2) } else { String::new() };
        let line = format!("{}\t{}\t{t}\n", textfmt::float_repr(*x), textfmt::format_g(*x));
        for b in line.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    assert_eq!(vals.len(), 272_540, "the value set itself changed");
    assert_eq!(h, DIGEST, "number formatting changed: got {h:#018x}");
}

#[test]
fn spot_checks() {
    for (x, repr, g, t) in [
        (-1250.5, "-1250.5", "-1250.5", "-1,250.50"),
        (10000.0, "10000.0", "10000", "10,000.00"),
        (1e-5, "1e-05", "1e-05", "0.00"),
        (0.1 + 0.2, "0.30000000000000004", "0.3", "0.30"),
        (1234567.891, "1234567.891", "1.23457e+06", "1,234,567.89"),
        (1e16, "1e+16", "1e+16", "10,000,000,000,000,000.00"),
        (0.00012345678, "0.00012345678", "0.000123457", "0.00"),
    ] {
        assert_eq!(textfmt::float_repr(x), repr, "repr {x:e}");
        assert_eq!(textfmt::format_g(x), g, "g {x:e}");
        assert_eq!(textfmt::format_thousands(x, 2), t, ",.2f {x:e}");
    }
}
