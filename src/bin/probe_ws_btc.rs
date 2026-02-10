//! Targeted WebSocket test using the active BTC Up/Down market (Feb 10, 12PM ET)
//!
//! Tests both RTDS and CLOB WS simultaneously on a known high-activity market:
//! - RTDS: filter activity/trades for this specific market via event_slug
//! - CLOB WS: subscribe to both Up and Down token IDs
//! - Compare message rates, latency, and content between the two sources

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use polymarket_copytrade::{CLOB_WS_MARKET_URL, RTDS_WS_URL};
use serde_json::json;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

// BTC Up or Down - February 10, 12PM ET
const MARKET_SLUG: &str = "bitcoin-up-or-down-february-10-12pm-et";
const CONDITION_ID: &str = "0x1bb4acb9d863d6aed0405c497ec852f4cbf597e0a8c741a62b549633cbccabeb";
const TOKEN_UP: &str =
    "75606474407719766631814632126542587195218111373589874641827458287512369110261";
const TOKEN_DOWN: &str =
    "46959492853924666183391306723049011783439401201789086075226006963592441806455";

#[derive(Debug)]
struct WsEvent {
    source: &'static str,
    event_type: String,
    timestamp: Instant,
    payload_preview: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== WebSocket Comparison Test: BTC Up/Down Market ===");
    println!("Market: {}", MARKET_SLUG);
    println!("Condition: {}", CONDITION_ID);
    println!("Token Up:   {}", TOKEN_UP);
    println!("Token Down: {}", TOKEN_DOWN);
    println!();

    let (tx, mut rx) = mpsc::unbounded_channel::<WsEvent>();
    let duration = Duration::from_secs(30);
    let start = Instant::now();

    // Spawn RTDS listener
    let tx_rtds = tx.clone();
    let rtds_handle = tokio::spawn(async move {
        if let Err(e) = run_rtds(tx_rtds, duration).await {
            eprintln!("RTDS error: {}", e);
        }
    });

    // Spawn CLOB WS listener
    let tx_clob = tx.clone();
    let clob_handle = tokio::spawn(async move {
        if let Err(e) = run_clob_ws(tx_clob, duration).await {
            eprintln!("CLOB WS error: {}", e);
        }
    });

    drop(tx); // Drop our sender so rx closes when both tasks finish

    // Collect all events
    let mut rtds_events = Vec::new();
    let mut clob_events = Vec::new();
    let mut total_rtds_all = 0u64; // total RTDS messages including non-BTC

    while let Some(event) = rx.recv().await {
        let elapsed = event.timestamp.duration_since(start);
        match event.source {
            "rtds" => {
                println!(
                    "[{:.1}s] RTDS  | {} | {}",
                    elapsed.as_secs_f64(),
                    event.event_type,
                    event.payload_preview
                );
                rtds_events.push(event);
            }
            "rtds_total" => {
                total_rtds_all = event.payload_preview.parse().unwrap_or(0);
            }
            "clob" => {
                println!(
                    "[{:.1}s] CLOB  | {} | {}",
                    elapsed.as_secs_f64(),
                    event.event_type,
                    event.payload_preview
                );
                clob_events.push(event);
            }
            _ => {}
        }
    }

    rtds_handle.await?;
    clob_handle.await?;

    // Summary
    println!();
    println!("=== Summary ===");
    println!("Duration: {:.1}s", start.elapsed().as_secs_f64());
    println!();
    println!("RTDS:");
    println!("  Total messages (all markets): {}", total_rtds_all);
    println!("  Messages for BTC market: {}", rtds_events.len());
    if !rtds_events.is_empty() {
        let mut sides: HashMap<String, usize> = HashMap::new();
        for e in &rtds_events {
            *sides.entry(e.event_type.clone()).or_default() += 1;
        }
        println!("  By type: {:?}", sides);
    }
    println!();
    println!("CLOB WS:");
    println!("  Total messages: {}", clob_events.len());
    if !clob_events.is_empty() {
        let mut types: HashMap<String, usize> = HashMap::new();
        for e in &clob_events {
            *types.entry(e.event_type.clone()).or_default() += 1;
        }
        println!("  By event_type: {:?}", types);
    }
    println!();

    println!("=== Test Complete ===");
    Ok(())
}

async fn run_rtds(tx: mpsc::UnboundedSender<WsEvent>, duration: Duration) -> Result<()> {
    let (ws, _) = connect_async(RTDS_WS_URL).await?;
    let (mut write, mut read) = ws.split();

    // Subscribe to activity/trades — try with event_slug filter
    let sub_filtered = json!({
        "action": "subscribe",
        "subscriptions": [{
            "topic": "activity",
            "type": "trades",
            "filters": serde_json::json!({"event_slug": MARKET_SLUG}).to_string()
        }]
    });
    println!("[RTDS] Sending filtered subscription: {}", sub_filtered);
    write
        .send(Message::Text(sub_filtered.to_string().into()))
        .await?;

    // Also subscribe unfiltered as fallback
    let sub_all = json!({
        "action": "subscribe",
        "subscriptions": [{
            "topic": "activity",
            "type": "trades"
        }]
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    println!("[RTDS] Sending unfiltered subscription: {}", sub_all);
    write
        .send(Message::Text(sub_all.to_string().into()))
        .await?;

    let start = Instant::now();
    let mut total_msgs = 0u64;
    let mut last_ping = Instant::now();

    loop {
        if start.elapsed() >= duration {
            break;
        }

        if last_ping.elapsed() >= Duration::from_secs(5) {
            let _ = write.send(Message::Ping(vec![].into())).await;
            last_ping = Instant::now();
        }

        match tokio::time::timeout(Duration::from_secs(1), read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                total_msgs += 1;
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text.as_str()) {
                    // Check if this trade is for our BTC market
                    let payload = parsed.get("payload");
                    let event_slug = payload
                        .and_then(|p| p.get("eventSlug"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let condition_id = payload
                        .and_then(|p| p.get("conditionId"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    if event_slug == MARKET_SLUG
                        || condition_id == CONDITION_ID
                    {
                        let side = payload
                            .and_then(|p| p.get("side"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let size = payload
                            .and_then(|p| p.get("size"))
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        let price = payload
                            .and_then(|p| p.get("price"))
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        let outcome = payload
                            .and_then(|p| p.get("outcome"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let wallet = payload
                            .and_then(|p| p.get("proxyWallet"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let tx_hash = payload
                            .and_then(|p| p.get("transactionHash"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");

                        let _ = tx.send(WsEvent {
                            source: "rtds",
                            event_type: format!("{}/{}", side, outcome),
                            timestamp: Instant::now(),
                            payload_preview: format!(
                                "size={:.2} price={:.4} wallet={}..{} tx={}..{}",
                                size,
                                price,
                                &wallet[..6.min(wallet.len())],
                                &wallet[wallet.len().saturating_sub(4)..],
                                &tx_hash[..10.min(tx_hash.len())],
                                &tx_hash[tx_hash.len().saturating_sub(6)..]
                            ),
                        });
                    }
                }
            }
            Ok(Some(Ok(_))) => {} // pong, ping, etc.
            Ok(Some(Err(e))) => {
                eprintln!("[RTDS] Error: {}", e);
                break;
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    let _ = tx.send(WsEvent {
        source: "rtds_total",
        event_type: String::new(),
        timestamp: Instant::now(),
        payload_preview: total_msgs.to_string(),
    });

    Ok(())
}

async fn run_clob_ws(tx: mpsc::UnboundedSender<WsEvent>, duration: Duration) -> Result<()> {
    let (ws, _) = connect_async(CLOB_WS_MARKET_URL).await?;
    let (mut write, mut read) = ws.split();

    // Subscribe to both Up and Down tokens
    let sub = json!({
        "type": "market",
        "assets_ids": [TOKEN_UP, TOKEN_DOWN],
        "custom_feature_enabled": true
    });
    println!("[CLOB] Sending subscription: {}", sub);
    write.send(Message::Text(sub.to_string().into())).await?;

    let start = Instant::now();
    let mut last_ping = Instant::now();

    loop {
        if start.elapsed() >= duration {
            break;
        }

        if last_ping.elapsed() >= Duration::from_secs(10) {
            let _ = write.send(Message::Text("PING".into())).await;
            last_ping = Instant::now();
        }

        match tokio::time::timeout(Duration::from_secs(1), read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if text.as_str() == "PONG" {
                    continue;
                }

                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text.as_str()) {
                    let event_type = parsed
                        .get("event_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");

                    // For book events, just note the size
                    let preview = match event_type {
                        "book" => {
                            let asset = parsed
                                .get("asset_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let side = if asset == TOKEN_UP { "Up" } else { "Down" };
                            let bids = parsed
                                .get("bids")
                                .and_then(|v| v.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0);
                            let asks = parsed
                                .get("asks")
                                .and_then(|v| v.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0);
                            format!("{}: {} bids, {} asks", side, bids, asks)
                        }
                        "last_trade_price" => {
                            let asset = parsed
                                .get("asset_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let side = if asset == TOKEN_UP { "Up" } else { "Down" };
                            let price = parsed
                                .get("price")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            format!("{}: price={}", side, price)
                        }
                        "price_change" => {
                            let asset = parsed
                                .get("asset_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let side = if asset == TOKEN_UP { "Up" } else { "Down" };
                            // Show raw JSON for first few
                            let snippet = if text.len() > 200 {
                                format!("{}...", &text[..200])
                            } else {
                                text.to_string()
                            };
                            format!("{}: {}", side, snippet)
                        }
                        _ => {
                            if text.len() > 150 {
                                format!("{}...", &text[..150])
                            } else {
                                text.to_string()
                            }
                        }
                    };

                    let _ = tx.send(WsEvent {
                        source: "clob",
                        event_type: event_type.to_string(),
                        timestamp: Instant::now(),
                        payload_preview: preview,
                    });
                } else if text.as_str() != "PONG" {
                    let _ = tx.send(WsEvent {
                        source: "clob",
                        event_type: "raw".to_string(),
                        timestamp: Instant::now(),
                        payload_preview: if text.len() > 100 {
                            format!("{}...", &text[..100])
                        } else {
                            text.to_string()
                        },
                    });
                }
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(e))) => {
                eprintln!("[CLOB] Error: {}", e);
                break;
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    Ok(())
}
