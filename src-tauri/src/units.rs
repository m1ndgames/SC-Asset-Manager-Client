//! Cargo units and the arithmetic that depends on them.
//!
//! Everything converts through a μSCU pivot rather than between pairs, so there
//! is one place to be wrong instead of six. INTEGRATIONS.md §8.

use serde::{Deserialize, Serialize};

/// 1 SCU = 1 000 000 μSCU, 1 cSCU = 10 000 μSCU (INTEGRATIONS.md §8), hence
/// 1 SCU = 100 cSCU.
const MICRO_PER_SCU: f64 = 1_000_000.0;
const MICRO_PER_CENTI_SCU: f64 = 10_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CargoUnit {
    /// Standard Cargo Unit.
    #[serde(rename = "SCU")]
    Scu,
    /// Centi-SCU, 1/100 SCU. What a buy line's `quantity` is denominated in.
    #[serde(rename = "cSCU")]
    CentiScu,
    /// Micro-SCU, 1/1 000 000 SCU.
    ///
    /// NOTE the wire name starts with U+03BC GREEK SMALL LETTER MU, not U+00B5
    /// MICRO SIGN. See `mu_is_greek_small_letter_mu`.
    #[serde(rename = "μSCU")]
    MicroScu,
}

impl CargoUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            CargoUnit::Scu => "SCU",
            CargoUnit::CentiScu => "cSCU",
            CargoUnit::MicroScu => "μSCU",
        }
    }

    fn micro_per(self) -> f64 {
        match self {
            CargoUnit::Scu => MICRO_PER_SCU,
            CargoUnit::CentiScu => MICRO_PER_CENTI_SCU,
            CargoUnit::MicroScu => 1.0,
        }
    }
}

/// Quantity expressed in μSCU.
pub fn to_micro(amount: f64, unit: CargoUnit) -> f64 {
    amount * unit.micro_per()
}

/// Quantity in μSCU expressed in `unit`.
pub fn from_micro(micro: f64, unit: CargoUnit) -> f64 {
    micro / unit.micro_per()
}

/// Convert a quantity between units through the μSCU pivot.
pub fn convert(amount: f64, from: CargoUnit, to: CargoUnit) -> f64 {
    if from == to {
        return amount;
    }
    from_micro(to_micro(amount, from), to)
}

/// A price per `from` unit expressed per `to` unit.
///
/// Prices scale inversely to quantities: 1.889409 aUEC/cSCU is 188.9409
/// aUEC/SCU. Keeping this as its own function is what stops a quantity
/// conversion being applied to a price.
pub fn convert_price(price: f64, from: CargoUnit, to: CargoUnit) -> f64 {
    if from == to {
        return price;
    }
    price * (to.micro_per() / from.micro_per())
}

/// A buy line's unit price in aUEC per SCU.
///
/// Derived from `shopPricePerCentiSCU`, **not** `price / quantity`: `price` is
/// rounded to whole aUEC, so dividing it loses precision. Validated against 21
/// real buy lines, where the two agree to within 0.1%.
pub fn buy_unit_price_per_scu(shop_price_per_centi_scu: f64) -> f64 {
    convert_price(shop_price_per_centi_scu, CargoUnit::CentiScu, CargoUnit::Scu)
}

/// A sell line's unit price in aUEC per SCU. The log has no price field on a
/// sell, so it comes from the total.
pub fn sell_unit_price_per_scu(amount_auec: f64, quantity_scu: f64) -> Option<f64> {
    if quantity_scu == 0.0 || !quantity_scu.is_finite() {
        return None;
    }
    Some(amount_auec / quantity_scu)
}

/// Quantity-weighted average buy price, for merging a buy into an existing
/// asset. INTEGRATIONS.md §8.
///
/// The web app's manual merge *overwrites* `buyPrice`; that destroys the cost
/// basis, so this client does not copy it.
///
/// Both prices must already be denominated in the same unit. Returns `None`
/// when the combined amount is zero, rather than a NaN that would be written to
/// the database.
pub fn weighted_average_price(
    old_amount: f64,
    old_price: f64,
    added_amount: f64,
    added_price: f64,
) -> Option<f64> {
    let total = old_amount + added_amount;
    if total <= 0.0 || !total.is_finite() {
        return None;
    }
    Some((old_amount * old_price + added_amount * added_price) / total)
}

/// The outcome of applying a sell to an asset's holdings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SellOutcome {
    /// What is actually sold, after clamping, in the asset's unit.
    pub sold: f64,
    /// What remains on the asset afterwards.
    pub remaining: f64,
    /// The asset is exhausted and must be deleted. INTEGRATIONS.md §8 says
    /// delete when the result is <= 0.
    pub delete_asset: bool,
    /// How much of the request could not be satisfied. Non-zero means the UI
    /// must show the discrepancy — never write a negative amount.
    pub over_sold_by: f64,
}

/// Apply a sell against available holdings, clamping to what exists.
pub fn apply_sell(available: f64, requested: f64) -> SellOutcome {
    let sold = requested.min(available).max(0.0);
    let remaining = available - sold;
    SellOutcome {
        sold,
        remaining,
        // Guard against float dust leaving 1e-13 SCU behind.
        delete_asset: remaining <= 1e-9,
        over_sold_by: (requested - sold).max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The U+03BC vs U+00B5 trap. Both render as "μ" in most fonts, so a diff
    /// cannot show the difference, but they are different bytes on the wire and
    /// the web app compares the string.
    #[test]
    fn mu_is_greek_small_letter_mu() {
        assert_eq!('μ' as u32, 0x03BC);
        assert_ne!('μ' as u32, 0x00B5);
        assert_eq!(CargoUnit::MicroScu.as_str(), "\u{03BC}SCU");
        assert_ne!(CargoUnit::MicroScu.as_str(), "\u{00B5}SCU");
    }

    #[test]
    fn unit_names_match_the_contract() {
        assert_eq!(
            serde_json::to_string(&CargoUnit::Scu).unwrap(),
            "\"SCU\""
        );
        assert_eq!(
            serde_json::to_string(&CargoUnit::CentiScu).unwrap(),
            "\"cSCU\""
        );
        assert_eq!(
            serde_json::to_string(&CargoUnit::MicroScu).unwrap(),
            "\"\u{03BC}SCU\""
        );
    }

    #[test]
    fn micro_pivot_round_trips() {
        for unit in [CargoUnit::Scu, CargoUnit::CentiScu, CargoUnit::MicroScu] {
            assert_eq!(from_micro(to_micro(1234.5, unit), unit), 1234.5);
        }
        assert_eq!(convert(1.0, CargoUnit::Scu, CargoUnit::CentiScu), 100.0);
        assert_eq!(convert(1.0, CargoUnit::Scu, CargoUnit::MicroScu), 1_000_000.0);
        assert_eq!(convert(1.0, CargoUnit::CentiScu, CargoUnit::MicroScu), 10_000.0);
    }

    /// The real buy line: quantity[100800.000000 cSCU] is 1008 SCU.
    #[test]
    fn buy_quantity_is_centi_scu() {
        assert_eq!(
            convert(100_800.0, CargoUnit::CentiScu, CargoUnit::Scu),
            1008.0
        );
    }

    /// The contract's own worked example: shopPricePerCentiSCU[1.889409] is
    /// 188.9409 aUEC/SCU.
    ///
    /// INTEGRATIONS.md §3 adds that "188.9409 x 1008 = 190,452.43, which rounds
    /// to the logged 190,453". The product is right but the rounding claim is
    /// off by one — 190,452.43 rounds to 190,452. The logged `price` is a whole
    /// aUEC higher because `shopPricePerCentiSCU` is itself only logged to six
    /// decimals, so neither figure is exact. That is precisely why the unit
    /// price comes from `shopPricePerCentiSCU` and the total from `price`,
    /// rather than deriving one from the other.
    #[test]
    fn buy_unit_price_matches_contract_example() {
        let per_scu = buy_unit_price_per_scu(1.889409);
        assert!((per_scu - 188.9409).abs() < 1e-9, "got {per_scu}");

        let total = per_scu * 1008.0;
        assert!((total - 190_452.43).abs() < 0.01, "got {total}");
        // Within one aUEC of what the game logged, not equal to it.
        assert!((total - 190_453.0).abs() <= 1.0, "got {total}");

        // The rounded `price` field is close but less precise — which is why
        // the contract says not to derive the unit price from it.
        let from_price = 190_453.0 / 1008.0;
        assert!((from_price - per_scu).abs() > 1e-6);
        assert!((from_price - per_scu).abs() / per_scu < 0.001);
    }

    /// The real sell line: 317120 aUEC for 640 SCU is 495.5 aUEC/SCU.
    #[test]
    fn sell_unit_price_matches_contract_example() {
        assert_eq!(sell_unit_price_per_scu(317_120.0, 640.0), Some(495.5));
        assert_eq!(sell_unit_price_per_scu(1.0, 0.0), None);
    }

    #[test]
    fn prices_scale_inversely_to_quantities() {
        // Exact: multiplying by 100 is representable.
        assert_eq!(
            convert_price(1.889409, CargoUnit::CentiScu, CargoUnit::Scu),
            188.9409
        );
        // Not exact: dividing by 100 lands on 1.8894090000000001. Compared with
        // a tolerance rather than massaged, because rounding a price before
        // writing it is the thing this codebase must never do.
        let back = convert_price(188.9409, CargoUnit::Scu, CargoUnit::CentiScu);
        assert!((back - 1.889409).abs() < 1e-12, "got {back}");
    }

    #[test]
    fn weighted_average_preserves_cost_basis() {
        // 1008 SCU @ 188.9409 merged with 480 SCU @ 227.3
        let avg = weighted_average_price(1008.0, 188.9409, 480.0, 227.3).unwrap();
        let expected = (1008.0 * 188.9409 + 480.0 * 227.3) / 1488.0;
        assert!((avg - expected).abs() < 1e-9);
        // It must sit between the two inputs, nearer the larger lot.
        assert!(avg > 188.9409 && avg < 227.3);
        assert!(avg < (188.9409 + 227.3) / 2.0);
    }

    #[test]
    fn weighted_average_rejects_degenerate_input() {
        assert_eq!(weighted_average_price(0.0, 10.0, 0.0, 20.0), None);
        assert_eq!(weighted_average_price(-5.0, 10.0, 5.0, 20.0), None);
    }

    #[test]
    fn sell_deletes_asset_at_exactly_zero() {
        let o = apply_sell(640.0, 640.0);
        assert_eq!(o.sold, 640.0);
        assert_eq!(o.remaining, 0.0);
        assert!(o.delete_asset);
        assert_eq!(o.over_sold_by, 0.0);
    }

    #[test]
    fn partial_sell_keeps_asset() {
        let o = apply_sell(1008.0, 320.0);
        assert_eq!(o.sold, 320.0);
        assert_eq!(o.remaining, 688.0);
        assert!(!o.delete_asset);
        assert_eq!(o.over_sold_by, 0.0);
    }

    /// Never write a negative amount; clamp and report the discrepancy.
    #[test]
    fn over_sell_is_clamped_and_reported() {
        let o = apply_sell(100.0, 250.0);
        assert_eq!(o.sold, 100.0);
        assert_eq!(o.remaining, 0.0);
        assert!(o.delete_asset);
        assert_eq!(o.over_sold_by, 150.0);
        assert!(o.remaining >= 0.0);
    }

    #[test]
    fn float_dust_still_deletes() {
        let o = apply_sell(1e-12, 1e-12);
        assert!(o.delete_asset, "a hair of residue must not keep the asset alive");
    }
}
