use std::time::Duration;

use anyhow::Result;
use polymarket_client_sdk::clob::types::request::BalanceAllowanceRequest;
use polymarket_client_sdk::clob::types::{OrderStatusType, Side as ClobSide};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use tracing::{info, warn};

use crate::auth::ClobContext;
use crate::state::TradingState;
use crate::types::{ExecutionResult, ExecutionStatus, OrderSide, SimulatedOrder};

/// Delay between consecutive order submissions to avoid rate limits.
const INTER_ORDER_DELAY: Duration = Duration::from_millis(200);

/// Delay before checking order fill status.
const FILL_CHECK_DELAY: Duration = Duration::from_secs(2);

/// Maximum retry attempts for transient errors.
const MAX_RETRIES: u32 = 3;

/// Base backoff delay for retries (doubles each attempt).
const BASE_BACKOFF: Duration = Duration::from_millis(500);

/// Check USDC balance, returning the amount in dollars.
pub async fn check_balance(ctx: &ClobContext) -> Result<f64> {
    let bal = ctx
        .client
        .balance_allowance(BalanceAllowanceRequest::default())
        .await?;
    // Balance is in raw USDC units (6 decimals): 5000000 = $5.00
    let raw = bal.balance.to_f64().unwrap_or(0.0);
    Ok(raw / 1_000_000.0)
}

/// Convert f64 price to Decimal truncated to 2 decimal places.
fn f64_to_price(val: f64) -> Result<Decimal> {
    let d = Decimal::from_f64_retain(val)
        .ok_or_else(|| anyhow::anyhow!("cannot convert price {val} to Decimal"))?;
    Ok(d.trunc_with_scale(2))
}

/// Convert f64 shares to Decimal truncated to 2 decimal places.
fn f64_to_shares(val: f64) -> Result<Decimal> {
    let d = Decimal::from_f64_retain(val)
        .ok_or_else(|| anyhow::anyhow!("cannot convert shares {val} to Decimal"))?;
    let truncated = d.trunc_with_scale(2);
    if truncated.is_zero() {
        anyhow::bail!("shares truncated to zero from {val}");
    }
    Ok(truncated)
}

/// Check if an error message indicates a transient/retryable failure.
fn is_transient_error(err_str: &str) -> bool {
    let lower = err_str.to_lowercase();
    lower.contains("429")
        || lower.contains("too many requests")
        || lower.contains("500")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("504")
        || lower.contains("internal server error")
        || lower.contains("bad gateway")
        || lower.contains("service unavailable")
        || lower.contains("gateway timeout")
        || lower.contains("timeout")
        || lower.contains("connection")
        || lower.contains("timed out")
}

/// Map our internal `OrderSide` to the CLOB SDK `Side`.
fn to_clob_side(side: OrderSide) -> ClobSide {
    match side {
        OrderSide::Buy => ClobSide::Buy,
        OrderSide::Sell => ClobSide::Sell,
    }
}

/// Execute a list of simulated orders on the CLOB, returning results for each.
///
/// Orders are processed sequentially (sells first, then buys — matching engine output order).
/// A balance guard skips all buys if the account has < $1 USDC.
pub async fn execute_orders(
    ctx: &ClobContext,
    orders: &[SimulatedOrder],
) -> Vec<ExecutionResult> {
    let mut results = Vec::with_capacity(orders.len());

    // Find the index where buys start (all sells come first from compute_orders)
    let first_buy_idx = orders
        .iter()
        .position(|o| o.side == OrderSide::Buy)
        .unwrap_or(orders.len());

    // Balance guard: check before processing any buys
    let mut skip_buys = false;
    if first_buy_idx < orders.len() {
        match check_balance(ctx).await {
            Ok(balance) => {
                info!("USDC balance: ${balance:.2}");
                if balance < 1.0 {
                    warn!("Balance ${balance:.2} < $1.00 — skipping all buy orders");
                    skip_buys = true;
                }
            }
            Err(e) => {
                warn!("Failed to check balance: {e} — skipping all buy orders");
                skip_buys = true;
            }
        }
    }

    for (idx, order) in orders.iter().enumerate() {
        // Skip buys if balance guard triggered
        if order.side == OrderSide::Buy && skip_buys {
            results.push(ExecutionResult {
                order_index: idx,
                status: ExecutionStatus::Skipped,
                order_id: String::new(),
                filled_shares: 0.0,
                filled_cost_usd: 0.0,
                error_msg: Some("insufficient balance".into()),
            });
            continue;
        }

        let result = execute_single_order(ctx, idx, order).await;
        results.push(result);

        // Delay between orders to avoid rate limits (except after the last one)
        if idx + 1 < orders.len() {
            tokio::time::sleep(INTER_ORDER_DELAY).await;
        }
    }

    results
}

/// Execute a single order with retry logic.
async fn execute_single_order(
    ctx: &ClobContext,
    index: usize,
    order: &SimulatedOrder,
) -> ExecutionResult {
    let price = match f64_to_price(order.price) {
        Ok(p) => p,
        Err(e) => {
            return ExecutionResult {
                order_index: index,
                status: ExecutionStatus::Failed,
                order_id: String::new(),
                filled_shares: 0.0,
                filled_cost_usd: 0.0,
                error_msg: Some(format!("price conversion: {e}")),
            };
        }
    };

    let shares = match f64_to_shares(order.shares) {
        Ok(s) => s,
        Err(e) => {
            return ExecutionResult {
                order_index: index,
                status: ExecutionStatus::Failed,
                order_id: String::new(),
                filled_shares: 0.0,
                filled_cost_usd: 0.0,
                error_msg: Some(format!("shares conversion: {e}")),
            };
        }
    };

    let side = to_clob_side(order.side);
    let token_id = &order.market.asset;

    info!(
        "Placing {} order: {} shares @ ${} — \"{}\" ({})",
        order.side.label(),
        shares,
        price,
        order.market.title,
        order.market.outcome,
    );

    // Build, sign, and post with retry for transient errors
    let post_resp = match build_sign_post_with_retry(ctx, token_id, price, shares, side).await {
        Ok(resp) => resp,
        Err(e) => {
            return ExecutionResult {
                order_index: index,
                status: ExecutionStatus::Failed,
                order_id: String::new(),
                filled_shares: 0.0,
                filled_cost_usd: 0.0,
                error_msg: Some(format!("{e}")),
            };
        }
    };

    if !post_resp.success {
        let msg = post_resp
            .error_msg
            .unwrap_or_else(|| format!("status: {}", post_resp.status));
        warn!("Order post failed: {msg}");
        return ExecutionResult {
            order_index: index,
            status: ExecutionStatus::Failed,
            order_id: post_resp.order_id,
            filled_shares: 0.0,
            filled_cost_usd: 0.0,
            error_msg: Some(msg),
        };
    }

    let order_id = post_resp.order_id.clone();

    // If already matched at post time, return immediately
    if post_resp.status == OrderStatusType::Matched {
        let filled_shares = shares.to_f64().unwrap_or(order.shares);
        let filled_cost = filled_shares * order.price;
        info!("Order {order_id} filled immediately ({filled_shares} shares, ${filled_cost:.2})");
        return ExecutionResult {
            order_index: index,
            status: ExecutionStatus::Filled,
            order_id,
            filled_shares,
            filled_cost_usd: filled_cost,
            error_msg: None,
        };
    }

    // Wait and check fill status
    tokio::time::sleep(FILL_CHECK_DELAY).await;

    match ctx.client.order(&order_id).await {
        Ok(status) => {
            let size_matched = status.size_matched.to_f64().unwrap_or(0.0);
            let original_size = status.original_size.to_f64().unwrap_or(order.shares);
            let fill_price = status.price.to_f64().unwrap_or(order.price);

            match status.status {
                OrderStatusType::Matched => {
                    let filled_cost = size_matched * fill_price;
                    info!("Order {order_id} fully filled ({size_matched} shares, ${filled_cost:.2})");
                    ExecutionResult {
                        order_index: index,
                        status: ExecutionStatus::Filled,
                        order_id,
                        filled_shares: size_matched,
                        filled_cost_usd: filled_cost,
                        error_msg: None,
                    }
                }
                OrderStatusType::Live => {
                    if size_matched > 0.0 {
                        let filled_cost = size_matched * fill_price;
                        info!(
                            "Order {order_id} partially filled ({size_matched}/{original_size} shares, ${filled_cost:.2})"
                        );
                        ExecutionResult {
                            order_index: index,
                            status: ExecutionStatus::PartialFill,
                            order_id,
                            filled_shares: size_matched,
                            filled_cost_usd: filled_cost,
                            error_msg: None,
                        }
                    } else {
                        info!("Order {order_id} resting on book (0/{original_size} filled)");
                        ExecutionResult {
                            order_index: index,
                            status: ExecutionStatus::Resting,
                            order_id,
                            filled_shares: 0.0,
                            filled_cost_usd: 0.0,
                            error_msg: None,
                        }
                    }
                }
                OrderStatusType::Canceled | OrderStatusType::Unmatched => {
                    let filled_cost = size_matched * fill_price;
                    if size_matched > 0.0 {
                        info!(
                            "Order {order_id} cancelled with partial fill ({size_matched} shares, ${filled_cost:.2})"
                        );
                        ExecutionResult {
                            order_index: index,
                            status: ExecutionStatus::PartialFill,
                            order_id,
                            filled_shares: size_matched,
                            filled_cost_usd: filled_cost,
                            error_msg: None,
                        }
                    } else {
                        warn!("Order {order_id} cancelled/unmatched with no fills");
                        ExecutionResult {
                            order_index: index,
                            status: ExecutionStatus::Failed,
                            order_id,
                            filled_shares: 0.0,
                            filled_cost_usd: 0.0,
                            error_msg: Some(format!("order {}", status.status)),
                        }
                    }
                }
                _ => {
                    // Delayed or unknown — optimistic assumption: treat as filled
                    warn!(
                        "Order {order_id} in unexpected status {} — assuming filled",
                        status.status
                    );
                    let filled_shares = shares.to_f64().unwrap_or(order.shares);
                    let filled_cost = filled_shares * order.price;
                    ExecutionResult {
                        order_index: index,
                        status: ExecutionStatus::Filled,
                        order_id,
                        filled_shares,
                        filled_cost_usd: filled_cost,
                        error_msg: None,
                    }
                }
            }
        }
        Err(e) => {
            // Status query failed but post succeeded — optimistic assumption
            warn!("Failed to check order {order_id} status: {e} — assuming filled");
            let filled_shares = shares.to_f64().unwrap_or(order.shares);
            let filled_cost = filled_shares * order.price;
            ExecutionResult {
                order_index: index,
                status: ExecutionStatus::Filled,
                order_id,
                filled_shares,
                filled_cost_usd: filled_cost,
                error_msg: Some(format!("status check failed: {e}")),
            }
        }
    }
}

/// Build, sign, and post a limit order with exponential backoff retry for transient errors.
///
/// Re-builds and re-signs on each retry attempt since `SignedOrder` is not `Clone`.
async fn build_sign_post_with_retry(
    ctx: &ClobContext,
    token_id: &str,
    price: Decimal,
    shares: Decimal,
    side: ClobSide,
) -> Result<polymarket_client_sdk::clob::types::response::PostOrderResponse> {
    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 0..MAX_RETRIES {
        let signable = ctx
            .client
            .limit_order()
            .token_id(token_id)
            .price(price)
            .size(shares)
            .side(side)
            .build()
            .await
            .map_err(|e| anyhow::anyhow!("build order: {e}"))?;

        let signed = ctx
            .client
            .sign(&ctx.signer, signable)
            .await
            .map_err(|e| anyhow::anyhow!("sign order: {e}"))?;

        match ctx.client.post_order(signed).await {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                let err_str = e.to_string();
                if is_transient_error(&err_str) && attempt + 1 < MAX_RETRIES {
                    let delay = BASE_BACKOFF * 2u32.pow(attempt);
                    warn!(
                        "Transient error posting order (attempt {}/{}): {err_str} — retrying in {:?}",
                        attempt + 1,
                        MAX_RETRIES,
                        delay,
                    );
                    tokio::time::sleep(delay).await;
                    last_err = Some(anyhow::anyhow!(e));
                } else {
                    return Err(anyhow::anyhow!("post order: {e}"));
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("retry exhausted")))
}

/// Check all resting orders and resolve any that have filled or been cancelled.
///
/// Queries the CLOB API for each resting order's current status. Updates TradingState:
/// - Filled → moves to holdings (budget already reserved for buys)
/// - Cancelled → returns reserved budget (buys), removes tracking
/// - Still resting → no change
pub async fn check_resting_orders(ctx: &ClobContext, state: &mut TradingState) {
    if state.resting_orders.is_empty() {
        return;
    }

    info!(
        "Checking {} resting order(s)...",
        state.resting_orders.len()
    );

    // Collect order IDs first to avoid borrow issues
    let order_ids: Vec<String> = state
        .resting_orders
        .iter()
        .map(|r| r.order_id.clone())
        .collect();

    for order_id in order_ids {
        match ctx.client.order(&order_id).await {
            Ok(status) => {
                let size_matched = status.size_matched.to_f64().unwrap_or(0.0);
                let fill_price = status.price.to_f64().unwrap_or(0.0);

                match status.status {
                    OrderStatusType::Matched => {
                        info!(
                            "Resting order {order_id} filled ({size_matched} shares @ ${fill_price:.2})"
                        );
                        state.resolve_resting_fill(&order_id, size_matched, fill_price);
                    }
                    OrderStatusType::Live => {
                        if size_matched > 0.0 {
                            // Partial fill on a still-live order — don't resolve yet,
                            // wait for full fill or cancellation
                            info!(
                                "Resting order {order_id} partially filled ({size_matched} shares), still live"
                            );
                        }
                        // else: still fully resting, no action needed
                    }
                    OrderStatusType::Canceled | OrderStatusType::Unmatched => {
                        if size_matched > 0.0 {
                            info!(
                                "Resting order {order_id} cancelled with partial fill ({size_matched} shares)"
                            );
                            state.resolve_resting_fill(&order_id, size_matched, fill_price);
                        } else {
                            info!("Resting order {order_id} cancelled with no fills");
                            state.resolve_resting_cancel(&order_id);
                        }
                    }
                    _ => {
                        warn!(
                            "Resting order {order_id} in unexpected status: {}",
                            status.status
                        );
                    }
                }
            }
            Err(e) => {
                warn!("Failed to check resting order {order_id}: {e}");
                // Leave it tracked — will retry next cycle
            }
        }
    }

    if !state.resting_orders.is_empty() {
        info!(
            "{} order(s) still resting on book",
            state.resting_orders.len()
        );
    }
}

impl OrderSide {
    fn label(self) -> &'static str {
        match self {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        }
    }
}
