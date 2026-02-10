use std::collections::HashMap;

use crate::types::{
    ExitSummary, HeldPosition, HoldingSummary, OrderSide, SimulatedOrder,
};

/// Tracks the bot's simulated trading state: holdings, budget, and P&L.
pub struct TradingState {
    /// Current holdings keyed by asset token ID.
    pub holdings: HashMap<String, HeldPosition>,
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

    /// Compute the exit summary with unrealized P&L based on latest prices.
    ///
    /// `latest_prices` maps asset token ID → current price.
    pub fn exit_summary(&self, latest_prices: &HashMap<String, f64>) -> ExitSummary {
        let mut holdings_summary = Vec::new();
        let mut unrealized_pnl = 0.0;

        for (asset, held) in &self.holdings {
            let cur_price = latest_prices.get(asset).copied().unwrap_or(held.avg_cost);
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

        ExitSummary {
            initial_budget: self.initial_budget,
            budget_remaining: self.budget_remaining,
            total_spent: self.total_spent,
            total_sell_proceeds: self.total_sell_proceeds,
            realized_pnl: self.realized_pnl,
            unrealized_pnl,
            total_pnl,
            total_events: self.total_events,
            total_orders: self.total_orders,
            total_buy_orders: self.total_buy_orders,
            total_sell_orders: self.total_sell_orders,
            holdings: holdings_summary,
        }
    }
}
