//! Probe 1C: RTDS WebSocket
//!
//! Connects to wss://ws-live-data.polymarket.com and:
//! - Tests connectivity with crypto_prices channel (correct format)
//! - Tries subscribing to activity/trades topics (likely non-existent)
//! - Documents available channels and message formats
//! - Logs all received messages for 30 seconds

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use polymarket_copytrade::RTDS_WS_URL;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Probe 1C: RTDS WebSocket ===");
    println!("URL: {}", RTDS_WS_URL);
    println!();

    // Connect
    println!("--- Connecting ---");
    let (ws_stream, response) = connect_async(RTDS_WS_URL).await?;
    println!("Connected! Response status: {}", response.status());
    println!();

    let (mut write, mut read) = ws_stream.split();

    // Use the correct RTDS subscription format: action + subscriptions array
    let subscriptions = vec![
        // Baseline: crypto_prices (documented, known to work)
        json!({
            "action": "subscribe",
            "subscriptions": [{
                "topic": "crypto_prices",
                "type": "update",
                "filters": "btcusdt"
            }]
        }),
        // Try activity/trades — likely non-existent but worth testing
        json!({
            "action": "subscribe",
            "subscriptions": [{
                "topic": "activity",
                "type": "trades"
            }]
        }),
        // Try plain trades topic
        json!({
            "action": "subscribe",
            "subscriptions": [{
                "topic": "trades",
                "type": "update"
            }]
        }),
    ];

    println!("--- Sending subscriptions ---");
    for sub in &subscriptions {
        println!("  Sending: {}", sub);
        write.send(Message::Text(sub.to_string().into())).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!();

    // Listen for messages for 30 seconds, sending PING every 5s
    println!("--- Listening for 30 seconds ---");
    let start = Instant::now();
    let timeout = Duration::from_secs(30);
    let mut msg_count = 0;
    let mut topics_seen = std::collections::HashSet::new();
    let mut last_ping = Instant::now();

    loop {
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            break;
        }

        // Send keepalive PING every 5 seconds
        if last_ping.elapsed() >= Duration::from_secs(5) {
            let _ = write.send(Message::Ping(vec![].into())).await;
            last_ping = Instant::now();
        }

        match tokio::time::timeout(Duration::from_secs(1), read.next()).await {
            Ok(Some(Ok(msg))) => {
                msg_count += 1;
                let elapsed = start.elapsed();
                match &msg {
                    Message::Text(text) => {
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text.as_str()) {
                            let topic = parsed
                                .get("topic")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            let msg_type = parsed
                                .get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            topics_seen.insert(format!("{}/{}", topic, msg_type));

                            // Print first 15 messages in full, then summaries
                            if msg_count <= 15 {
                                println!(
                                    "[{:.1}s] #{} topic={} type={}: {}",
                                    elapsed.as_secs_f64(),
                                    msg_count,
                                    topic,
                                    msg_type,
                                    if text.len() > 300 {
                                        format!("{}...", &text[..300])
                                    } else {
                                        text.to_string()
                                    }
                                );
                            } else if msg_count % 20 == 0 {
                                println!(
                                    "[{:.1}s] #{} ({} msgs so far)",
                                    elapsed.as_secs_f64(),
                                    msg_count,
                                    msg_count,
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
                    Message::Pong(_) => {} // ignore pong responses
                    Message::Ping(_) => {
                        println!("[{:.1}s] Ping received", elapsed.as_secs_f64());
                    }
                    Message::Close(frame) => {
                        println!("[{:.1}s] Close frame: {:?}", elapsed.as_secs_f64(), frame);
                        break;
                    }
                    other => {
                        println!("[{:.1}s] Other: {:?}", elapsed.as_secs_f64(), other);
                    }
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
            Err(_) => continue, // timeout on read, loop back to check duration
        }
    }

    println!();
    println!("--- Summary ---");
    println!("Total messages received: {}", msg_count);
    println!("Duration: {:.1}s", start.elapsed().as_secs_f64());
    println!("Topics/types seen: {:?}", topics_seen);
    println!();
    println!("Note: RTDS only documents crypto_prices, crypto_prices_chainlink,");
    println!("and comments topics. There is NO documented activity/trades topic.");
    println!("For trade monitoring, REST polling or CLOB WS market channel is needed.");
    println!();

    println!("=== Probe 1C Complete ===");
    Ok(())
}
