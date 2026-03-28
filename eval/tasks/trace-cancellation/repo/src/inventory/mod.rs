pub mod cache;

use std::collections::HashMap;

/// Manages physical inventory levels.
pub struct InventoryService {
    stock: HashMap<String, StockRecord>,
    low_stock_threshold: u32,
}

#[derive(Debug, Clone)]
pub struct StockRecord {
    pub sku: String,
    pub available: u32,
    pub reserved: u32,
    pub warehouse: String,
}

impl StockRecord {
    pub fn effective_available(&self) -> u32 {
        self.available.saturating_sub(self.reserved)
    }
}

impl InventoryService {
    pub fn new() -> Self {
        Self {
            stock: HashMap::new(),
            low_stock_threshold: 10,
        }
    }

    pub fn set_threshold(&mut self, threshold: u32) {
        self.low_stock_threshold = threshold;
    }

    /// Reserve inventory for an order. Returns error if insufficient stock.
    pub fn reserve(&mut self, sku: &str, quantity: u32) -> Result<(), InventoryError> {
        let record = self.stock.get_mut(sku)
            .ok_or_else(|| InventoryError::SkuNotFound(sku.to_string()))?;

        if record.effective_available() < quantity {
            return Err(InventoryError::InsufficientStock {
                sku: sku.to_string(),
                requested: quantity,
                available: record.effective_available(),
            });
        }

        record.reserved += quantity;

        if record.effective_available() <= self.low_stock_threshold {
            self.trigger_reorder_alert(sku);
        }

        Ok(())
    }

    /// Release previously reserved inventory (e.g., on cancellation).
    /// Silently ignores over-release — does not track who reserved what.
    pub fn release(&mut self, sku: &str, quantity: u32) {
        if let Some(record) = self.stock.get_mut(sku) {
            record.reserved = record.reserved.saturating_sub(quantity);
        }
        // NOTE: no error if SKU doesn't exist — fire and forget
    }

    /// Permanently reduce stock (after shipping).
    pub fn deduct(&mut self, sku: &str, quantity: u32) -> Result<(), InventoryError> {
        let record = self.stock.get_mut(sku)
            .ok_or_else(|| InventoryError::SkuNotFound(sku.to_string()))?;

        if record.available < quantity {
            return Err(InventoryError::InsufficientStock {
                sku: sku.to_string(),
                requested: quantity,
                available: record.available,
            });
        }

        record.available -= quantity;
        record.reserved = record.reserved.saturating_sub(quantity);
        Ok(())
    }

    pub fn add_stock(&mut self, sku: String, available: u32, warehouse: String) {
        self.stock.insert(sku.clone(), StockRecord {
            sku,
            available,
            reserved: 0,
            warehouse,
        });
    }

    pub fn get_stock(&self, sku: &str) -> Option<&StockRecord> {
        self.stock.get(sku)
    }

    pub fn low_stock_items(&self) -> Vec<&StockRecord> {
        self.stock.values()
            .filter(|r| r.effective_available() <= self.low_stock_threshold)
            .collect()
    }

    fn trigger_reorder_alert(&self, sku: &str) {
        // In production, this would send an alert to the warehouse
        eprintln!("[inventory] low stock alert: {}", sku);
    }
}

#[derive(Debug)]
pub enum InventoryError {
    SkuNotFound(String),
    InsufficientStock {
        sku: String,
        requested: u32,
        available: u32,
    },
}

impl std::fmt::Display for InventoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SkuNotFound(sku) => write!(f, "SKU not found: {}", sku),
            Self::InsufficientStock { sku, requested, available } => {
                write!(f, "Insufficient stock for {}: requested {}, available {}", sku, requested, available)
            }
        }
    }
}
