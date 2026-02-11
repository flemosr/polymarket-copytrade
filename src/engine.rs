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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{HeldPosition, RestingOrder};
    use serde_json::json;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// Build an SDK `Position` via JSON deserialization (struct is #[non_exhaustive]).
    fn make_test_position(
        asset: &str,
        condition_id: &str,
        title: &str,
        outcome: &str,
        outcome_index: i32,
        event_slug: &str,
        cur_price: f64,
        current_value: f64,
    ) -> Position {
        serde_json::from_value(json!({
            "proxyWallet": "0x0000000000000000000000000000000000000001",
            "asset": asset,
            "conditionId": condition_id,
            "size": "100",
            "avgPrice": "0.50",
            "initialValue": "50",
            "currentValue": current_value.to_string(),
            "cashPnl": "0",
            "percentPnl": "0",
            "totalBought": "100",
            "realizedPnl": "0",
            "percentRealizedPnl": "0",
            "curPrice": cur_price.to_string(),
            "redeemable": false,
            "mergeable": false,
            "title": title,
            "slug": "test-market",
            "icon": "",
            "eventSlug": event_slug,
            "outcome": outcome,
            "outcomeIndex": outcome_index,
            "oppositeOutcome": "No",
            "oppositeAsset": "0xopposite",
            "endDate": "2025-12-31",
            "negativeRisk": false
        }))
        .expect("valid test Position JSON")
    }

    fn make_market(asset: &str) -> MarketPosition {
        MarketPosition {
            condition_id: String::new(),
            asset: asset.to_string(),
            title: String::new(),
            outcome: String::new(),
            outcome_index: 0,
            event_slug: String::new(),
        }
    }

    // ── compute_weights ────────────────────────────────────────────

    #[test]
    fn weights_empty() {
        let w = compute_weights(&[]);
        assert!(w.is_empty());
    }

    #[test]
    fn weights_single_position() {
        let pos = make_test_position("a1", "c1", "T", "Yes", 0, "slug", 0.50, 100.0);
        let w = compute_weights(&[pos]);
        assert_eq!(w.len(), 1);
        assert!(approx_eq(w[0].1, 1.0));
        assert!(approx_eq(w[0].2, 0.50));
    }

    #[test]
    fn weights_two_equal() {
        let p1 = make_test_position("a1", "c1", "T1", "Yes", 0, "s", 0.40, 50.0);
        let p2 = make_test_position("a2", "c2", "T2", "No", 1, "s", 0.60, 50.0);
        let w = compute_weights(&[p1, p2]);
        assert_eq!(w.len(), 2);
        assert!(approx_eq(w[0].1, 0.5));
        assert!(approx_eq(w[1].1, 0.5));
    }

    #[test]
    fn weights_uneven() {
        let p1 = make_test_position("a1", "c1", "T1", "Yes", 0, "s", 0.50, 300.0);
        let p2 = make_test_position("a2", "c2", "T2", "No", 1, "s", 0.50, 100.0);
        let w = compute_weights(&[p1, p2]);
        assert!(approx_eq(w[0].1, 0.75));
        assert!(approx_eq(w[1].1, 0.25));
    }

    #[test]
    fn weights_zero_total_value() {
        let p1 = make_test_position("a1", "c1", "T1", "Yes", 0, "s", 0.50, 0.0);
        let p2 = make_test_position("a2", "c2", "T2", "No", 1, "s", 0.50, 0.0);
        let w = compute_weights(&[p1, p2]);
        assert!(w.is_empty());
    }

    #[test]
    fn weights_preserves_fields() {
        let pos = make_test_position(
            "token123",
            "cond456",
            "Will it rain?",
            "Yes",
            0,
            "rain-event",
            0.70,
            100.0,
        );
        let w = compute_weights(&[pos]);
        assert_eq!(w[0].0.asset, "token123");
        assert_eq!(w[0].0.condition_id, "cond456");
        assert_eq!(w[0].0.title, "Will it rain?");
        assert_eq!(w[0].0.outcome, "Yes");
        assert_eq!(w[0].0.outcome_index, 0);
        assert_eq!(w[0].0.event_slug, "rain-event");
    }

    // ── compute_target_state ───────────────────────────────────────

    #[test]
    fn target_basic() {
        let weights = vec![(make_market("a1"), 0.5, 0.50)];
        let targets = compute_target_state(&weights, 1000.0, 1.0, 1.0);
        assert_eq!(targets.len(), 1);
        assert!(approx_eq(targets[0].target_value_usd, 500.0));
        assert!(approx_eq(targets[0].target_shares, 1000.0)); // 500 / 0.50
    }

    #[test]
    fn target_copy_percentage() {
        let weights = vec![(make_market("a1"), 1.0, 0.50)];
        let targets = compute_target_state(&weights, 1000.0, 0.5, 1.0);
        assert!(approx_eq(targets[0].target_value_usd, 500.0));
    }

    #[test]
    fn target_max_trade_caps() {
        let weights = vec![(make_market("a1"), 1.0, 0.50)];
        let targets = compute_target_state(&weights, 1000.0, 1.0, 0.30);
        assert!(approx_eq(targets[0].target_value_usd, 300.0)); // capped at 30%
    }

    #[test]
    fn target_zero_price() {
        let weights = vec![(make_market("a1"), 1.0, 0.0)];
        let targets = compute_target_state(&weights, 1000.0, 1.0, 1.0);
        assert!(approx_eq(targets[0].target_shares, 0.0));
    }

    #[test]
    fn target_multiple_markets() {
        let weights = vec![
            (make_market("a1"), 0.5, 0.40),
            (make_market("a2"), 0.3, 0.60),
            (make_market("a3"), 0.2, 0.80),
        ];
        let targets = compute_target_state(&weights, 1000.0, 1.0, 1.0);
        assert_eq!(targets.len(), 3);
        assert!(approx_eq(targets[0].target_value_usd, 500.0));
        assert!(approx_eq(targets[1].target_value_usd, 300.0));
        assert!(approx_eq(targets[2].target_value_usd, 200.0));
        // Shares = usd / price
        assert!(approx_eq(targets[0].target_shares, 1250.0));
        assert!(approx_eq(targets[1].target_shares, 500.0));
        assert!(approx_eq(targets[2].target_shares, 250.0));
    }

    #[test]
    fn target_preserves_fields() {
        let mut m = make_market("xyz");
        m.title = "My Market".to_string();
        m.outcome = "Yes".to_string();
        let weights = vec![(m, 1.0, 0.50)];
        let targets = compute_target_state(&weights, 100.0, 1.0, 1.0);
        assert_eq!(targets[0].market.asset, "xyz");
        assert_eq!(targets[0].market.title, "My Market");
        assert_eq!(targets[0].market.outcome, "Yes");
        assert!(approx_eq(targets[0].trader_weight, 1.0));
        assert!(approx_eq(targets[0].cur_price, 0.50));
    }

    // ── compute_orders ─────────────────────────────────────────────

    #[test]
    fn orders_initial_replication() {
        let state = TradingState::new(1000.0);
        let targets = vec![
            TargetAllocation {
                market: make_market("a1"),
                trader_weight: 0.5,
                target_value_usd: 500.0,
                target_shares: 1000.0,
                cur_price: 0.50,
            },
            TargetAllocation {
                market: make_market("a2"),
                trader_weight: 0.5,
                target_value_usd: 500.0,
                target_shares: 500.0,
                cur_price: 1.0,
            },
        ];
        let orders = compute_orders(&targets, &state, 1000.0, &HashMap::new(), "test");
        assert_eq!(orders.len(), 2);
        assert!(orders.iter().all(|o| o.side == OrderSide::Buy));
    }

    #[test]
    fn orders_sell_before_buy() {
        let mut state = TradingState::new(1000.0);
        // Hold 20 shares of a1
        state.holdings.insert(
            "a1".to_string(),
            HeldPosition {
                asset: "a1".to_string(),
                title: String::new(),
                outcome: String::new(),
                shares: 20.0,
                total_cost: 10.0,
                avg_cost: 0.50,
            },
        );
        let targets = vec![
            TargetAllocation {
                market: make_market("a1"),
                trader_weight: 0.5,
                target_value_usd: 5.0,
                target_shares: 10.0,
                cur_price: 0.50,
            },
            TargetAllocation {
                market: make_market("a2"),
                trader_weight: 0.5,
                target_value_usd: 5.0,
                target_shares: 10.0,
                cur_price: 0.50,
            },
        ];
        let orders = compute_orders(&targets, &state, 0.0, &HashMap::new(), "test");
        // First order should be a sell (sells come before buys)
        assert!(!orders.is_empty());
        assert_eq!(orders[0].side, OrderSide::Sell);
        assert_eq!(orders[0].market.asset, "a1");
    }

    #[test]
    fn orders_exit_sell_trader_exited() {
        let mut state = TradingState::new(1000.0);
        state.holdings.insert(
            "a1".to_string(),
            HeldPosition {
                asset: "a1".to_string(),
                title: "Exited Market".to_string(),
                outcome: "Yes".to_string(),
                shares: 10.0,
                total_cost: 5.0,
                avg_cost: 0.50,
            },
        );
        // No targets (trader has exited), but price_map has the asset
        let mut price_map = HashMap::new();
        price_map.insert("a1".to_string(), 0.60);
        let orders = compute_orders(&[], &state, 1000.0, &price_map, "test");
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].side, OrderSide::Sell);
        assert_eq!(orders[0].market.asset, "a1");
        assert!(approx_eq(orders[0].shares, 10.0));
        assert!(approx_eq(orders[0].price, 0.60));
    }

    #[test]
    fn orders_exit_sell_resolved_zero() {
        let mut state = TradingState::new(1000.0);
        state.holdings.insert(
            "a1".to_string(),
            HeldPosition {
                asset: "a1".to_string(),
                title: "Resolved".to_string(),
                outcome: "Yes".to_string(),
                shares: 10.0,
                total_cost: 5.0,
                avg_cost: 0.50,
            },
        );
        let mut price_map = HashMap::new();
        price_map.insert("a1".to_string(), 0.0);
        let orders = compute_orders(&[], &state, 1000.0, &price_map, "test");
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].side, OrderSide::Sell);
        assert!(approx_eq(orders[0].price, 0.0));
        assert!(approx_eq(orders[0].cost_usd, 0.0)); // no proceeds
    }

    #[test]
    fn orders_min_order_usd_buy() {
        let state = TradingState::new(1000.0);
        // Target buy worth $0.50 — below $1 minimum
        let targets = vec![TargetAllocation {
            market: make_market("a1"),
            trader_weight: 1.0,
            target_value_usd: 0.50,
            target_shares: 1.0,
            cur_price: 0.50,
        }];
        let orders = compute_orders(&targets, &state, 1000.0, &HashMap::new(), "test");
        assert!(orders.is_empty()); // skipped due to minimum
    }

    #[test]
    fn orders_no_minimum_for_sells() {
        let mut state = TradingState::new(1000.0);
        state.holdings.insert(
            "a1".to_string(),
            HeldPosition {
                asset: "a1".to_string(),
                title: String::new(),
                outcome: String::new(),
                shares: 10.0,
                total_cost: 5.0,
                avg_cost: 0.50,
            },
        );
        // Target 9 shares → sell 1 share at $0.50 = $0.50 proceeds (below $1)
        let targets = vec![TargetAllocation {
            market: make_market("a1"),
            trader_weight: 1.0,
            target_value_usd: 4.5,
            target_shares: 9.0,
            cur_price: 0.50,
        }];
        let orders = compute_orders(&targets, &state, 1000.0, &HashMap::new(), "test");
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].side, OrderSide::Sell);
        assert!(approx_eq(orders[0].shares, 1.0));
    }

    #[test]
    fn orders_budget_exhaustion_partial() {
        let state = TradingState::new(5.0);
        let targets = vec![
            TargetAllocation {
                market: make_market("a1"),
                trader_weight: 0.5,
                target_value_usd: 3.0,
                target_shares: 6.0,
                cur_price: 0.50,
            },
            TargetAllocation {
                market: make_market("a2"),
                trader_weight: 0.5,
                target_value_usd: 4.0,
                target_shares: 8.0,
                cur_price: 0.50,
            },
        ];
        let orders = compute_orders(&targets, &state, 5.0, &HashMap::new(), "test");
        // First buy: $3 (full), second buy: $2 remaining (partial)
        assert_eq!(orders.len(), 2);
        assert!(approx_eq(orders[0].cost_usd, 3.0));
        assert!(approx_eq(orders[1].cost_usd, 2.0));
        assert!(approx_eq(orders[1].shares, 4.0)); // $2 / $0.50
    }

    #[test]
    fn orders_budget_exhaustion_complete() {
        let state = TradingState::new(0.50);
        let targets = vec![TargetAllocation {
            market: make_market("a1"),
            trader_weight: 1.0,
            target_value_usd: 5.0,
            target_shares: 10.0,
            cur_price: 0.50,
        }];
        // $0.50 budget — below $1 minimum, no buys possible
        let orders = compute_orders(&targets, &state, 0.50, &HashMap::new(), "test");
        assert!(orders.is_empty());
    }

    #[test]
    fn orders_resting_prevents_duplicate() {
        let mut state = TradingState::new(1000.0);
        // Resting buy for 5 shares of a1
        state.resting_orders.push(RestingOrder {
            order_id: "order1".to_string(),
            asset: "a1".to_string(),
            title: String::new(),
            outcome: String::new(),
            side: OrderSide::Buy,
            shares: 5.0,
            price: 0.50,
            cost_usd: 2.50,
        });
        // Target 10 shares → effective held = 5 (resting), need 5 more
        let targets = vec![TargetAllocation {
            market: make_market("a1"),
            trader_weight: 1.0,
            target_value_usd: 5.0,
            target_shares: 10.0,
            cur_price: 0.50,
        }];
        let orders = compute_orders(&targets, &state, 1000.0, &HashMap::new(), "test");
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].side, OrderSide::Buy);
        assert!(approx_eq(orders[0].shares, 5.0)); // only 5 more, not 10
    }

    #[test]
    fn orders_resting_sell_covers_exit() {
        let mut state = TradingState::new(1000.0);
        state.holdings.insert(
            "a1".to_string(),
            HeldPosition {
                asset: "a1".to_string(),
                title: "Exited".to_string(),
                outcome: "Yes".to_string(),
                shares: 10.0,
                total_cost: 5.0,
                avg_cost: 0.50,
            },
        );
        // Resting sell covers all held shares
        state.resting_orders.push(RestingOrder {
            order_id: "sell1".to_string(),
            asset: "a1".to_string(),
            title: "Exited".to_string(),
            outcome: "Yes".to_string(),
            side: OrderSide::Sell,
            shares: 10.0,
            price: 0.50,
            cost_usd: 5.0,
        });
        let mut price_map = HashMap::new();
        price_map.insert("a1".to_string(), 0.60);
        // No targets (trader exited) — but resting sell already covers it
        let orders = compute_orders(&[], &state, 1000.0, &price_map, "test");
        assert!(orders.is_empty()); // effective_held_shares = 10 - 10 = 0
    }

    #[test]
    fn orders_missing_exit_price_skips() {
        let mut state = TradingState::new(1000.0);
        state.holdings.insert(
            "a1".to_string(),
            HeldPosition {
                asset: "a1".to_string(),
                title: "Unknown".to_string(),
                outcome: "Yes".to_string(),
                shares: 10.0,
                total_cost: 5.0,
                avg_cost: 0.50,
            },
        );
        // No targets and no price_map entry → should skip (with warning)
        let orders = compute_orders(&[], &state, 1000.0, &HashMap::new(), "test");
        assert!(orders.is_empty());
    }
}
