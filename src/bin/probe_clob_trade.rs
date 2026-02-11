//! Probe 3A: CLOB Authentication & Order Round-Trip
//!
//! Validates the full CLOB trading flow:
//! 1. Authenticate with private key (GnosisSafe signature type)
//! 2. Check balance & allowance
//! 3. Pick a market (env var or auto-select)
//! 4. Query tick size & neg-risk metadata
//! 5. Place an unfillable limit order (BUY at $0.01, 5 shares)
//! 6. Query the order
//! 7. Cancel the order
//! 8. (Optional --execute) Place a FAK market buy ($2.00)
//!
//! Safe by default — the limit order is deliberately unfillable.
//! Only the --execute flag risks real funds ($2.00).

use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::Parser;
use polymarket_client_sdk::clob::types::request::BalanceAllowanceRequest;
use polymarket_client_sdk::clob::types::{Amount, OrderType, Side, SignatureType};
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::auth::{LocalSigner, Signer};
use polymarket_client_sdk::{POLYGON, PRIVATE_KEY_VAR, derive_safe_wallet};
use rust_decimal_macros::dec;

use polymarket_copytrade::CLOB_API_BASE;

#[derive(Parser)]
#[command(about = "Probe CLOB auth, limit order, cancel, and optional market order")]
struct Cli {
    /// Actually execute a tiny ($0.10) market order — costs real funds
    #[arg(long)]
    execute: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();

    println!("=== Probe 3A: CLOB Auth & Order Round-Trip ===\n");

    // ── Step 1: Setup ──────────────────────────────────────────────
    println!("--- Step 1: Setup ---");
    let private_key =
        std::env::var(PRIVATE_KEY_VAR).context("POLYMARKET_PRIVATE_KEY not set in env")?;
    let signer = LocalSigner::from_str(&private_key)
        .context("invalid private key")?
        .with_chain_id(Some(POLYGON));

    let eoa = signer.address();
    println!("EOA address:  {eoa}");

    let safe = derive_safe_wallet(eoa, POLYGON).context("failed to derive Safe address")?;
    println!("Safe address: {safe}");
    println!();

    // ── Step 2: Authenticate ───────────────────────────────────────
    println!("--- Step 2: Authenticate ---");
    let config = Config::builder().use_server_time(true).build();
    let client = Client::new(CLOB_API_BASE, config)?
        .authentication_builder(&signer)
        .signature_type(SignatureType::GnosisSafe)
        .authenticate()
        .await
        .context("CLOB authentication failed")?;
    println!("Authenticated successfully");

    let api_keys = client.api_keys().await?;
    println!("API keys: {api_keys:#?}");
    println!();

    // ── Step 3: Balance & allowance ────────────────────────────────
    println!("--- Step 3: Balance & Allowance ---");
    let bal = client
        .balance_allowance(BalanceAllowanceRequest::default())
        .await?;
    println!("USDC balance: {}", bal.balance);
    println!("Allowances:   {:#?}", bal.allowances);
    if bal.balance.is_zero() {
        println!("WARNING: balance is 0 — limit order will still work (unfillable price)");
    }
    println!();

    // ── Step 4: Pick a market ──────────────────────────────────────
    println!("--- Step 4: Pick a Market ---");
    let token_id = pick_token_id().await?;
    println!("Token ID: {token_id}");
    println!();

    // ── Step 5: Market metadata ────────────────────────────────────
    println!("--- Step 5: Market Metadata ---");
    let tick = client.tick_size(&token_id).await?;
    println!("Tick size: {tick:?}");
    let neg_risk = client.neg_risk(&token_id).await?;
    println!("Neg risk:  {neg_risk:?}");
    println!();

    // ── Step 6: Place unfillable limit order ───────────────────────
    println!("--- Step 6: Place Limit Order (BUY @ $0.01, 1 share) ---");
    let signable = client
        .limit_order()
        .token_id(&token_id)
        .price(dec!(0.01))
        .size(dec!(5.0))
        .side(Side::Buy)
        .build()
        .await?;
    let signed = client.sign(&signer, signable).await?;
    let post_resp = client.post_order(signed).await?;
    println!("Response: {post_resp:#?}");

    let order_id = post_resp.order_id.clone();
    if !post_resp.success {
        println!("WARNING: order post reported failure — continuing to query/cancel anyway");
    }
    println!();

    // ── Step 7: Query the order ────────────────────────────────────
    println!("--- Step 7: Query Order ---");
    if !order_id.is_empty() {
        let order = client.order(&order_id).await?;
        println!("Order: {order:#?}");
    } else {
        println!("No order_id returned — skipping query");
    }
    println!();

    // ── Step 8: Cancel the order ───────────────────────────────────
    println!("--- Step 8: Cancel Order ---");
    if !order_id.is_empty() {
        let cancel = client.cancel_order(&order_id).await?;
        println!("Cancel result: {cancel:#?}");
    } else {
        println!("No order_id — skipping cancel");
    }
    println!();

    // ── Step 9: Optional tiny market order ─────────────────────────
    if cli.execute {
        println!("--- Step 9: Execute Market Order (FAK BUY $1.00) ---");
        println!("WARNING: This will spend ~$1.00 of real USDC");

        if bal.balance < dec!(1.00) {
            bail!(
                "insufficient balance ({}) for $1.00 market order",
                bal.balance
            );
        }

        let signable = client
            .market_order()
            .token_id(&token_id)
            .side(Side::Buy)
            .amount(Amount::usdc(dec!(1.00))?)
            .order_type(OrderType::FAK)
            .build()
            .await?;
        let signed = client.sign(&signer, signable).await?;
        let mkt_resp = client.post_order(signed).await?;
        println!("Market order response: {mkt_resp:#?}");

        if mkt_resp.success {
            println!("\nWaiting 5s for trade to appear in data API...");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            let http = reqwest::Client::new();
            let trades_url = format!(
                "https://data-api.polymarket.com/trades?user={safe}&limit=5"
            );
            let resp: serde_json::Value = http.get(&trades_url).send().await?.json().await?;
            if let Some(arr) = resp.as_array() {
                println!("Recent trades for Safe ({}):", safe);
                for t in arr.iter().take(3) {
                    let side = t.get("side").and_then(|v| v.as_str()).unwrap_or("?");
                    let size = t.get("size").and_then(|v| v.as_str()).unwrap_or("?");
                    let price = t.get("price").and_then(|v| v.as_str()).unwrap_or("?");
                    let hash = t
                        .get("transactionHash")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    println!("  {side} {size} @ {price}  tx={hash}");
                }
            } else {
                println!("Unexpected response: {resp}");
            }
        }
        println!();
    } else {
        println!("--- Step 9: Skipped (use --execute to place a $1.00 market order) ---\n");
    }

    println!("=== Probe 3A Complete ===");
    Ok(())
}

/// Resolve a token ID to trade on.
/// Prefers PROBE_TOKEN_ID env var; falls back to fetching a liquid market.
async fn pick_token_id() -> Result<String> {
    if let Ok(id) = std::env::var("PROBE_TOKEN_ID") {
        if !id.is_empty() {
            println!("(from PROBE_TOKEN_ID env var)");
            return Ok(id);
        }
    }

    // Fallback: grab a token from a top-volume trader's active positions
    println!("(auto-selecting from leaderboard)");
    let http = reqwest::Client::new();

    // Fetch top trader by daily volume
    let lb_url = "https://data-api.polymarket.com/v1/leaderboard?limit=1&orderBy=vol&timePeriod=day";
    let lb: serde_json::Value = http.get(lb_url).send().await?.json().await?;
    let trader = lb
        .as_array()
        .and_then(|a| a.first())
        .and_then(|t| t.get("proxyWallet").or(t.get("address")))
        .and_then(|v| v.as_str())
        .context("could not find a trader on leaderboard")?
        .to_string();
    println!("  top trader: {trader}");

    // Fetch their active positions to get a token ID
    let pos_url = format!(
        "https://data-api.polymarket.com/positions?user={trader}&limit=5&sortBy=value&sortOrder=desc"
    );
    let positions: serde_json::Value = http.get(&pos_url).send().await?.json().await?;
    let token = positions
        .as_array()
        .and_then(|arr| {
            arr.iter().find_map(|p| {
                let val = p.get("currentValue")?.as_f64()?;
                let price = p.get("curPrice")?.as_f64()?;
                if val > 0.0 && price > 0.05 && price < 0.95 {
                    p.get("asset").and_then(|v| v.as_str()).map(String::from)
                } else {
                    None
                }
            })
        })
        .context("could not find a suitable active position with a token ID")?;

    Ok(token)
}
