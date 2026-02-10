//! Probe 1A: REST Trades endpoint
//!
//! Hits GET https://data-api.polymarket.com/trades?user=<addr> and documents:
//! - Response shape and fields
//! - Pagination (limit/offset)
//! - Filtering (side=BUY)
//! - Latency over multiple requests
//! - Dedup field (transactionHash) uniqueness

use anyhow::Result;
use polymarket_copytrade::{DATA_API_BASE, TRADER_ADDRESS};
use serde_json::Value;
use std::collections::HashSet;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<()> {
    let client = reqwest::Client::new();
    let base_url = format!("{}/trades", DATA_API_BASE);

    println!("=== Probe 1A: REST Trades ===");
    println!("Trader: {}", TRADER_ADDRESS);
    println!();

    // 1. Fetch recent trades (default params)
    println!("--- 1. Fetch recent trades (default) ---");
    let start = Instant::now();
    let resp = client
        .get(&base_url)
        .query(&[("user", TRADER_ADDRESS)])
        .send()
        .await?;
    let latency = start.elapsed();
    let status = resp.status();
    let body: Value = resp.json().await?;
    println!("Status: {}", status);
    println!("Latency: {:?}", latency);

    let trades = body.as_array();
    match trades {
        Some(arr) => {
            println!("Trade count: {}", arr.len());
            if let Some(first) = arr.first() {
                println!("\nSample trade (first):");
                println!("{}", serde_json::to_string_pretty(first)?);
                println!("\nFields present:");
                if let Some(obj) = first.as_object() {
                    for key in obj.keys() {
                        println!("  - {}", key);
                    }
                }
            }
        }
        None => {
            println!("Response is not an array:");
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
    }
    println!();

    // 2. Test pagination: limit=5
    println!("--- 2. Pagination: limit=5 ---");
    let start = Instant::now();
    let resp = client
        .get(&base_url)
        .query(&[("user", TRADER_ADDRESS), ("limit", "5")])
        .send()
        .await?;
    let latency = start.elapsed();
    let body: Value = resp.json().await?;
    let count = body.as_array().map(|a| a.len()).unwrap_or(0);
    println!("Returned {} trades (latency: {:?})", count, latency);
    println!();

    // 3. Test pagination: limit=5, offset=5
    println!("--- 3. Pagination: limit=5, offset=5 ---");
    let start = Instant::now();
    let resp = client
        .get(&base_url)
        .query(&[
            ("user", TRADER_ADDRESS),
            ("limit", "5"),
            ("offset", "5"),
        ])
        .send()
        .await?;
    let latency = start.elapsed();
    let body: Value = resp.json().await?;
    let count = body.as_array().map(|a| a.len()).unwrap_or(0);
    println!("Returned {} trades (latency: {:?})", count, latency);
    println!();

    // 4. Test filter: side=BUY
    println!("--- 4. Filter: side=BUY ---");
    let start = Instant::now();
    let resp = client
        .get(&base_url)
        .query(&[
            ("user", TRADER_ADDRESS),
            ("side", "BUY"),
            ("limit", "5"),
        ])
        .send()
        .await?;
    let latency = start.elapsed();
    let body: Value = resp.json().await?;
    if let Some(arr) = body.as_array() {
        println!("Returned {} BUY trades (latency: {:?})", arr.len(), latency);
        for trade in arr.iter().take(2) {
            let side = trade.get("side").and_then(|v| v.as_str()).unwrap_or("?");
            println!("  side={}", side);
        }
    }
    println!();

    // 5. Latency measurements over 5 requests
    println!("--- 5. Latency over 5 requests (limit=1) ---");
    let mut latencies = Vec::new();
    for i in 0..5 {
        let start = Instant::now();
        let _resp = client
            .get(&base_url)
            .query(&[("user", TRADER_ADDRESS), ("limit", "1")])
            .send()
            .await?
            .text()
            .await?;
        let latency = start.elapsed();
        println!("  Request {}: {:?}", i + 1, latency);
        latencies.push(latency);
    }
    let avg = latencies.iter().sum::<std::time::Duration>() / latencies.len() as u32;
    println!("  Average: {:?}", avg);
    println!();

    // 6. Check transactionHash uniqueness for dedup
    println!("--- 6. transactionHash uniqueness check ---");
    let resp = client
        .get(&base_url)
        .query(&[("user", TRADER_ADDRESS), ("limit", "100")])
        .send()
        .await?;
    let body: Value = resp.json().await?;
    if let Some(arr) = body.as_array() {
        let mut hashes = HashSet::new();
        let mut missing_hash = 0;
        let mut duplicate_count = 0;
        for trade in arr {
            if let Some(hash) = trade.get("transactionHash").and_then(|v| v.as_str()) {
                if !hashes.insert(hash.to_string()) {
                    duplicate_count += 1;
                }
            } else {
                missing_hash += 1;
            }
        }
        println!("  Total trades: {}", arr.len());
        println!("  Unique hashes: {}", hashes.len());
        println!("  Duplicate hashes: {}", duplicate_count);
        println!("  Missing transactionHash: {}", missing_hash);

        // Also check if there's an 'id' field
        let has_id = arr
            .first()
            .and_then(|t| t.get("id"))
            .is_some();
        println!("  Has 'id' field: {}", has_id);
    }
    println!();

    println!("=== Probe 1A Complete ===");
    Ok(())
}
