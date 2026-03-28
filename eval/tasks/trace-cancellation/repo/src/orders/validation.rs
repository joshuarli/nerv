use super::{Order, OrderError, OrderStatus};

/// Validates an order before processing.
pub fn validate_order(order: &Order) -> Result<(), OrderError> {
    if order.items.is_empty() {
        return Err(OrderError::ValidationError("Order has no items".into()));
    }

    if order.customer_id.is_empty() {
        return Err(OrderError::ValidationError("Missing customer ID".into()));
    }

    if order.shipping_address.is_empty() {
        return Err(OrderError::ValidationError("Missing shipping address".into()));
    }

    if order.status != OrderStatus::Pending {
        return Err(OrderError::ValidationError(
            format!("Order must be Pending, got {:?}", order.status),
        ));
    }

    for item in &order.items {
        validate_item(item)?;
    }

    Ok(())
}

fn validate_item(item: &super::OrderItem) -> Result<(), OrderError> {
    if item.quantity == 0 {
        return Err(OrderError::ValidationError(
            format!("Item {} has zero quantity", item.sku),
        ));
    }
    if item.price_cents == 0 {
        return Err(OrderError::ValidationError(
            format!("Item {} has zero price", item.sku),
        ));
    }
    if item.sku.is_empty() {
        return Err(OrderError::ValidationError("Item has empty SKU".into()));
    }
    Ok(())
}

/// Check if a customer is eligible for express processing.
pub fn is_express_eligible(order: &Order) -> bool {
    order.items.len() <= 3
        && order.subtotal() < 50000
        && !order.shipping_address.contains("PO Box")
}

/// Estimate fraud risk score (0.0 = safe, 1.0 = high risk).
pub fn fraud_risk_score(order: &Order) -> f64 {
    let mut score: f64 = 0.0;

    // High value orders
    if order.subtotal() > 100000 {
        score += 0.3;
    }

    // Many distinct items
    if order.items.len() > 10 {
        score += 0.2;
    }

    // Large quantities of single item
    if order.items.iter().any(|i| i.quantity > 50) {
        score += 0.4;
    }

    score.min(1.0)
}
