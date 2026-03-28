pub mod validation;

use crate::inventory::InventoryService;
use crate::metrics::MetricsCollector;
use crate::notifications::NotificationService;
use crate::payments::PaymentProcessor;
use crate::shipping::ShippingCalculator;

#[derive(Debug, Clone, PartialEq)]
pub enum OrderStatus {
    Pending,
    Confirmed,
    Shipped,
    Cancelled,
    Refunded,
}

#[derive(Debug, Clone)]
pub struct OrderItem {
    pub sku: String,
    pub quantity: u32,
    pub price_cents: u64,
}

#[derive(Debug, Clone)]
pub struct Order {
    pub id: String,
    pub customer_id: String,
    pub items: Vec<OrderItem>,
    pub status: OrderStatus,
    pub shipping_address: String,
    pub payment_id: Option<String>,
    pub total_cents: u64,
}

impl Order {
    pub fn subtotal(&self) -> u64 {
        self.items.iter().map(|i| i.price_cents * i.quantity as u64).sum()
    }
}

pub struct OrderService {
    inventory: InventoryService,
    payments: PaymentProcessor,
    notifications: NotificationService,
    metrics: MetricsCollector,
    shipping: ShippingCalculator,
}

impl OrderService {
    pub fn new(
        inventory: InventoryService,
        payments: PaymentProcessor,
        notifications: NotificationService,
        metrics: MetricsCollector,
        shipping: ShippingCalculator,
    ) -> Self {
        Self { inventory, payments, notifications, metrics, shipping }
    }

    /// Create a new order: validate, reserve inventory, calculate shipping, charge payment.
    pub fn create_order(&mut self, mut order: Order) -> Result<Order, OrderError> {
        // Validate
        validation::validate_order(&order)?;

        // Reserve inventory for each item
        for item in &order.items {
            self.inventory.reserve(&item.sku, item.quantity)
                .map_err(|e| OrderError::InventoryError(e.to_string()))?;
        }

        // Calculate shipping
        let shipping = self.shipping.calculate_rate(
            &order.shipping_address,
            self.estimate_weight(&order),
        );
        order.total_cents = order.subtotal() + shipping;

        // Charge payment
        let payment_id = self.payments.charge(order.customer_id.clone(), order.total_cents)
            .map_err(|e| OrderError::PaymentError(e.to_string()))?;
        order.payment_id = Some(payment_id);
        order.status = OrderStatus::Confirmed;

        // Track and notify
        self.metrics.track_event("order_created", &order.id);
        self.notifications.send_order_confirmation(&order);

        Ok(order)
    }

    /// Cancel an order: release inventory, process refund, notify customer.
    pub fn cancel_order(&mut self, order: &mut Order) -> Result<(), OrderError> {
        if order.status == OrderStatus::Shipped {
            return Err(OrderError::AlreadyShipped);
        }
        if order.status == OrderStatus::Cancelled {
            return Err(OrderError::AlreadyCancelled);
        }

        // Release reserved inventory
        for item in &order.items {
            self.inventory.release(&item.sku, item.quantity);
        }

        // Mark as cancelled immediately
        order.status = OrderStatus::Cancelled;

        // Notify customer that order is cancelled
        self.notifications.send_cancellation_notice(&order);

        // Process refund (this can fail — external payment provider)
        if let Some(ref payment_id) = order.payment_id {
            self.payments.refund(payment_id, order.total_cents)
                .map_err(|e| OrderError::RefundError(e.to_string()))?;
            order.status = OrderStatus::Refunded;
        }

        self.metrics.track_event("order_cancelled", &order.id);

        Ok(())
    }

    /// Bulk cancel all pending orders for a customer.
    pub fn cancel_all_pending(&mut self, customer_id: &str, orders: &mut [Order]) -> Vec<OrderError> {
        let mut errors = Vec::new();
        for order in orders.iter_mut() {
            if order.customer_id == customer_id && order.status == OrderStatus::Pending {
                if let Err(e) = self.cancel_order(order) {
                    errors.push(e);
                    // Continues to next order even if one fails
                }
            }
        }
        if errors.is_empty() {
            self.notifications.send_bulk_cancellation_summary(customer_id, orders.len());
        }
        errors
    }

    fn estimate_weight(&self, order: &Order) -> f64 {
        order.items.iter().map(|i| i.quantity as f64 * 0.5).sum()
    }
}

#[derive(Debug)]
pub enum OrderError {
    ValidationError(String),
    InventoryError(String),
    PaymentError(String),
    RefundError(String),
    AlreadyShipped,
    AlreadyCancelled,
}
