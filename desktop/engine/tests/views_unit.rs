//! Data-free checks of the numeric and text semantics the views lean on
//! (the golden tests cover the views themselves, but need personal data).

use finance_engine::views::analytics::{display_merchant, normalize_merchant};
use finance_engine::views::investments::irr_annual_pct;
use finance_engine::views::receivables::counterparty_key;
use finance_engine::views::{title_case, round_n};

#[test]
fn round_n_rounds_the_exact_value() {
    // half-even on the exact binary value, at 2, 1 and 0 decimals
    let cases = [
        (0.125, 2, 0.12), (0.375, 2, 0.38), (2.675, 2, 2.67), (0.005, 2, 0.01), (1.005, 2, 1.0),
        (0.015, 2, 0.01), (0.025, 2, 0.03), (2.5, 0, 2.0), (3.5, 0, 4.0), (-0.125, 2, -0.12),
        (0.375, 1, 0.4), (123456.785, 2, 123456.79),
    ];
    for (x, nd, want) in cases {
        assert_eq!(round_n(x, nd), want, "round({x}, {nd})");
    }
}

#[test]
fn title_case_words() {
    assert_eq!(title_case("okx"), "Okx");
    assert_eq!(title_case("binance"), "Binance");
    assert_eq!(title_case("o'neil 2nd"), "O'Neil 2Nd");
}

#[test]
fn merchant_keys() {
    assert_eq!(display_merchant("Tesco Stores FX Rate £1 = $1.2500", 48), "Tesco Stores");
    assert_eq!(display_merchant("ATM WITHDRAWAL (21MAR24)", 48), "ATM WITHDRAWAL");
    assert_eq!(display_merchant("   ", 48), "(blank)");
    assert_eq!(normalize_merchant("SEC P100000001 PURCHASE"), "sec purchase");
}

#[test]
fn counterparties() {
    assert_eq!(counterparty_key("CR TO 848-123456-001 rent"), "848-123456-001");
    assert_eq!(counterparty_key("FPS Payment to The Landlord Ltd deposit"), "landlord deposit");
    assert_eq!(counterparty_key("123"), "unknown");
}

#[test]
fn irr() {
    // 100 in, worth 110 a year (12 months) later: 10% a year
    let mut flows = vec![0.0; 13];
    flows[0] = 100.0;
    let r = irr_annual_pct(&flows, 110.0).unwrap();
    assert!((r - 10.0).abs() < 1e-6, "{r}");
    assert_eq!(irr_annual_pct(&[0.0, 0.0], 5.0), None);
}
