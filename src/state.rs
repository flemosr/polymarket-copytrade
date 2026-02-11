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
