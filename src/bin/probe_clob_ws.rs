//! Probe 1D: CLOB WebSocket
//!
//! Connects to wss://ws-subscriptions-clob.polymarket.com/ws/market and:
//! - Subscribes to market channel using token/asset IDs (not condition IDs)
//! - Observes message format (book, last_trade_price, price_change events)
//! - Checks if trader identity is present in market data
//! - Documents available event types
//! - Logs messages for 30 seconds

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use polymarket_copytrade::CLOB_WS_MARKET_URL;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Probe 1D: CLOB WebSocket ===");
    println!("URL: {}", CLOB_WS_MARKET_URL);
    println!();

    // First, fetch a real active asset/token ID from the trader's positions
    let asset_id = get_active_asset_id().await?;
    println!("Using asset_id (token ID): {}", asset_id);
    println!();

    // Connect to /ws/market endpoint
    println!("--- Connecting ---");
    let (ws_stream, response) = connect_async(CLOB_WS_MARKET_URL).await?;
    println!("Connected! Response status: {}", response.status());
    println!();

    let (mut write, mut read) = ws_stream.split();

    // Subscribe using the correct format:
    // - type: "market"
    // - assets_ids: array of token/asset IDs (not condition IDs)
    let subscribe_msg = json!({
        "type": "market",
        "assets_ids": [&asset_id],
        "custom_feature_enabled": true
    });

    println!("--- Sending subscription ---");
    println!("  {}", subscribe_msg);
    write
        .send(Message::Text(subscribe_msg.to_string().into()))
        .await?;
    println!();

    // Listen for messages for 30 seconds, sending "PING" text keepalive every 10s
    println!("--- Listening for 30 seconds ---");
    let start = Instant::now();
    let timeout = Duration::from_secs(30);
    let mut msg_count = 0;
    let mut event_types = std::collections::HashSet::new();
    let mut has_trader_identity = false;
    let mut last_ping = Instant::now();

    loop {
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            break;
        }

        // Send text "PING" keepalive every 10 seconds (CLOB WS protocol)
        if last_ping.elapsed() >= Duration::from_secs(10) {
            let _ = write.send(Message::Text("PING".into())).await;
            last_ping = Instant::now();
        }

        match tokio::time::timeout(Duration::from_secs(1), read.next()).await {
            Ok(Some(Ok(msg))) => {
                msg_count += 1;
                let elapsed = start.elapsed();
                match &msg {
                    Message::Text(text) => {
                        // Skip PONG responses
                        if text.as_str() == "PONG" {
                            continue;
                        }

                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text.as_str()) {
                            // Track event types
                            let event_type = parsed
                                .get("event_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            event_types.insert(event_type.to_string());

                            // Check for trader identity fields
                            let identity_fields = [
                                "maker", "taker", "user", "owner", "trader", "proxyWallet",
                            ];
                            for field in &identity_fields {
                                if parsed.get(*field).is_some() {
                                    has_trader_identity = true;
                                    println!(
                                        "  *** Found trader identity field '{}' in message!",
                                        field
                                    );
                                }
                                // Also check nested in data arrays
                                if let Some(arr) = parsed.get("data").and_then(|d| d.as_array()) {
                                    for item in arr {
                                        if item.get(*field).is_some() {
                                            has_trader_identity = true;
                                            println!(
                                                "  *** Found trader identity field '{}' in data item!",
                                                field
                                            );
                                        }
                                    }
                                }
                            }

                            if msg_count <= 10 {
                                println!(
                                    "[{:.1}s] #{} event_type={}: {}",
                                    elapsed.as_secs_f64(),
                                    msg_count,
                                    event_type,
                                    if text.len() > 500 {
                                        format!("{}...", &text[..500])
                                    } else {
                                        text.to_string()
                                    }
                                );
                            } else if msg_count % 10 == 0 {
                                println!(
                                    "[{:.1}s] #{} event_type={} ({} msgs so far, types: {:?})",
                                    elapsed.as_secs_f64(),
                                    msg_count,
                                    event_type,
                                    msg_count,
                                    event_types
                                );
                            }
                        } else {
                            println!(
                                "[{:.1}s] #{} (non-JSON): {}",
                                elapsed.as_secs_f64(),
                                msg_count,
                                if text.len() > 200 {
                                    format!("{}...", &text[..200])
                                } else {
                                    text.to_string()
                                }
                            );
                        }
                    }
                    Message::Pong(_) => {}
                    Message::Ping(_) => {
                        println!("[{:.1}s] Ping", elapsed.as_secs_f64());
                    }
                    Message::Close(frame) => {
                        println!("[{:.1}s] Close: {:?}", elapsed.as_secs_f64(), frame);
                        break;
                    }
                    _ => {}
                }
            }
            Ok(Some(Err(e))) => {
                println!("WebSocket error: {}", e);
                break;
            }
            Ok(None) => {
                println!("WebSocket stream ended");
                break;
            }
            Err(_) => continue, // timeout on read, loop back
        }
    }

    println!();
    println!("--- Summary ---");
    println!("Total messages received: {}", msg_count);
    println!("Duration: {:.1}s", start.elapsed().as_secs_f64());
    println!("Event types seen: {:?}", event_types);
    println!(
        "Trader identity in market data: {}",
        if has_trader_identity { "YES" } else { "NO" }
    );
    println!();
    println!("Note: The CLOB market channel provides book snapshots, price changes,");
    println!("and last_trade_price events. It does NOT include trader identity.");
    println!("The user channel requires the trader's own API credentials.");
    println!();

    println!("=== Probe 1D Complete ===");
    Ok(())
}

/// Fetch an asset/token ID from the trader's active positions
async fn get_active_asset_id() -> Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(&format!(
            "{}/positions",
            polymarket_copytrade::DATA_API_BASE
        ))
        .query(&[
            ("user", polymarket_copytrade::TRADER_ADDRESS),
            ("limit", "20"),
        ])
        .send()
        .await?;
    let body: serde_json::Value = resp.json().await?;
    let positions = body
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Response is not an array"))?;

    for pos in positions {
        let value = pos
            .get("currentValue")
            .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0.0);
        if value > 100.0 {
            if let Some(asset) = pos.get("asset").and_then(|v| v.as_str()) {
                let title = pos.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                println!("Found active position: {} (value=${:.2})", title, value);
                println!("  asset/token ID: {}", asset);
                return Ok(asset.to_string());
            }
        }
    }
    anyhow::bail!("No active positions with value > $100 found")
}
