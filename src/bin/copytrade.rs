use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use polymarket_client_sdk::data::Client;
use polymarket_client_sdk::gamma::Client as GammaClient;
use polymarket_client_sdk::types::Address;
use rust_decimal::prelude::ToPrimitive;
use tracing::{info, warn};

use polymarket_copytrade::api::{
    build_exit_price_map, fetch_active_positions, fetch_recent_trades,
};
use polymarket_copytrade::auth::{self, ClobContext};
use polymarket_copytrade::config::{AppConfig, CONFIG_PATH};
use polymarket_copytrade::engine::{compute_orders, compute_target_state, compute_weights};
use polymarket_copytrade::executor;
use polymarket_copytrade::reporter;
use polymarket_copytrade::state::TradingState;
use polymarket_copytrade::types::{CopytradeEvent, EventTrigger, HeldPosition};

#[derive(Parser)]
#[command(name = "copytrade", about = "Polymarket portfolio copytrade bot")]
struct Args {
    /// Run in simulation mode (no real orders placed)
    #[arg(long, conflicts_with = "live")]
    dry_run: bool,

    /// Run in live mode (places real CLOB orders)
    #[arg(long, conflicts_with = "dry_run")]
    live: bool,

    /// Trader proxy wallet address to copy
    #[arg(long)]
    trader_address: String,

    /// Total budget in USD
    #[arg(long)]
    budget: f64,

    /// Percentage of budget to allocate (0-100)
    #[arg(long)]
    copy_percentage: f64,

    /// Maximum percentage of running budget per position (0-100)
    #[arg(long)]
    max_trade_size: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    // Require exactly one mode
    if !args.dry_run && !args.live {
        anyhow::bail!("Must specify either --dry-run or --live");
    }
    if args.budget <= 0.0 {
        anyhow::bail!("--budget must be positive");
    }
    if !(0.0..=100.0).contains(&args.copy_percentage) {
        anyhow::bail!("--copy-percentage must be between 0 and 100");
    }
    if !(0.0..=100.0).contains(&args.max_trade_size) {
        anyhow::bail!("--max-trade-size must be between 0 and 100");
    }

    // Load config
    let config_path = Path::new(CONFIG_PATH);
    let config = AppConfig::load(config_path)?;
    info!("Loaded config from {}", config_path.display());

    let copy_pct = args.copy_percentage / 100.0;
    let max_trade_pct = args.max_trade_size / 100.0;
    let trader_addr: Address = args
        .trader_address
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid trader address: {e}"))?;
    let trader_short_id = &args.trader_address[args.trader_address.len().saturating_sub(6)..];

    let poll_interval_secs = config.settings.poll_interval_secs;
    let is_live = args.live;

    let mode = if args.dry_run { "dry-run" } else { "live" };
    info!(
        "Starting copytrade ({mode}) — trader={} budget={} copy%={} max_trade%={} poll={}s",
        args.trader_address, args.budget, args.copy_percentage, args.max_trade_size, poll_interval_secs,
    );

    let data_client = Client::default();
    let gamma_client = GammaClient::default();
    let mut state = TradingState::new(args.budget);
    let mut seen_hashes: HashSet<String> = HashSet::new();

    // Authenticate with CLOB if live mode
    let clob_ctx = if is_live {
        info!("Authenticating with CLOB API...");
        let ctx = auth::authenticate(&config.account.private_key).await?;
        info!("Authenticated — EOA: {} Safe: {}", ctx.eoa, ctx.safe);

        // Cancel any stale orders from previous runs
        info!("Cancelling stale orders from previous runs...");
        match ctx.client.cancel_all_orders().await {
            Ok(resp) => {
                if !resp.canceled.is_empty() {
                    info!("Cancelled {} stale order(s)", resp.canceled.len());
                }
            }
            Err(e) => {
                warn!("Failed to cancel stale orders: {e}");
            }
        }

        // Seed holdings from actual Safe wallet positions
        let mut seeded_prices: HashMap<String, f64> = HashMap::new();
        info!("Fetching existing Safe wallet positions...");
        match fetch_active_positions(&data_client, ctx.safe).await {
            Ok(positions) => {
                if !positions.is_empty() {
                    info!(
                        "Found {} existing position(s) in Safe wallet",
                        positions.len()
                    );
                    for pos in &positions {
                        let shares = pos.size.to_f64().unwrap_or(0.0);
                        let avg_cost = pos.avg_price.to_f64().unwrap_or(0.0);
                        let cur_price = pos.cur_price.to_f64().unwrap_or(0.0);
                        let total_cost = shares * avg_cost;
                        let asset = pos.asset.to_string();

                        seeded_prices.insert(asset.clone(), cur_price);
                        state.holdings.insert(
                            asset.clone(),
                            HeldPosition {
                                asset,
                                title: pos.title.clone(),
                                outcome: pos.outcome.clone(),
                                shares,
                                total_cost,
                                avg_cost,
                            },
                        );
                        state.budget_remaining -= total_cost;
                        state.total_spent += total_cost;
                    }
                    info!(
                        "Seeded {} holding(s) (${:.2} committed, ${:.2} remaining)",
                        state.holdings.len(),
                        state.total_spent,
                        state.budget_remaining,
                    );
                }
            }
            Err(e) => {
                warn!("Failed to fetch Safe wallet positions: {e}");
            }
        }

        // Check balance + holdings current value >= budget
        let balance = executor::check_balance(&ctx).await?;
        let holdings_value: f64 = state
            .holdings
            .iter()
            .map(|(asset, h)| {
                // Use seeded_prices (cur_price from data API) if available, fall back to avg_cost
                let price = seeded_prices.get(asset).copied().unwrap_or(h.avg_cost);
                h.shares * price
            })
            .sum();
        let total_capital = balance + holdings_value;
        info!("USDC balance: ${balance:.2}, holdings value: ${holdings_value:.2}, total: ${total_capital:.2}");
        if total_capital < args.budget {
            anyhow::bail!(
                "Insufficient capital: ${total_capital:.2} (${balance:.2} cash + ${holdings_value:.2} holdings) but --budget is ${:.2}",
                args.budget
            );
        }

        Some(ctx)
    } else {
        None
    };

    // --- Initial replication ---
    info!("Fetching trader portfolio...");
    match fetch_active_positions(&data_client, trader_addr).await {
        Ok(positions) => {
            if positions.is_empty() {
                warn!("Trader has no active (unresolved) positions");
            } else {
                info!("Found {} active positions", positions.len());
                let weights = compute_weights(&positions);
                let prices = build_price_map(&positions);
                let running_budget = state.effective_capital(&prices);
                let targets =
                    compute_target_state(&weights, running_budget, copy_pct, max_trade_pct);
                let orders = compute_orders(
                    &targets,
                    &state,
                    state.budget_remaining,
                    &HashMap::new(),
                    trader_short_id,
                );

                let execution_results = if let Some(ctx) = &clob_ctx {
                    let results = executor::execute_orders(ctx, &orders).await;
                    state.apply_execution_results(&orders, &results);
                    Some(results)
                } else {
                    state.apply_orders(&orders);
                    None
                };

                let event = CopytradeEvent {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    trigger: EventTrigger::InitialReplication,
                    detected_trade_hashes: vec![],
                    orders,
                    budget_remaining: state.budget_remaining,
                    total_spent: state.total_spent,
                    execution_results,
                };
                reporter::report_event(&event);
                state.total_events += 1;
            }
        }
        Err(e) => {
            warn!("Failed to fetch positions: {e}");
        }
    }

    // --- Seed dedup set ---
    info!("Seeding dedup set from recent trades...");
    match fetch_recent_trades(&data_client, trader_addr, 50).await {
        Ok(trades) => {
            for trade in &trades {
                seen_hashes.insert(format!("{}", trade.transaction_hash));
            }
            info!("Seeded {} trade hashes", seen_hashes.len());
        }
        Err(e) => {
            warn!("Failed to seed trades: {e}");
        }
    }

    // --- Polling loop ---
    // Check if any initial orders are resting (give them a moment to fill)
    if !state.resting_orders.is_empty() {
        info!(
            "Tracking {} resting order(s) from initial replication",
            state.resting_orders.len()
        );
    }

    info!("Entering polling loop (interval: {poll_interval_secs}s). Press Ctrl+C to stop.");
    let poll_duration = Duration::from_secs(poll_interval_secs);

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Shutdown signal received");
                break;
            }
            _ = tokio::time::sleep(poll_duration) => {
                if let Err(e) = poll_cycle(
                    &data_client,
                    &gamma_client,
                    clob_ctx.as_ref(),
                    trader_addr,
                    trader_short_id,
                    &mut state,
                    &mut seen_hashes,
                    copy_pct,
                    max_trade_pct,
                ).await {
                    warn!("Poll cycle error: {e}");
                }
            }
        }
    }

    // --- Cancel resting orders on shutdown (live mode) ---
    if let Some(ctx) = &clob_ctx {
        if !state.resting_orders.is_empty() {
            info!(
                "Cancelling {} resting order(s) on shutdown...",
                state.resting_orders.len()
            );
            let order_ids: Vec<String> = state
                .resting_orders
                .iter()
                .map(|r| r.order_id.clone())
                .collect();
            let id_refs: Vec<&str> = order_ids.iter().map(|s| s.as_str()).collect();
            match ctx.client.cancel_orders(&id_refs).await {
                Ok(resp) => {
                    if !resp.canceled.is_empty() {
                        info!("Cancelled {} order(s)", resp.canceled.len());
                    }
                    for (id, err) in &resp.not_canceled {
                        warn!("Failed to cancel order {id}: {err}");
                    }
                }
                Err(e) => {
                    warn!("Failed to cancel resting orders: {e}");
                }
            }
            // Resolve all resting orders as cancelled in state
            for order_id in &order_ids {
                state.resolve_resting_cancel(order_id);
            }
        }
    }

    // --- Exit summary ---
    info!("Computing exit summary...");
    let active_prices = match fetch_active_positions(&data_client, trader_addr).await {
        Ok(positions) => build_price_map(&positions),
        Err(e) => {
            warn!("Failed to fetch final positions for exit summary: {e}");
            HashMap::new()
        }
    };
    let held_assets: Vec<String> = state.holdings.keys().cloned().collect();
    let latest_prices =
        build_exit_price_map(&gamma_client, &active_prices, &held_assets).await?;
    let summary = state.exit_summary(&latest_prices);
    reporter::report_exit_summary(&summary);

    Ok(())
}

/// One polling cycle: fetch recent trades, detect new ones, rebalance if needed.
async fn poll_cycle(
    client: &Client,
    gamma: &GammaClient,
    clob_ctx: Option<&ClobContext>,
    addr: Address,
    trader_short_id: &str,
    state: &mut TradingState,
    seen_hashes: &mut HashSet<String>,
    copy_pct: f64,
    max_trade_pct: f64,
) -> Result<()> {
    // Check resting orders before computing new ones
    if let Some(ctx) = clob_ctx {
        executor::check_resting_orders(ctx, state).await;
    }

    info!("Polling... (seen: {} hashes)", seen_hashes.len());
    let trades = fetch_recent_trades(client, addr, 50).await?;

    let mut new_hashes = Vec::new();
    for trade in &trades {
        let hash = format!("{}", trade.transaction_hash);
        if seen_hashes.insert(hash.clone()) {
            new_hashes.push(hash);
        }
    }

    if new_hashes.is_empty() {
        info!("No new trades");
        return Ok(());
    }

    info!("Detected {} new trade(s), rebalancing...", new_hashes.len());

    let positions = fetch_active_positions(client, addr).await?;
    let active_prices = build_price_map(&positions);

    let weights = compute_weights(&positions);
    let running_budget = state.effective_capital(&active_prices);
    let targets = compute_target_state(&weights, running_budget, copy_pct, max_trade_pct);

    // Build price map with gamma fallback for held assets the trader exited
    let held_assets: Vec<String> = state.holdings.keys().cloned().collect();
    let price_map = build_exit_price_map(gamma, &active_prices, &held_assets).await?;

    let orders = compute_orders(&targets, state, state.budget_remaining, &price_map, trader_short_id);

    if !orders.is_empty() {
        let execution_results = if let Some(ctx) = clob_ctx {
            let results = executor::execute_orders(ctx, &orders).await;
            state.apply_execution_results(&orders, &results);
            Some(results)
        } else {
            state.apply_orders(&orders);
            None
        };

        let event = CopytradeEvent {
            timestamp: chrono::Utc::now().to_rfc3339(),
            trigger: EventTrigger::TradeDetected,
            detected_trade_hashes: new_hashes,
            orders,
            budget_remaining: state.budget_remaining,
            total_spent: state.total_spent,
            execution_results,
        };
        reporter::report_event(&event);
        state.total_events += 1;
    } else {
        info!("No rebalancing orders needed");
    }

    Ok(())
}

/// Build a map of asset → current price from positions.
fn build_price_map(
    positions: &[polymarket_client_sdk::data::types::response::Position],
) -> HashMap<String, f64> {
    positions
        .iter()
        .map(|p| {
            (
                p.asset.to_string(),
                p.cur_price.to_f64().unwrap_or(0.0),
            )
        })
        .collect()
}
