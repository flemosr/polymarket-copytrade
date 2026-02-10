use std::collections::{HashMap, HashSet};
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
use polymarket_copytrade::engine::{compute_orders, compute_target_state, compute_weights};
use polymarket_copytrade::reporter;
use polymarket_copytrade::state::TradingState;
use polymarket_copytrade::types::{CopytradeEvent, EventTrigger};

#[derive(Parser)]
#[command(name = "copytrade", about = "Polymarket portfolio copytrade bot")]
struct Args {
    /// Run in simulation mode (no real orders placed)
    #[arg(long)]
    dry_run: bool,

    /// Trader proxy wallet address to copy
    #[arg(long)]
    trader_address: String,

    /// Total budget in USD
    #[arg(long)]
    budget: f64,

    /// Percentage of budget to allocate (0-100)
    #[arg(long)]
    copy_percentage: f64,

    /// Maximum trade size per position in USD
    #[arg(long)]
    max_trade_size: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    // Validate
    if !args.dry_run {
        anyhow::bail!("Only --dry-run mode is supported in Phase 2");
    }
    if args.budget <= 0.0 {
        anyhow::bail!("--budget must be positive");
    }
    if !(0.0..=100.0).contains(&args.copy_percentage) {
        anyhow::bail!("--copy-percentage must be between 0 and 100");
    }
    if args.max_trade_size <= 0.0 {
        anyhow::bail!("--max-trade-size must be positive");
    }

    let copy_pct = args.copy_percentage / 100.0;
    let trader_addr: Address = args
        .trader_address
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid trader address: {e}"))?;
    let trader_short_id = &args.trader_address[args.trader_address.len().saturating_sub(6)..];

    let poll_interval_secs: u64 = std::env::var("POLL_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    info!(
        "Starting copytrade (dry-run) — trader={} budget={} copy%={} max_trade={}",
        args.trader_address, args.budget, args.copy_percentage, args.max_trade_size
    );

    let data_client = Client::default();
    let gamma_client = GammaClient::default();
    let mut state = TradingState::new(args.budget);
    let mut seen_hashes: HashSet<String> = HashSet::new();

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
                    compute_target_state(&weights, running_budget, copy_pct, args.max_trade_size);
                let orders = compute_orders(
                    &targets,
                    &state,
                    state.budget_remaining,
                    &HashMap::new(),
                    trader_short_id,
                );
                state.apply_orders(&orders);

                let event = CopytradeEvent {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    trigger: EventTrigger::InitialReplication,
                    detected_trade_hashes: vec![],
                    orders,
                    budget_remaining: state.budget_remaining,
                    total_spent: state.total_spent,
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
                    trader_addr,
                    trader_short_id,
                    &mut state,
                    &mut seen_hashes,
                    copy_pct,
                    args.max_trade_size,
                ).await {
                    warn!("Poll cycle error: {e}");
                }
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
    addr: Address,
    trader_short_id: &str,
    state: &mut TradingState,
    seen_hashes: &mut HashSet<String>,
    copy_pct: f64,
    max_trade_size: f64,
) -> Result<()> {
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
    let targets = compute_target_state(&weights, running_budget, copy_pct, max_trade_size);

    // Build price map with gamma fallback for held assets the trader exited
    let held_assets: Vec<String> = state.holdings.keys().cloned().collect();
    let price_map = build_exit_price_map(gamma, &active_prices, &held_assets).await?;

    let orders = compute_orders(&targets, state, state.budget_remaining, &price_map, trader_short_id);

    if !orders.is_empty() {
        state.apply_orders(&orders);

        let event = CopytradeEvent {
            timestamp: chrono::Utc::now().to_rfc3339(),
            trigger: EventTrigger::TradeDetected,
            detected_trade_hashes: new_hashes,
            orders,
            budget_remaining: state.budget_remaining,
            total_spent: state.total_spent,
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
