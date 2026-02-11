use std::collections::HashMap;

use crate::types::{
    ExecutionResult, ExecutionStatus, ExitSummary, HeldPosition, HoldingSummary, OrderSide,
    RestingOrder, SimulatedOrder,
};

/// Tracks the bot's simulated trading state: holdings, budget, and P&L.
pub struct TradingState {
    /// Current holdings keyed by asset token ID.
    pub holdings: HashMap<String, HeldPosition>,
    /// Orders resting on the CLOB book (not yet filled).
    pub resting_orders: Vec<RestingOrder>,
    pub initial_budget: f64,
    pub budget_remaining: f64,
    pub total_spent: f64,
    pub total_sell_proceeds: f64,
    pub realized_pnl: f64,
    pub total_events: u64,
    pub total_orders: u64,
    pub total_buy_orders: u64,
    pub total_sell_orders: u64,
}

impl TradingState {
    pub fn new(budget: f64) -> Self {
        Self {
            holdings: HashMap::new(),
            resting_orders: Vec::new(),
            initial_budget: budget,
            budget_remaining: budget,
            total_spent: 0.0,
            total_sell_proceeds: 0.0,
            realized_pnl: 0.0,
            total_events: 0,
            total_orders: 0,
            total_buy_orders: 0,
            total_sell_orders: 0,
        }
    }

    /// Running budget: cash + current market value of all holdings + resting order value.
    pub fn effective_capital(&self, prices: &HashMap<String, f64>) -> f64 {
        let holdings_value: f64 = self
            .holdings
            .iter()
            .map(|(asset, held)| {
                let price = prices.get(asset).copied().unwrap_or(held.avg_cost);
                held.shares * price
            })
            .sum();
        // Include value of resting buy orders (budget was already deducted for these)
        let resting_buy_value: f64 = self
            .resting_orders
            .iter()
            .filter(|r| r.side == OrderSide::Buy)
            .map(|r| {
                let price = prices.get(&r.asset).copied().unwrap_or(r.price);
                r.shares * price
            })
            .sum();
        self.budget_remaining + holdings_value + resting_buy_value
    }

    /// Effective held shares for an asset, including resting order adjustments.
    ///
    /// Returns `holdings.shares + resting_buy_shares - resting_sell_shares` so the
    /// rebalancing engine doesn't generate duplicate orders for resting positions.
    pub fn effective_held_shares(&self, asset: &str) -> f64 {
        let held = self
            .holdings
            .get(asset)
            .map(|h| h.shares)
            .unwrap_or(0.0);
        let resting_buy: f64 = self
            .resting_orders
            .iter()
            .filter(|r| r.asset == asset && r.side == OrderSide::Buy)
            .map(|r| r.shares)
            .sum();
        let resting_sell: f64 = self
            .resting_orders
            .iter()
            .filter(|r| r.asset == asset && r.side == OrderSide::Sell)
            .map(|r| r.shares)
            .sum();
        held + resting_buy - resting_sell
    }

    /// Track a resting order and reserve budget for buys.
    pub fn add_resting_order(&mut self, order: RestingOrder) {
        if order.side == OrderSide::Buy {
            self.budget_remaining -= order.cost_usd;
        }
        self.resting_orders.push(order);
    }

    /// Handle a resting order that has been filled.
    ///
    /// Moves the fill into actual holdings. For buys, budget was already reserved
    /// when the order was placed. For sells, proceeds are now credited.
    pub fn resolve_resting_fill(
        &mut self,
        order_id: &str,
        filled_shares: f64,
        fill_price: f64,
    ) {
        let idx = match self.resting_orders.iter().position(|r| r.order_id == order_id) {
            Some(i) => i,
            None => return,
        };
        let resting = self.resting_orders.remove(idx);
        let filled_cost = filled_shares * fill_price;

        match resting.side {
            OrderSide::Buy => {
                // Budget was already deducted when order was placed.
                // Adjust for any difference between reserved and actual cost.
                let reserved = resting.cost_usd;
                let diff = reserved - filled_cost;
                self.budget_remaining += diff; // return over-reservation (or deduct under)
                self.total_spent += filled_cost;
                self.total_buy_orders += 1;

                let asset_key = resting.asset.clone();
                let held = self
                    .holdings
                    .entry(resting.asset)
                    .or_insert_with(|| HeldPosition {
                        asset: asset_key,
                        title: resting.title.clone(),
                        outcome: resting.outcome.clone(),
                        shares: 0.0,
                        total_cost: 0.0,
                        avg_cost: 0.0,
                    });
                held.shares += filled_shares;
                held.total_cost += filled_cost;
                held.avg_cost = if held.shares > 0.0 {
                    held.total_cost / held.shares
                } else {
                    0.0
                };
            }
            OrderSide::Sell => {
                self.budget_remaining += filled_cost;
                self.total_sell_proceeds += filled_cost;
                self.total_sell_orders += 1;

                if let Some(held) = self.holdings.get_mut(&resting.asset) {
                    let pnl = (fill_price - held.avg_cost) * filled_shares;
                    self.realized_pnl += pnl;
                    held.shares -= filled_shares;
                    held.total_cost -= held.avg_cost * filled_shares;
                    if held.shares <= 0.0 {
                        self.holdings.remove(&resting.asset);
                    }
                }
            }
        }
        self.total_orders += 1;
    }

    /// Handle a resting order that was cancelled without filling.
    ///
    /// Returns reserved budget for buy orders.
    pub fn resolve_resting_cancel(&mut self, order_id: &str) {
        let idx = match self.resting_orders.iter().position(|r| r.order_id == order_id) {
            Some(i) => i,
            None => return,
        };
        let resting = self.resting_orders.remove(idx);
        if resting.side == OrderSide::Buy {
            self.budget_remaining += resting.cost_usd;
        }
    }

    /// Apply a set of simulated orders to the trading state.
    pub fn apply_orders(&mut self, orders: &[SimulatedOrder]) {
        for order in orders {
            match order.side {
                OrderSide::Buy => {
                    self.budget_remaining -= order.cost_usd;
                    self.total_spent += order.cost_usd;
                    self.total_buy_orders += 1;

                    let held = self
                        .holdings
                        .entry(order.market.asset.clone())
                        .or_insert_with(|| HeldPosition {
                            asset: order.market.asset.clone(),
                            title: order.market.title.clone(),
                            outcome: order.market.outcome.clone(),
                            shares: 0.0,
                            total_cost: 0.0,
                            avg_cost: 0.0,
                        });
                    held.shares += order.shares;
                    held.total_cost += order.cost_usd;
                    held.avg_cost = if held.shares > 0.0 {
                        held.total_cost / held.shares
                    } else {
                        0.0
                    };
                }
                OrderSide::Sell => {
                    self.budget_remaining += order.cost_usd;
                    self.total_sell_proceeds += order.cost_usd;
                    self.total_sell_orders += 1;

                    if let Some(held) = self.holdings.get_mut(&order.market.asset) {
                        // Realized P&L = (sell_price - avg_cost) * shares
                        let pnl = (order.price - held.avg_cost) * order.shares;
                        self.realized_pnl += pnl;

                        held.shares -= order.shares;
                        held.total_cost -= held.avg_cost * order.shares;
                        if held.shares <= 0.0 {
                            self.holdings.remove(&order.market.asset);
                        }
                    }
                }
            }
            self.total_orders += 1;
        }
    }

    /// Apply live execution results to the trading state.
    ///
    /// - `Filled` / `PartialFill` → apply to holdings immediately.
    /// - `Resting` → track as resting order (budget reserved for buys).
    /// - `Failed` / `Skipped` → no state change.
    pub fn apply_execution_results(
        &mut self,
        orders: &[SimulatedOrder],
        results: &[ExecutionResult],
    ) {
        let filled_orders: Vec<SimulatedOrder> = results
            .iter()
            .filter(|r| {
                r.status == ExecutionStatus::Filled || r.status == ExecutionStatus::PartialFill
            })
            .filter_map(|r| {
                let original = orders.get(r.order_index)?;
                Some(SimulatedOrder {
                    market: original.market.clone(),
                    side: original.side,
                    shares: r.filled_shares,
                    price: if r.filled_shares > 0.0 {
                        r.filled_cost_usd / r.filled_shares
                    } else {
                        original.price
                    },
                    cost_usd: r.filled_cost_usd,
                })
            })
            .collect();

        self.apply_orders(&filled_orders);

        // Track resting orders (budget reserved for buys, sells tracked for dedup)
        for result in results {
            if let Some(original) = orders.get(result.order_index) {
                match result.status {
                    ExecutionStatus::Resting => {
                        self.add_resting_order(RestingOrder {
                            order_id: result.order_id.clone(),
                            asset: original.market.asset.clone(),
                            title: original.market.title.clone(),
                            outcome: original.market.outcome.clone(),
                            side: original.side,
                            shares: original.shares,
                            price: original.price,
                            cost_usd: original.cost_usd,
                        });
                    }
                    ExecutionStatus::PartialFill => {
                        // Track the unfilled remainder as a resting order
                        let remaining_shares = original.shares - result.filled_shares;
                        if remaining_shares > 0.0 && !result.order_id.is_empty() {
                            let remaining_cost = remaining_shares * original.price;
                            self.add_resting_order(RestingOrder {
                                order_id: result.order_id.clone(),
                                asset: original.market.asset.clone(),
                                title: original.market.title.clone(),
                                outcome: original.market.outcome.clone(),
                                side: original.side,
                                shares: remaining_shares,
                                price: original.price,
                                cost_usd: remaining_cost,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Compute the exit summary with unrealized P&L based on latest prices.
    ///
    /// `latest_prices` maps asset token ID → current price.
    pub fn exit_summary(&self, latest_prices: &HashMap<String, f64>) -> ExitSummary {
        let mut holdings_summary = Vec::new();
        let mut unrealized_pnl = 0.0;

        for (asset, held) in &self.holdings {
            let cur_price = latest_prices.get(asset).copied().unwrap_or(0.0);
            let current_value = held.shares * cur_price;
            let position_unrealized = (cur_price - held.avg_cost) * held.shares;
            unrealized_pnl += position_unrealized;

            holdings_summary.push(HoldingSummary {
                asset: held.asset.clone(),
                title: held.title.clone(),
                outcome: held.outcome.clone(),
                shares: held.shares,
                avg_cost: held.avg_cost,
                cur_price,
                current_value,
                unrealized_pnl: position_unrealized,
            });
        }

        let total_pnl = self.realized_pnl + unrealized_pnl;
        let pnl_percent = if self.initial_budget > 0.0 {
            (total_pnl / self.initial_budget) * 100.0
        } else {
            0.0
        };

        ExitSummary {
            initial_budget: self.initial_budget,
            budget_remaining: self.budget_remaining,
            total_spent: self.total_spent,
            total_sell_proceeds: self.total_sell_proceeds,
            realized_pnl: self.realized_pnl,
            unrealized_pnl,
            total_pnl,
            pnl_percent,
            total_events: self.total_events,
            total_orders: self.total_orders,
            total_buy_orders: self.total_buy_orders,
            total_sell_orders: self.total_sell_orders,
            holdings: holdings_summary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MarketPosition;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
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

    fn make_order(asset: &str, side: OrderSide, shares: f64, price: f64) -> SimulatedOrder {
        SimulatedOrder {
            market: make_market(asset),
            side,
            shares,
            price,
            cost_usd: shares * price,
        }
    }

    fn make_resting(
        order_id: &str,
        asset: &str,
        side: OrderSide,
        shares: f64,
        price: f64,
    ) -> RestingOrder {
        RestingOrder {
            order_id: order_id.to_string(),
            asset: asset.to_string(),
            title: String::new(),
            outcome: String::new(),
            side,
            shares,
            price,
            cost_usd: shares * price,
        }
    }

    // ── Constructor ────────────────────────────────────────────────

    #[test]
    fn new_initializes_correctly() {
        let s = TradingState::new(500.0);
        assert!(approx_eq(s.initial_budget, 500.0));
        assert!(approx_eq(s.budget_remaining, 500.0));
        assert!(approx_eq(s.total_spent, 0.0));
        assert!(approx_eq(s.total_sell_proceeds, 0.0));
        assert!(approx_eq(s.realized_pnl, 0.0));
        assert_eq!(s.total_events, 0);
        assert_eq!(s.total_orders, 0);
        assert!(s.holdings.is_empty());
        assert!(s.resting_orders.is_empty());
    }

    // ── effective_capital ──────────────────────────────────────────

    #[test]
    fn effective_capital_empty() {
        let s = TradingState::new(500.0);
        let prices = HashMap::new();
        assert!(approx_eq(s.effective_capital(&prices), 500.0));
    }

    #[test]
    fn effective_capital_with_holdings() {
        let mut s = TradingState::new(300.0);
        s.holdings.insert(
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
        let mut prices = HashMap::new();
        prices.insert("a1".to_string(), 0.60);
        // 300 + 10*0.60 = 306
        assert!(approx_eq(s.effective_capital(&prices), 306.0));
    }

    #[test]
    fn effective_capital_with_resting_buys() {
        let mut s = TradingState::new(300.0);
        s.resting_orders
            .push(make_resting("o1", "a1", OrderSide::Buy, 10.0, 0.50));
        let mut prices = HashMap::new();
        prices.insert("a1".to_string(), 0.60);
        // 300 + 10*0.60 (resting buy value at market price) = 306
        assert!(approx_eq(s.effective_capital(&prices), 306.0));
    }

    #[test]
    fn effective_capital_missing_price_falls_back_to_avg_cost() {
        let mut s = TradingState::new(300.0);
        s.holdings.insert(
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
        // No price in map → falls back to avg_cost (0.50)
        let prices = HashMap::new();
        // 300 + 10*0.50 = 305
        assert!(approx_eq(s.effective_capital(&prices), 305.0));
    }

    // ── effective_held_shares ──────────────────────────────────────

    #[test]
    fn effective_held_shares_no_holdings() {
        let s = TradingState::new(500.0);
        assert!(approx_eq(s.effective_held_shares("a1"), 0.0));
    }

    #[test]
    fn effective_held_shares_holdings_only() {
        let mut s = TradingState::new(500.0);
        s.holdings.insert(
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
        assert!(approx_eq(s.effective_held_shares("a1"), 10.0));
    }

    #[test]
    fn effective_held_shares_with_resting_buy() {
        let mut s = TradingState::new(500.0);
        s.holdings.insert(
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
        s.resting_orders
            .push(make_resting("o1", "a1", OrderSide::Buy, 5.0, 0.50));
        assert!(approx_eq(s.effective_held_shares("a1"), 15.0));
    }

    #[test]
    fn effective_held_shares_with_resting_sell() {
        let mut s = TradingState::new(500.0);
        s.holdings.insert(
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
        s.resting_orders
            .push(make_resting("o1", "a1", OrderSide::Sell, 3.0, 0.50));
        assert!(approx_eq(s.effective_held_shares("a1"), 7.0));
    }

    #[test]
    fn effective_held_shares_combined_buy_and_sell() {
        let mut s = TradingState::new(500.0);
        s.holdings.insert(
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
        s.resting_orders
            .push(make_resting("o1", "a1", OrderSide::Buy, 5.0, 0.50));
        s.resting_orders
            .push(make_resting("o2", "a1", OrderSide::Sell, 3.0, 0.50));
        // 10 + 5 - 3 = 12
        assert!(approx_eq(s.effective_held_shares("a1"), 12.0));
    }

    // ── Resting Order Lifecycle ────────────────────────────────────

    #[test]
    fn resting_add_buy_reserves_budget() {
        let mut s = TradingState::new(100.0);
        s.add_resting_order(make_resting("o1", "a1", OrderSide::Buy, 10.0, 0.50));
        assert!(approx_eq(s.budget_remaining, 95.0)); // 100 - 5
        assert_eq!(s.resting_orders.len(), 1);
    }

    #[test]
    fn resting_add_sell_no_budget_change() {
        let mut s = TradingState::new(100.0);
        s.add_resting_order(make_resting("o1", "a1", OrderSide::Sell, 10.0, 0.50));
        assert!(approx_eq(s.budget_remaining, 100.0));
        assert_eq!(s.resting_orders.len(), 1);
    }

    #[test]
    fn resting_fill_buy() {
        let mut s = TradingState::new(100.0);
        s.add_resting_order(make_resting("o1", "a1", OrderSide::Buy, 10.0, 0.50));
        assert!(approx_eq(s.budget_remaining, 95.0));

        s.resolve_resting_fill("o1", 10.0, 0.50);
        assert!(s.resting_orders.is_empty());
        assert!(approx_eq(s.total_spent, 5.0));
        assert_eq!(s.total_buy_orders, 1);
        let held = s.holdings.get("a1").unwrap();
        assert!(approx_eq(held.shares, 10.0));
        assert!(approx_eq(held.avg_cost, 0.50));
    }

    #[test]
    fn resting_fill_buy_price_diff() {
        let mut s = TradingState::new(100.0);
        // Reserved at $0.50 per share (cost_usd = 5.0)
        s.add_resting_order(make_resting("o1", "a1", OrderSide::Buy, 10.0, 0.50));
        assert!(approx_eq(s.budget_remaining, 95.0));

        // Actually filled at $0.40 per share (cost = 4.0)
        s.resolve_resting_fill("o1", 10.0, 0.40);
        // Over-reservation of $1.0 returned
        assert!(approx_eq(s.budget_remaining, 96.0)); // 95 + (5.0 - 4.0)
        assert!(approx_eq(s.total_spent, 4.0));
    }

    #[test]
    fn resting_fill_sell() {
        let mut s = TradingState::new(100.0);
        s.holdings.insert(
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
        s.add_resting_order(make_resting("o1", "a1", OrderSide::Sell, 10.0, 0.60));

        s.resolve_resting_fill("o1", 10.0, 0.60);
        assert!(approx_eq(s.budget_remaining, 106.0)); // 100 + 6.0 proceeds
        assert!(approx_eq(s.total_sell_proceeds, 6.0));
        assert!(approx_eq(s.realized_pnl, 1.0)); // (0.60 - 0.50) * 10
        assert!(s.holdings.is_empty()); // fully sold
    }

    #[test]
    fn resting_cancel_buy_refunds_budget() {
        let mut s = TradingState::new(100.0);
        s.add_resting_order(make_resting("o1", "a1", OrderSide::Buy, 10.0, 0.50));
        assert!(approx_eq(s.budget_remaining, 95.0));

        s.resolve_resting_cancel("o1");
        assert!(approx_eq(s.budget_remaining, 100.0)); // refunded
        assert!(s.resting_orders.is_empty());
    }

    #[test]
    fn resting_cancel_sell_no_budget_change() {
        let mut s = TradingState::new(100.0);
        s.add_resting_order(make_resting("o1", "a1", OrderSide::Sell, 10.0, 0.50));

        s.resolve_resting_cancel("o1");
        assert!(approx_eq(s.budget_remaining, 100.0));
        assert!(s.resting_orders.is_empty());
    }

    #[test]
    fn resting_unknown_order_id_noop() {
        let mut s = TradingState::new(100.0);
        s.resolve_resting_fill("nonexistent", 10.0, 0.50);
        s.resolve_resting_cancel("nonexistent");
        assert!(approx_eq(s.budget_remaining, 100.0));
        assert!(s.holdings.is_empty());
    }

    // ── apply_orders ───────────────────────────────────────────────

    #[test]
    fn apply_orders_buy() {
        let mut s = TradingState::new(100.0);
        let orders = vec![make_order("a1", OrderSide::Buy, 10.0, 0.50)];
        s.apply_orders(&orders);

        assert!(approx_eq(s.budget_remaining, 95.0));
        assert!(approx_eq(s.total_spent, 5.0));
        assert_eq!(s.total_buy_orders, 1);
        assert_eq!(s.total_orders, 1);
        let held = s.holdings.get("a1").unwrap();
        assert!(approx_eq(held.shares, 10.0));
        assert!(approx_eq(held.avg_cost, 0.50));
    }

    #[test]
    fn apply_orders_sell() {
        let mut s = TradingState::new(100.0);
        // First buy to establish position
        s.holdings.insert(
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
        let orders = vec![make_order("a1", OrderSide::Sell, 10.0, 0.60)];
        s.apply_orders(&orders);

        assert!(approx_eq(s.budget_remaining, 106.0)); // 100 + 6.0
        assert!(approx_eq(s.total_sell_proceeds, 6.0));
        assert!(approx_eq(s.realized_pnl, 1.0)); // (0.60 - 0.50) * 10
        assert_eq!(s.total_sell_orders, 1);
        assert!(s.holdings.is_empty()); // fully sold → removed
    }

    #[test]
    fn apply_orders_full_sell_removes_position() {
        let mut s = TradingState::new(100.0);
        s.holdings.insert(
            "a1".to_string(),
            HeldPosition {
                asset: "a1".to_string(),
                title: String::new(),
                outcome: String::new(),
                shares: 5.0,
                total_cost: 2.5,
                avg_cost: 0.50,
            },
        );
        s.apply_orders(&[make_order("a1", OrderSide::Sell, 5.0, 0.50)]);
        assert!(s.holdings.get("a1").is_none());
    }

    #[test]
    fn apply_orders_sell_funds_buy() {
        let mut s = TradingState::new(0.0); // no cash
        s.holdings.insert(
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
        let orders = vec![
            make_order("a1", OrderSide::Sell, 10.0, 0.50),
            make_order("a2", OrderSide::Buy, 10.0, 0.50),
        ];
        s.apply_orders(&orders);

        assert!(approx_eq(s.budget_remaining, 0.0)); // sell proceeds funded buy
        assert!(s.holdings.get("a1").is_none());
        let held = s.holdings.get("a2").unwrap();
        assert!(approx_eq(held.shares, 10.0));
    }

    #[test]
    fn apply_orders_buy_updates_avg_cost() {
        let mut s = TradingState::new(1000.0);
        // Buy 10 at 0.40
        s.apply_orders(&[make_order("a1", OrderSide::Buy, 10.0, 0.40)]);
        // Buy 10 more at 0.60
        s.apply_orders(&[make_order("a1", OrderSide::Buy, 10.0, 0.60)]);

        let held = s.holdings.get("a1").unwrap();
        assert!(approx_eq(held.shares, 20.0));
        // avg_cost = (10*0.40 + 10*0.60) / 20 = 10 / 20 = 0.50
        assert!(approx_eq(held.avg_cost, 0.50));
    }

    // ── apply_execution_results ────────────────────────────────────

    #[test]
    fn execution_filled() {
        let mut s = TradingState::new(100.0);
        let orders = vec![make_order("a1", OrderSide::Buy, 10.0, 0.50)];
        let results = vec![ExecutionResult {
            order_index: 0,
            status: ExecutionStatus::Filled,
            order_id: "oid1".to_string(),
            filled_shares: 10.0,
            filled_cost_usd: 5.0,
            error_msg: None,
        }];
        s.apply_execution_results(&orders, &results);

        assert!(approx_eq(s.budget_remaining, 95.0));
        assert!(approx_eq(s.total_spent, 5.0));
        let held = s.holdings.get("a1").unwrap();
        assert!(approx_eq(held.shares, 10.0));
        assert!(s.resting_orders.is_empty());
    }

    #[test]
    fn execution_partial_fill() {
        let mut s = TradingState::new(100.0);
        let orders = vec![make_order("a1", OrderSide::Buy, 10.0, 0.50)];
        let results = vec![ExecutionResult {
            order_index: 0,
            status: ExecutionStatus::PartialFill,
            order_id: "oid1".to_string(),
            filled_shares: 6.0,
            filled_cost_usd: 3.0,
            error_msg: None,
        }];
        s.apply_execution_results(&orders, &results);

        // 6 shares filled immediately
        let held = s.holdings.get("a1").unwrap();
        assert!(approx_eq(held.shares, 6.0));
        assert!(approx_eq(s.total_spent, 3.0));
        // Remaining 4 shares tracked as resting
        assert_eq!(s.resting_orders.len(), 1);
        assert!(approx_eq(s.resting_orders[0].shares, 4.0));
        assert_eq!(s.resting_orders[0].order_id, "oid1");
        // Budget: 100 - 3.0 (filled) - 2.0 (resting 4*0.50) = 95.0
        assert!(approx_eq(s.budget_remaining, 95.0));
    }

    #[test]
    fn execution_resting() {
        let mut s = TradingState::new(100.0);
        let orders = vec![make_order("a1", OrderSide::Buy, 10.0, 0.50)];
        let results = vec![ExecutionResult {
            order_index: 0,
            status: ExecutionStatus::Resting,
            order_id: "oid1".to_string(),
            filled_shares: 0.0,
            filled_cost_usd: 0.0,
            error_msg: None,
        }];
        s.apply_execution_results(&orders, &results);

        assert!(s.holdings.is_empty()); // nothing filled
        assert_eq!(s.resting_orders.len(), 1);
        assert!(approx_eq(s.resting_orders[0].shares, 10.0));
        // Budget reserved for resting buy
        assert!(approx_eq(s.budget_remaining, 95.0));
    }

    #[test]
    fn execution_failed() {
        let mut s = TradingState::new(100.0);
        let orders = vec![make_order("a1", OrderSide::Buy, 10.0, 0.50)];
        let results = vec![ExecutionResult {
            order_index: 0,
            status: ExecutionStatus::Failed,
            order_id: String::new(),
            filled_shares: 0.0,
            filled_cost_usd: 0.0,
            error_msg: Some("insufficient balance".to_string()),
        }];
        s.apply_execution_results(&orders, &results);

        assert!(approx_eq(s.budget_remaining, 100.0)); // no change
        assert!(s.holdings.is_empty());
        assert!(s.resting_orders.is_empty());
    }

    #[test]
    fn execution_skipped() {
        let mut s = TradingState::new(100.0);
        let orders = vec![make_order("a1", OrderSide::Buy, 10.0, 0.50)];
        let results = vec![ExecutionResult {
            order_index: 0,
            status: ExecutionStatus::Skipped,
            order_id: String::new(),
            filled_shares: 0.0,
            filled_cost_usd: 0.0,
            error_msg: None,
        }];
        s.apply_execution_results(&orders, &results);

        assert!(approx_eq(s.budget_remaining, 100.0));
        assert!(s.holdings.is_empty());
        assert!(s.resting_orders.is_empty());
    }

    #[test]
    fn execution_mixed_statuses() {
        let mut s = TradingState::new(100.0);
        let orders = vec![
            make_order("a1", OrderSide::Buy, 10.0, 0.50),
            make_order("a2", OrderSide::Buy, 8.0, 0.40),
            make_order("a3", OrderSide::Buy, 5.0, 0.60),
        ];
        let results = vec![
            ExecutionResult {
                order_index: 0,
                status: ExecutionStatus::Filled,
                order_id: "o1".to_string(),
                filled_shares: 10.0,
                filled_cost_usd: 5.0,
                error_msg: None,
            },
            ExecutionResult {
                order_index: 1,
                status: ExecutionStatus::Resting,
                order_id: "o2".to_string(),
                filled_shares: 0.0,
                filled_cost_usd: 0.0,
                error_msg: None,
            },
            ExecutionResult {
                order_index: 2,
                status: ExecutionStatus::Failed,
                order_id: String::new(),
                filled_shares: 0.0,
                filled_cost_usd: 0.0,
                error_msg: Some("error".to_string()),
            },
        ];
        s.apply_execution_results(&orders, &results);

        // a1: filled → in holdings
        assert!(approx_eq(s.holdings.get("a1").unwrap().shares, 10.0));
        // a2: resting → tracked, budget reserved
        assert_eq!(s.resting_orders.len(), 1);
        assert_eq!(s.resting_orders[0].asset, "a2");
        // a3: failed → no effect
        assert!(s.holdings.get("a3").is_none());
        // Budget: 100 - 5.0 (a1 filled) - 3.2 (a2 resting: 8*0.40) = 91.8
        assert!(approx_eq(s.budget_remaining, 91.8));
    }

    // ── exit_summary ───────────────────────────────────────────────

    #[test]
    fn exit_summary_basic() {
        let mut s = TradingState::new(100.0);
        s.budget_remaining = 90.0;
        s.total_spent = 10.0;
        s.holdings.insert(
            "a1".to_string(),
            HeldPosition {
                asset: "a1".to_string(),
                title: "Test".to_string(),
                outcome: "Yes".to_string(),
                shares: 20.0,
                total_cost: 10.0,
                avg_cost: 0.50,
            },
        );
        let mut prices = HashMap::new();
        prices.insert("a1".to_string(), 0.60);

        let summary = s.exit_summary(&prices);
        // unrealized = (0.60 - 0.50) * 20 = 2.0
        assert!(approx_eq(summary.unrealized_pnl, 2.0));
        assert!(approx_eq(summary.total_pnl, 2.0));
        assert!(approx_eq(summary.pnl_percent, 2.0)); // 2/100 * 100
        assert_eq!(summary.holdings.len(), 1);
        assert!(approx_eq(summary.holdings[0].current_value, 12.0));
    }

    #[test]
    fn exit_summary_with_realized_pnl() {
        let mut s = TradingState::new(100.0);
        s.realized_pnl = 5.0;
        s.budget_remaining = 95.0;
        s.holdings.insert(
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
        let mut prices = HashMap::new();
        prices.insert("a1".to_string(), 0.70);

        let summary = s.exit_summary(&prices);
        // unrealized = (0.70 - 0.50) * 10 = 2.0
        assert!(approx_eq(summary.unrealized_pnl, 2.0));
        assert!(approx_eq(summary.realized_pnl, 5.0));
        assert!(approx_eq(summary.total_pnl, 7.0)); // 5 + 2
    }

    #[test]
    fn exit_summary_missing_price_falls_back_to_zero() {
        let mut s = TradingState::new(100.0);
        s.holdings.insert(
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
        let prices = HashMap::new(); // no price

        let summary = s.exit_summary(&prices);
        // Falls back to price 0 → unrealized = (0 - 0.50) * 10 = -5.0
        assert!(approx_eq(summary.unrealized_pnl, -5.0));
        assert!(approx_eq(summary.holdings[0].cur_price, 0.0));
    }

    #[test]
    fn exit_summary_empty_holdings() {
        let mut s = TradingState::new(100.0);
        s.realized_pnl = 3.0;

        let summary = s.exit_summary(&HashMap::new());
        assert!(summary.holdings.is_empty());
        assert!(approx_eq(summary.unrealized_pnl, 0.0));
        assert!(approx_eq(summary.total_pnl, 3.0)); // realized only
    }
}
