//! Probe: Fetch our own Safe wallet positions via the SDK data client.
//!
//! Derives the Safe address from POLYMARKET_PRIVATE_KEY, then queries
//! the data API using the typed SDK client.

use std::str::FromStr;

use anyhow::{Context, Result};
use polymarket_client_sdk::auth::{LocalSigner, Signer};
use polymarket_client_sdk::data;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::{POLYGON, PRIVATE_KEY_VAR, derive_safe_wallet};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let private_key = std::env::var(PRIVATE_KEY_VAR).context("POLYMARKET_PRIVATE_KEY not set")?;
    let signer = LocalSigner::from_str(&private_key)?.with_chain_id(Some(POLYGON));
    let safe = derive_safe_wallet(signer.address(), POLYGON)
        .context("failed to derive Safe address")?;

    println!("Safe address: {safe}\n");

    let client = data::Client::default();
    let req = PositionsRequest::builder()
        .user(safe)
        .limit(100)?
        .build();

    let positions = client.positions(&req).await?;

    if positions.is_empty() {
        println!("No positions found.");
        return Ok(());
    }

    println!("{} position(s):\n", positions.len());
    for p in &positions {
        println!("  asset:        {}...{}", &p.asset.to_string()[..8], &p.asset.to_string()[p.asset.to_string().len()-8..]);
        println!("  condition_id: {}", p.condition_id);
        println!("  outcome:      {}", p.outcome);
        println!("  size:         {}", p.size);
        println!("  avg_price:    {}", p.avg_price);
        println!("  cur_price:    {}", p.cur_price);
        println!("  current_value:{}", p.current_value);
        println!("  initial_value:{}", p.initial_value);
        println!("  proxy_wallet: {}", p.proxy_wallet);
        println!();
    }

    Ok(())
}
