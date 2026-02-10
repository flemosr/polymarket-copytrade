use std::collections::HashMap;

use anyhow::Result;
use polymarket_client_sdk::data::Client;
use polymarket_client_sdk::data::types::request::{PositionsRequest, TradesRequest};
use polymarket_client_sdk::data::types::response::{Position, Trade};
use polymarket_client_sdk::gamma::Client as GammaClient;
use polymarket_client_sdk::gamma::types::request::MarketsRequest;
use polymarket_client_sdk::types::Address;
use rust_decimal::Decimal;
use tracing::{debug, warn};

/// Fetch all active (unresolved) positions for the given trader address.
///
/// Paginates through all positions and filters to only include those with
/// `current_value > 0` and `0 < cur_price < 1` (excluding resolved markets).
pub async fn fetch_active_positions(client: &Client, addr: Address) -> Result<Vec<Position>> {
    let mut all = Vec::new();
    let mut offset: i32 = 0;
    let page_size: i32 = 100;

    loop {
        let req = PositionsRequest::builder()
            .user(addr)
            .limit(page_size)?
            .offset(offset)?
            .build();
        let page = client.positions(&req).await?;
        let count = page.len() as i32;

        for pos in page {
            if pos.current_value > Decimal::ZERO
                && pos.cur_price > Decimal::ZERO
                && pos.cur_price < Decimal::ONE
            {
                all.push(pos);
            }
        }

        if count < page_size {
            break;
        }
        offset += page_size;
    }

    debug!("Fetched {} active positions", all.len());
    Ok(all)
}

/// Fetch the most recent trades for the given trader address.
pub async fn fetch_recent_trades(
    client: &Client,
    addr: Address,
    limit: i32,
) -> Result<Vec<Trade>> {
    let req = TradesRequest::builder()
        .user(addr)
        .limit(limit)?
        .build();
    let trades = client.trades(&req).await?;
    debug!("Fetched {} recent trades", trades.len());
    Ok(trades)
}

/// Look up current prices for the given CLOB token IDs via the gamma API.
///
/// Returns a map of `token_id → price`. Tokens not found are omitted.
pub async fn fetch_gamma_prices(
    gamma: &GammaClient,
    token_ids: &[String],
) -> Result<HashMap<String, f64>> {
    if token_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut prices = HashMap::new();

    // Query one token at a time — batch (repeated params) returns 422 on the gamma API.
    for token_id in token_ids {
        let req = MarketsRequest::builder()
            .clob_token_ids(vec![token_id.clone()])
            .build();

        match gamma.markets(&req).await {
            Ok(markets) => {
                for market in &markets {
                    if let Some(price) =
                        extract_token_price(market, token_id)
                    {
                        prices.insert(token_id.clone(), price);
                    }
                }
            }
            Err(e) => {
                warn!("Gamma lookup failed for token {token_id}: {e}");
            }
        }
    }

    debug!("Gamma resolved prices for {}/{} tokens", prices.len(), token_ids.len());
    Ok(prices)
}

/// Build a comprehensive price map for exit pricing.
///
/// 1. Starts from `active_prices` (built from active positions).
/// 2. For any `needed` assets not found, queries the gamma API.
pub async fn build_exit_price_map(
    gamma: &GammaClient,
    active_prices: &HashMap<String, f64>,
    needed: &[String],
) -> Result<HashMap<String, f64>> {
    let mut map = active_prices.clone();

    let missing: Vec<String> = needed
        .iter()
        .filter(|a| !map.contains_key(a.as_str()))
        .cloned()
        .collect();

    if missing.is_empty() {
        return Ok(map);
    }

    debug!("{} held assets missing from active positions, querying gamma", missing.len());

    let gamma_prices = fetch_gamma_prices(gamma, &missing).await?;
    for (asset, price) in gamma_prices {
        map.insert(asset, price);
    }

    Ok(map)
}

/// Extract the price for a specific token ID from a gamma Market response.
///
/// `outcome_prices` and `clob_token_ids` are parallel lists (JSON-encoded string
/// arrays or comma-separated). Find the index of `token_id` in `clob_token_ids`
/// and return the price at that index.
fn extract_token_price(
    market: &polymarket_client_sdk::gamma::types::response::Market,
    token_id: &str,
) -> Option<f64> {
    let prices_str = market.outcome_prices.as_deref()?;
    let tokens_str = market.clob_token_ids.as_deref()?;

    let token_ids = parse_string_list(tokens_str);
    let prices = parse_string_list(prices_str);

    let idx = token_ids.iter().position(|t| t == token_id)?;
    prices.get(idx)?.parse::<f64>().ok()
}

/// Parse a value that may be a JSON array `["a","b"]` or comma-separated `a,b`.
fn parse_string_list(s: &str) -> Vec<String> {
    // Try JSON array first
    if let Ok(arr) = serde_json::from_str::<Vec<String>>(s) {
        return arr;
    }
    // Fall back to comma-separated
    s.split(',').map(|v| v.trim().to_string()).collect()
}
