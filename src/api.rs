use anyhow::Result;
use polymarket_client_sdk::data::Client;
use polymarket_client_sdk::data::types::request::{PositionsRequest, TradesRequest};
use polymarket_client_sdk::data::types::response::{Position, Trade};
use polymarket_client_sdk::types::Address;
use rust_decimal::Decimal;
use tracing::debug;

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
