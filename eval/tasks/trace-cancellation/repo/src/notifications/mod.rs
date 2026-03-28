use std::collections::VecDeque;

/// Sends notifications to customers via email/SMS.
/// Messages are queued and dispatched asynchronously (in production).
pub struct NotificationService {
    queue: VecDeque<Notification>,
    templates: NotificationTemplates,
}

#[derive(Debug, Clone)]
pub struct Notification {
    pub recipient: String,
    pub subject: String,
    pub body: String,
    pub channel: Channel,
    pub priority: Priority,
}

#[derive(Debug, Clone)]
pub enum Channel {
    Email,
    Sms,
    Push,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Low,
    Normal,
    High,
    Urgent,
}

struct NotificationTemplates {
    order_confirmed: String,
    order_cancelled: String,
    refund_processed: String,
    shipping_update: String,
}

impl Default for NotificationTemplates {
    fn default() -> Self {
        Self {
            order_confirmed: "Your order {{order_id}} has been confirmed.".into(),
            order_cancelled: "Your order {{order_id}} has been cancelled.".into(),
            refund_processed: "Refund of {{amount}} has been processed for order {{order_id}}.".into(),
            shipping_update: "Your order {{order_id}} has shipped. Tracking: {{tracking}}".into(),
        }
    }
}

impl NotificationService {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            templates: NotificationTemplates::default(),
        }
    }

    pub fn send_order_confirmation(&mut self, order: &crate::orders::Order) {
        let body = self.templates.order_confirmed
            .replace("{{order_id}}", &order.id);
        self.enqueue(Notification {
            recipient: order.customer_id.clone(),
            subject: format!("Order {} confirmed", order.id),
            body,
            channel: Channel::Email,
            priority: Priority::Normal,
        });
    }

    /// Sends cancellation notice to the customer.
    /// WARNING: This fires immediately — does not wait for refund to complete.
    pub fn send_cancellation_notice(&mut self, order: &crate::orders::Order) {
        let body = self.templates.order_cancelled
            .replace("{{order_id}}", &order.id);
        self.enqueue(Notification {
            recipient: order.customer_id.clone(),
            subject: format!("Order {} cancelled", order.id),
            body,
            channel: Channel::Email,
            priority: Priority::High,
        });
    }

    pub fn send_refund_confirmation(&mut self, order: &crate::orders::Order, amount_cents: u64) {
        let body = self.templates.refund_processed
            .replace("{{order_id}}", &order.id)
            .replace("{{amount}}", &format!("${:.2}", amount_cents as f64 / 100.0));
        self.enqueue(Notification {
            recipient: order.customer_id.clone(),
            subject: format!("Refund processed for order {}", order.id),
            body,
            channel: Channel::Email,
            priority: Priority::High,
        });
    }

    pub fn send_shipping_update(&mut self, order: &crate::orders::Order, tracking: &str) {
        let body = self.templates.shipping_update
            .replace("{{order_id}}", &order.id)
            .replace("{{tracking}}", tracking);
        self.enqueue(Notification {
            recipient: order.customer_id.clone(),
            subject: format!("Order {} shipped", order.id),
            body,
            channel: Channel::Email,
            priority: Priority::Normal,
        });
    }

    pub fn send_bulk_cancellation_summary(&mut self, customer_id: &str, count: usize) {
        self.enqueue(Notification {
            recipient: customer_id.to_string(),
            subject: format!("{} orders cancelled", count),
            body: format!("{} of your pending orders have been cancelled.", count),
            channel: Channel::Email,
            priority: Priority::High,
        });
    }

    pub fn pending_count(&self) -> usize {
        self.queue.len()
    }

    pub fn drain_queue(&mut self) -> Vec<Notification> {
        self.queue.drain(..).collect()
    }

    fn enqueue(&mut self, notification: Notification) {
        self.queue.push_back(notification);
    }
}
