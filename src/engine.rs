use std::collections::HashMap;

use polymarket_client_sdk::data::types::response::Position;
use rust_decimal::prelude::ToPrimitive;
use tracing::{info, warn};

use crate::state::TradingState;
use crate::types::{MarketPosition, OrderSide, SimulatedOrder, TargetAllocation};

/// Minimum order value in USD — Polymarket CLOB rejects orders below $1 notional.
const MIN_ORDER_USD: f64 = 1.00;

/// Extract a `MarketPosition` from an SDK `Position`.
fn extract_market(pos: &Position) -> MarketPosition {
    MarketPosition {
        condition_id: format!("{}", pos.condition_id),
        asset: pos.asset.to_string(),
        title: pos.title.clone(),
        outcome: pos.outcome.clone(),
        outcome_index: pos.outcome_index,
        event_slug: pos.event_slug.clone(),
    }
}

/// Compute portfolio weights from active positions.
///
/// Returns `(MarketPosition, weight, cur_price)` tuples where weight is
/// `current_value / total_portfolio_value`.
pub fn compute_weights(positions: &[Position]) -> Vec<(MarketPosition, f64, f64)> {
    let total_value: f64 = positions
        .iter()
        .map(|p| p.current_value.to_f64().unwrap_or(0.0))
        .sum();

    if total_value <= 0.0 {
        return Vec::new();
    }

    positions
        .iter()
        .map(|p| {
            let value = p.current_value.to_f64().unwrap_or(0.0);
            let weight = value / total_value;
            let price = p.cur_price.to_f64().unwrap_or(0.0);
            (extract_market(p), weight, price)
        })
        .collect()
}

/// Compute the target state (allocation per market) given weights and parameters.
///
/// `max_trade_pct` is the maximum fraction (0.0–1.0) of `budget` allocatable to
/// any single market position.
pub fn compute_target_state(
    weights: &[(MarketPosition, f64, f64)],
    budget: f64,
    copy_pct: f64,
    max_trade_pct: f64,
) -> Vec<TargetAllocation> {
    let max_per_market = max_trade_pct * budget;
    weights
        .iter()
        .map(|(market, weight, cur_price)| {
            let raw_target = weight * budget * copy_pct;
            let target_usd = raw_target.min(max_per_market);
            let target_shares = if *cur_price > 0.0 {
                target_usd / cur_price
            } else {
                0.0
            };

            TargetAllocation {
                market: market.clone(),
                trader_weight: *weight,
                target_value_usd: target_usd,
                target_shares,
                cur_price: *cur_price,
            }
        })
        .collect()
}

/// Compute the diff between target allocations and current holdings, producing
/// simulated orders. Processes sells first (to free budget), then buys.
///
/// `price_map` provides real market prices for assets the trader has exited.
/// Used instead of `avg_cost` to get accurate realized P&L on exits.
pub fn compute_orders(
    targets: &[TargetAllocation],
    state: &TradingState,
    budget_remaining: f64,
    price_map: &HashMap<String, f64>,
    trader_short_id: &str,
) -> Vec<SimulatedOrder> {
    let mut sells = Vec::new();
    let mut buys = Vec::new();

    // Build a set of target assets for detecting exits
    let target_assets: std::collections::HashSet<&str> =
        targets.iter().map(|t| t.market.asset.as_str()).collect();

    // For each target, compare with effective holdings (includes resting orders)
    for target in targets {
        let held_shares = state.effective_held_shares(&target.market.asset);

        let diff = target.target_shares - held_shares;

        if diff > 0.0 {
            // Need to buy more — subject to $1 minimum notional
            let cost = diff * target.cur_price;
            if cost >= MIN_ORDER_USD {
                buys.push(SimulatedOrder {
                    market: target.market.clone(),
                    side: OrderSide::Buy,
                    shares: diff,
                    price: target.cur_price,
                    cost_usd: cost,
                });
            }
        } else if diff < 0.0 {
            // Need to sell some — no minimum for sells (CLOB allows closing below $1)
            let sell_shares = -diff;
            let proceeds = sell_shares * target.cur_price;
            sells.push(SimulatedOrder {
                market: target.market.clone(),
                side: OrderSide::Sell,
                shares: sell_shares,
                price: target.cur_price,
                cost_usd: proceeds,
            });
        }
    }

    // Sell holdings that the trader has exited entirely
    for (asset, held) in &state.holdings {
        if !target_assets.contains(asset.as_str()) && held.shares > 0.0 {
            // Use effective shares to account for any resting sell orders
            let effective = state.effective_held_shares(asset);
            if effective <= 0.0 {
                continue; // already covered by a resting sell
            }
            let price = match price_map.get(asset) {
                Some(&p) => p,
                None => {
                    warn!(
                        "[{trader_short_id}] No market price for exited asset {} ({}), skipping sell",
                        asset, held.title
                    );
                    continue;
                }
            };
            let reason = if price == 0.0 || price == 1.0 {
                "resolved"
            } else {
                "trader exited"
            };
            info!(
                "[{trader_short_id}] Position exit: \"{}\" ({}) — price: {price:.4} ({reason})",
                held.title, held.outcome
            );
            let proceeds = effective * price;
            sells.push(SimulatedOrder {
                market: MarketPosition {
                    condition_id: String::new(),
                    asset: asset.clone(),
                    title: held.title.clone(),
                    outcome: held.outcome.clone(),
                    outcome_index: 0,
                    event_slug: String::new(),
                },
                side: OrderSide::Sell,
                shares: effective,
                price,
                cost_usd: proceeds,
            });
        }
    }

    // Process sells first (frees budget), then buys (consumes budget)
    let mut orders = Vec::new();
    let mut available = budget_remaining;

    // All sells go through — they free budget
    for sell in sells {
        available += sell.cost_usd;
        orders.push(sell);
    }

    // Buys are capped by available budget
    for buy in buys {
        if available < MIN_ORDER_USD {
            break;
        }
        if buy.cost_usd <= available {
            available -= buy.cost_usd;
            orders.push(buy);
        } else {
            // Partial fill: buy what we can afford
            let affordable_shares = available / buy.price;
            let cost = affordable_shares * buy.price;
            if cost >= MIN_ORDER_USD {
                orders.push(SimulatedOrder {
                    shares: affordable_shares,
                    cost_usd: cost,
                    ..buy
                });
                available -= cost;
            }
        }
    }

    orders
}
