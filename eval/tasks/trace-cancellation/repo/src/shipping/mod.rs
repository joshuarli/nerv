use std::collections::HashMap;

/// Calculates shipping rates based on destination and weight.
/// Only used during order creation — not involved in cancellation.
pub struct ShippingCalculator {
    base_rates: HashMap<String, u64>,
    per_kg_rate: u64,
    free_shipping_threshold: u64,
}

impl ShippingCalculator {
    pub fn new() -> Self {
        let mut base_rates = HashMap::new();
        base_rates.insert("domestic".into(), 599);
        base_rates.insert("express".into(), 1499);
        base_rates.insert("international".into(), 2499);
        Self {
            base_rates,
            per_kg_rate: 200,
            free_shipping_threshold: 10000,
        }
    }

    /// Calculate the shipping rate in cents.
    pub fn calculate_rate(&self, address: &str, weight_kg: f64) -> u64 {
        let tier = self.classify_address(address);
        let base = self.base_rates.get(&tier).copied().unwrap_or(599);
        let weight_cost = (weight_kg * self.per_kg_rate as f64) as u64;
        base + weight_cost
    }

    /// Check if an order qualifies for free shipping.
    pub fn qualifies_for_free_shipping(&self, subtotal: u64) -> bool {
        subtotal >= self.free_shipping_threshold
    }

    /// Estimate delivery days based on shipping tier.
    pub fn estimate_delivery_days(&self, address: &str) -> u32 {
        match self.classify_address(address).as_str() {
            "domestic" => 5,
            "express" => 2,
            "international" => 14,
            _ => 7,
        }
    }

    fn classify_address(&self, address: &str) -> String {
        if address.contains("Express") {
            "express".into()
        } else if address.contains("International") || address.contains("CA ") || address.contains("UK ") {
            "international".into()
        } else {
            "domestic".into()
        }
    }
}
