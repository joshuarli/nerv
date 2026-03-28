pub mod retry;

use retry::RetryPolicy;

/// Processes payments through an external payment gateway.
pub struct PaymentProcessor {
    api_key: String,
    retry_policy: RetryPolicy,
    gateway_url: String,
}

#[derive(Debug, Clone)]
pub struct PaymentRecord {
    pub id: String,
    pub customer_id: String,
    pub amount_cents: u64,
    pub status: PaymentStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PaymentStatus {
    Pending,
    Charged,
    Refunded,
    Failed,
    Disputed,
}

impl PaymentProcessor {
    pub fn new(api_key: String, gateway_url: String) -> Self {
        Self {
            api_key,
            retry_policy: RetryPolicy::default(),
            gateway_url,
        }
    }

    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Charge a customer. Returns payment ID on success.
    /// Retries on transient failures according to the retry policy.
    pub fn charge(&self, customer_id: String, amount_cents: u64) -> Result<String, PaymentError> {
        let mut last_error = None;

        for attempt in 0..=self.retry_policy.max_retries {
            match self.attempt_charge(&customer_id, amount_cents) {
                Ok(id) => return Ok(id),
                Err(e) if e.is_transient() && attempt < self.retry_policy.max_retries => {
                    let delay = self.retry_policy.delay_for_attempt(attempt);
                    std::thread::sleep(delay);
                    last_error = Some(e);
                }
                Err(e) => return Err(e),
            }
        }

        Err(last_error.unwrap_or(PaymentError::Unknown))
    }

    /// Refund a previous charge. No retry — refunds are idempotent at the
    /// gateway level, so failed refunds should be investigated manually.
    pub fn refund(&self, payment_id: &str, amount_cents: u64) -> Result<(), PaymentError> {
        // Simulate external API call
        if payment_id.is_empty() {
            return Err(PaymentError::InvalidPaymentId);
        }
        let _ = (amount_cents, &self.gateway_url, &self.api_key);
        Ok(())
    }

    /// Check the status of a payment.
    pub fn check_status(&self, payment_id: &str) -> Result<PaymentStatus, PaymentError> {
        if payment_id.is_empty() {
            return Err(PaymentError::InvalidPaymentId);
        }
        // Simulate status check
        Ok(PaymentStatus::Charged)
    }

    fn attempt_charge(&self, customer_id: &str, amount_cents: u64) -> Result<String, PaymentError> {
        // Simulate external API call
        let _ = (&self.gateway_url, &self.api_key, customer_id, amount_cents);
        Ok(format!("pay_{}", uuid_v4()))
    }
}

#[derive(Debug)]
pub enum PaymentError {
    InsufficientFunds,
    CardDeclined,
    GatewayTimeout,
    NetworkError(String),
    InvalidPaymentId,
    Unknown,
}

impl PaymentError {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::GatewayTimeout | Self::NetworkError(_))
    }
}

impl std::fmt::Display for PaymentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsufficientFunds => write!(f, "insufficient funds"),
            Self::CardDeclined => write!(f, "card declined"),
            Self::GatewayTimeout => write!(f, "gateway timeout"),
            Self::NetworkError(msg) => write!(f, "network error: {}", msg),
            Self::InvalidPaymentId => write!(f, "invalid payment ID"),
            Self::Unknown => write!(f, "unknown payment error"),
        }
    }
}

fn uuid_v4() -> String {
    format!("{:08x}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u32)
}
