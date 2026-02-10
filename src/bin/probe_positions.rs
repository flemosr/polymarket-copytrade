//! Probe 1B: REST Positions endpoint
//!
//! Hits GET https://data-api.polymarket.com/positions?user=<addr> and documents:
//! - Response shape and fields
//! - Active positions (currentValue > 0)
//! - Portfolio weight computation
//! - Pagination behavior

use anyhow::Result;
use polymarket_copytrade::{DATA_API_BASE, TRADER_ADDRESS};
use serde_json::Value;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<()> {
    let client = reqwest::Client::new();
    let base_url = format!("{}/positions", DATA_API_BASE);

    println!("=== Probe 1B: REST Positions ===");
    println!("Trader: {}", TRADER_ADDRESS);
    println!();

    // 1. Fetch positions (default params)
    println!("--- 1. Fetch positions (default) ---");
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

    let positions = body.as_array();
    match positions {
        Some(arr) => {
            println!("Position count: {}", arr.len());
            if let Some(first) = arr.first() {
                println!("\nSample position (first):");
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

    // 2. Paginate to get all positions
    println!("--- 2. Paginating all positions ---");
    let mut all_positions: Vec<Value> = Vec::new();
    let mut offset = 0;
    let limit = 100;
    loop {
        let resp = client
            .get(&base_url)
            .query(&[
                ("user", TRADER_ADDRESS),
                ("limit", &limit.to_string()),
                ("offset", &offset.to_string()),
            ])
            .send()
            .await?;
        let body: Value = resp.json().await?;
        match body.as_array() {
            Some(arr) if !arr.is_empty() => {
                println!("  Offset {}: got {} positions", offset, arr.len());
                all_positions.extend(arr.clone());
                if arr.len() < limit {
                    break;
                }
                offset += limit;
            }
            _ => break,
        }
    }
    println!("Total positions fetched: {}", all_positions.len());
    println!();

    // 3. Filter active positions and compute portfolio weights
    println!("--- 3. Active positions & portfolio weights ---");
    let mut active: Vec<(&Value, f64)> = Vec::new();
    for pos in &all_positions {
        let current_value = parse_f64(pos, "currentValue").unwrap_or(0.0);
        if current_value > 0.0 {
            active.push((pos, current_value));
        }
    }

    let total_value: f64 = active.iter().map(|(_, v)| v).sum();
    println!("Active positions: {} (of {} total)", active.len(), all_positions.len());
    println!("Total portfolio value: ${:.2}", total_value);
    println!();

    // Sort by value descending
    active.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Print table
    println!(
        "{:<60} {:>12} {:>8} {:>10} {:>10}",
        "Market", "Value ($)", "Weight%", "Outcome", "CurPrice"
    );
    println!("{}", "-".repeat(104));
    for (pos, value) in &active {
        let title = pos.get("title").and_then(|v| v.as_str()).unwrap_or("?");
        let title_truncated: String = title.chars().take(58).collect();
        let weight = value / total_value * 100.0;
        let outcome = pos.get("outcome").and_then(|v| v.as_str()).unwrap_or("?");
        let cur_price = parse_f64(pos, "curPrice").unwrap_or(0.0);
        println!(
            "{:<60} {:>12.2} {:>7.2}% {:>10} {:>10.4}",
            title_truncated, value, weight, outcome, cur_price
        );
    }
    println!();

    // 4. Document field completeness
    println!("--- 4. Field completeness check ---");
    let important_fields = [
        "proxyWallet",
        "asset",
        "conditionId",
        "title",
        "outcome",
        "size",
        "avgPrice",
        "currentValue",
        "curPrice",
        "cashPnl",
        "percentPnl",
        "marketSlug",
    ];
    if let Some(first) = all_positions.first() {
        for field in &important_fields {
            let present = first.get(*field).is_some();
            let value_preview = first
                .get(*field)
                .map(|v| {
                    let s = v.to_string();
                    if s.len() > 50 {
                        format!("{}...", &s[..50])
                    } else {
                        s
                    }
                })
                .unwrap_or_else(|| "MISSING".to_string());
            println!("  {:<20} {} ({})", field, if present { "✓" } else { "✗" }, value_preview);
        }
    }
    println!();

    println!("=== Probe 1B Complete ===");
    Ok(())
}

fn parse_f64(val: &Value, field: &str) -> Option<f64> {
    val.get(field).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
    })
}
