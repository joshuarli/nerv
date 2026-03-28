# OrderFlow

E-commerce order processing system.

## Structure

```
src/
  lib.rs
  orders/
    mod.rs              # Order, OrderService (create, cancel, bulk_cancel)
    validation.rs       # Order validation, fraud scoring
  inventory/
    mod.rs              # InventoryService (reserve, release, deduct)
    cache.rs            # InventoryCache for storefront reads
  payments/
    mod.rs              # PaymentProcessor (charge, refund)
    retry.rs            # RetryPolicy with exponential backoff
  notifications/
    mod.rs              # NotificationService (email/SMS queue)
  metrics/
    mod.rs              # MetricsCollector (counters, timings)
  shipping/
    mod.rs              # ShippingCalculator (rates, delivery estimates)
```

## Build

```
cargo check
```
