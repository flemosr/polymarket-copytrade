# Phase 1: Exploration Findings

**Date:** 2026-02-10
**Test trader:** randomly picked from the Polymarket leaderboard (high-volume sports bettor)

## Trader Identity: Proxy Wallets

Polymarket users have two addresses ([docs](https://docs.polymarket.com/developers/proxy-wallet)):

| Component | Description | Visible? |
|-----------|-------------|----------|
| **EOA** | User's actual private key wallet (MetaMask, MagicLink) | Not publicly shown |
| **Proxy Wallet** | Deterministic smart contract on Polygon, controlled by the EOA | Shown on profile, used in all API responses |

The proxy wallet holds all USDC and position tokens. It is deployed via CREATE2, so one EOA always maps to one proxy wallet. The address on a trader's profile page IS their `proxyWallet`.

- The Data API `user=` parameter matches against `proxyWallet` ([trades docs](https://docs.polymarket.com/api-reference/core/get-trades-for-a-user-or-markets))
- RTDS trade payloads include `proxyWallet` — client-side filtering by `proxyWallet == target_address` is correct
- A single trader has exactly one proxy wallet per Polymarket account
- A person *could* operate multiple accounts (different EOAs = different proxy wallets) — we'd need to track each separately

Two proxy wallet types exist depending on login method:
1. **Gnosis Safe** (MetaMask/browser wallet) — factory `0xaacfeea03eb1561c4e67d661e40682bd20e3541b`
2. **Polymarket Custom Proxy** (MagicLink/email) — factory `0xaB45c5A4B0c941a2F231C04C3f49182e1A254052`

---

## 1A — REST Trades Endpoint

**Endpoint:** `GET https://data-api.polymarket.com/trades?user=<addr>`

### Response Shape

Each trade is an object with these fields:

| Field | Type | Example |
|-------|------|---------|
| `asset` | string | `"82084099493722841326424854053682115622937305216054831588899774334225620550718"` |
| `conditionId` | string | `"0x589b74a8c97aebaf0a6edd9849ea933cac8130ccfc992395b34e171476463b5c"` |
| `eventSlug` | string | `"nba-mem-gsw-2026-02-09"` |
| `outcome` | string | `"Grizzlies"` |
| `outcomeIndex` | number | `0` |
| `price` | number | `0.24` |
| `side` | string | `"BUY"` |
| `size` | number | `40890.77` |
| `timestamp` | number | `1770690301` (unix epoch) |
| `title` | string | `"Grizzlies vs. Warriors"` |
| `transactionHash` | string | `"0xed417440f2fd0d412d7e94ecb22cf19d3e4c7ceed312cc3f2645acc5dd44efc4"` |
| `proxyWallet` | string | `"0xdb27bf2ac5d428a9c63dbc914611036855a6c56e"` |
| `name` | string | `"DrPufferfish"` |
| `slug` | string | `"nba-mem-gsw-2026-02-09"` |
| `icon` | string | URL to market icon |
| `bio` | string | empty |
| `pseudonym` | string | `"Extraneous-Twine"` |
| `profileImage` | string | empty |
| `profileImageOptimized` | string | empty |

**No `id` field** — dedup must use `transactionHash`.

### Pagination

- `limit` and `offset` parameters work correctly
- Default returns 100 trades
- `limit=5` returns exactly 5
- `offset=5` returns the next page

### Filtering

- `side=BUY` filter works — returns only BUY trades
- Confirmed all returned trades have `side=BUY`

### Latency

| Request | Latency |
|---------|---------|
| 1 (cold) | 407ms |
| 2 | 56ms |
| 3 | 48ms |
| 4 | 47ms |
| 5 | 46ms |
| **Average** | **121ms** |

Cold start is ~400ms, subsequent requests ~50ms (connection reuse).

### Dedup Strategy

- **`transactionHash` is unique** across 100 tested trades (0 duplicates, 0 missing)
- Recommended dedup key: `transactionHash`
- No `id` field exists

---

## 1B — REST Positions Endpoint

**Endpoint:** `GET https://data-api.polymarket.com/positions?user=<addr>`

### Response Shape

Each position is an object with these fields:

| Field | Type | Example | Present |
|-------|------|---------|---------|
| `asset` | string | token ID | Yes |
| `conditionId` | string | `0x307d...` | Yes |
| `title` | string | `"Thunder vs. Spurs"` | Yes |
| `outcome` | string | `"Thunder"` | Yes |
| `outcomeIndex` | number | `0` | Yes |
| `size` | number | `1303042.778` (shares) | Yes |
| `avgPrice` | number | `0.2811` | Yes |
| `currentValue` | number | `0` (or positive) | Yes |
| `curPrice` | number | `0` (or positive) | Yes |
| `cashPnl` | number | `-366346.5679` | Yes |
| `percentPnl` | number | `-99.9999` | Yes |
| `initialValue` | number | `366346.5679` | Yes |
| `realizedPnl` | number | `0` | Yes |
| `percentRealizedPnl` | number | `-100` | Yes |
| `proxyWallet` | string | address | Yes |
| `eventSlug` | string | slug | Yes |
| `endDate` | string | `"2026-02-05"` | Yes |
| `mergeable` | bool | `false` | Yes |
| `negativeRisk` | bool | `false` | Yes |
| `redeemable` | bool | `true` | Yes |
| `oppositeAsset` | string | token ID | Yes |
| `oppositeOutcome` | string | outcome name | Yes |
| `totalBought` | number | shares bought | Yes |
| `marketSlug` | - | - | **MISSING** |

**Note:** `marketSlug` is not returned; use `slug` or `eventSlug` instead.

### Pagination

- Default returns 100 positions
- DrPufferfish has **535 total positions** (paginated: 100+100+100+100+100+35)
- `limit` and `offset` work correctly

### Active Positions & Portfolio Weights

**Active positions:** 30 (of 535 total, filtered by `currentValue > 0`)
**Total portfolio value:** $1,569,781.16

**Portfolio weight formula:** `weight = position.currentValue / totalPortfolioValue`

Top positions by weight:

| Market | Value ($) | Weight% | Outcome | CurPrice |
|--------|-----------|---------|---------|----------|
| Cavaliers vs. Nuggets | 863,514.51 | 55.01% | Cavaliers | 1.0000 |
| Spread: Warriors (-9.5) | 250,695.23 | 15.97% | Grizzlies | 1.0000 |
| Thunder vs. Lakers | 138,647.91 | 8.83% | Thunder | 1.0000 |
| Spread: Thunder (-6.5) | 87,669.07 | 5.58% | Thunder | 1.0000 |
| Bulls vs. Nets | 38,406.38 | 2.45% | Nets | 1.0000 |
| Boston Celtics NBA Finals | 25,177.68 | 1.60% | Yes | 0.0580 |
| Cleveland Cavaliers NBA Finals | 25,177.11 | 1.60% | Yes | 0.0635 |
| Spurs vs. Lakers | 23,996.40 | 1.53% | Lakers | 0.2550 |

**Observation:** Many resolved positions (`curPrice=1.0`) are still showing as active because they have unredeemed shares. For copytrade weight computation, we should consider filtering by `curPrice < 1.0 && curPrice > 0.0` to only mirror positions in active (unresolved) markets.

---

## 1C — RTDS WebSocket

**URL:** `wss://ws-live-data.polymarket.com`

### Subscription Format

Correct format (not on docs site, but used in [official SDK](https://github.com/Polymarket/real-time-data-client)):
```json
{
  "action": "subscribe",
  "subscriptions": [{
    "topic": "activity",
    "type": "trades"
  }]
}
```

### Key Finding: Activity/Trades Topic — Officially Supported but Poorly Documented

The official docs at `docs.polymarket.com` only list 3 of 8 available RTDS topics. The `activity`/`trades` channel is **officially supported** — it is used in the [official TypeScript SDK](https://github.com/Polymarket/real-time-data-client) example code and implemented by community libraries in [Rust](https://lib.rs/crates/polymarket-rtds), [Go](https://pkg.go.dev/github.com/Matthew17-21/go-polymarket-real-time-data-client), and JS.

Full RTDS topic list (from official SDK):

| Topic | Types | Auth |
|-------|-------|------|
| **`activity`** | **`trades`, `orders_matched`** | No |
| `comments` | `comment_created/removed`, `reaction_created/removed` | No |
| `rfq` | `request_created/edited/canceled/expired`, `quote_created/edited/canceled/expired` | No |
| `crypto_prices` | `update` | No |
| `crypto_prices_chainlink` | `update` | No |
| `equity_prices` | `update` | No |
| `clob_market` | `agg_orderbook`, `price_change`, `last_trade_price`, etc. | No |
| `clob_user` | `order`, `trade` | Yes |

The `activity`/`trades` topic delivers a real-time firehose of ALL platform trades:

- **2,001 messages in 30 seconds** (~67 trades/sec)
- Each message includes the same fields as the REST `/trades` endpoint: `asset`, `conditionId`, `side`, `size`, `price`, `proxyWallet`, `outcome`, `title`, etc.
- Messages are wrapped in: `{ "topic": "activity", "type": "trades", "connection_id": "...", "payload": { ...trade... } }`
- Trades include `proxyWallet` — **we can filter for our target trader client-side**
- Supports `market_slug` and `event_slug` filters, but **not** `proxyWallet` filtering server-side

### Sample RTDS Trade Message Payload

Same shape as REST trades response, embedded in `payload`:
```json
{
  "topic": "activity",
  "type": "trades",
  "connection_id": "...",
  "payload": {
    "asset": "...",
    "conditionId": "...",
    "eventSlug": "...",
    "outcome": "...",
    "price": 0.24,
    "side": "BUY",
    "size": 40890.77,
    "title": "...",
    "transactionHash": "0x...",
    "proxyWallet": "0x..."
  }
}
```

### Other Subscriptions Tested

| Subscription | Result |
|-------------|--------|
| `crypto_prices` / `update` (filters: `"btcusdt"`) | Error: filters require JSON format, not plain string |
| `trades` / `update` | Error: "topic: trades and type: update not found" |
| `activity` / `trades` | **WORKS** — firehose of all platform trades |

### Server-Side Filtering

The `event_slug` filter was accepted without error when passed as a JSON string:
```json
{
  "action": "subscribe",
  "subscriptions": [{
    "topic": "activity",
    "type": "trades",
    "filters": "{\"event_slug\":\"bitcoin-up-or-down-february-10-12pm-et\"}"
  }]
}
```
Supports `market_slug` and `event_slug` filters, but **not** `proxyWallet` — trader filtering must be done client-side.

### Binary Market Trade Duplication

A single trade on a binary market produces **two RTDS messages** with the same `transactionHash` — one for each side of the trade (e.g., BUY/Up and BUY/Down). Each message has a different `proxyWallet` (buyer vs. seller) and `outcome`. When deduplicating, `transactionHash` alone is not unique per message — use `transactionHash` + `proxyWallet` or `transactionHash` + `outcome` as the composite key.

### Latency

- Messages arrive in bulk batches (~20 trades per burst, bursts every ~0.3s)
- Real-time latency appears to be **sub-second** from trade execution

### Known Reliability Issue

[GitHub issue #26](https://github.com/Polymarket/real-time-data-client/issues/26) on the official SDK repo reports that the `activity` stream **silently stops delivering messages after ~18-22 minutes** while the WebSocket connection (ping/pong) remains healthy. The stream receives ~14-30 messages/second before silently dying.

**Implication for Phase 2:** We need a heartbeat/watchdog mechanism to detect when the stream goes silent and automatically reconnect. Cannot rely solely on WebSocket connection state.

### Existing Rust Crate

The [`polymarket-rtds`](https://lib.rs/crates/polymarket-rtds) crate by Rimantovas wraps the RTDS WebSocket with typed `Topic::Activity` / `MessageType::Trades` enums. Worth evaluating for Phase 2 instead of raw `tokio-tungstenite`.

---

## 1D — CLOB WebSocket

**URL:** `wss://ws-subscriptions-clob.polymarket.com/ws/market`

### Connection

- Connected successfully to `/ws/market` endpoint
- **Important:** URL must include `/market` suffix (bare `/ws/` returns 404)
- Uses `assets_ids` (plural, array of token IDs) — NOT condition IDs

### Subscription Format

```json
{
  "type": "market",
  "assets_ids": ["TOKEN_ID"],
  "custom_feature_enabled": true
}
```

### Observations (Resolved Market)

- Received 3 messages in 30 seconds on a resolved market (Cavaliers vs. Nuggets, curPrice=1.0)
- Minimal activity as expected for a settled market

### Observations (Active Market — BTC Up/Down Feb 10)

Tested on `bitcoin-up-or-down-february-10-12pm-et` (condition `0x1bb4acb9...`), subscribing to both Up and Down token IDs simultaneously. Results over 30 seconds:

| Event Type | Count | Description |
|------------|-------|-------------|
| `price_change` | 805 | Every orderbook change (new order placed/cancelled) |
| `best_bid_ask` | 24 | Best bid/ask updates |
| `book` | 14 | Full orderbook snapshots (~96 bids, 3 asks) |
| `last_trade_price` | 7 | Actual matched trades (prices 0.95–0.97 for Up) |

- **No trader identity fields** in any event type (confirmed: no maker/taker/user/proxyWallet)
- Event types available: `book`, `price_change`, `last_trade_price`, `tick_size_change`, `best_bid_ask`, `new_market`, `market_resolved`
- Multiple token IDs can be subscribed on a single connection via the `assets_ids` array

### Keepalive

Send text `"PING"` every 10 seconds (not a WebSocket ping frame). Server responds with `"PONG"`.

### User Channel (Not Tested)

The `/ws/user` channel requires the trader's own API credentials (`apiKey`, `secret`, `passphrase`). Not useful for monitoring someone else's trades.

---

## Recommendations

### Phase 2: REST Polling (Chosen Approach)

For portfolio mirroring, detection latency barely matters — when we detect a trade, we re-fetch `/positions` and rebalance, not copy the individual trade. A 5–10 second polling delay doesn't meaningfully affect fill price on prediction markets.

**Trade Detection:**
- Poll `GET /trades?user=<addr>&limit=5` every N seconds
- Dedup by `transactionHash` via `HashSet<String>`
- On new trade detected → re-fetch positions → recompute weights → rebalance
- Simple, reliable, no WebSocket complexity

**Portfolio Snapshots:**
- `GET /positions?user=<addr>` with pagination (limit=100, offset)
- On startup: full snapshot to compute initial portfolio weights
- After each detected trade: re-snapshot for updated weights

### Phase 5: RTDS WebSocket (Future Upgrade)

REST polling doesn't scale when monitoring many target traders. RTDS provides:
- Sub-second detection via the `activity`/`trades` firehose (~67 msgs/sec)
- Client-side `proxyWallet` filtering for all monitored traders on a single connection
- But requires watchdog/reconnect for the ~20-minute silent death bug
- Hybrid mode recommended: RTDS primary + REST polling fallback

### Portfolio Weight Computation

```
active_positions = positions.filter(|p| p.currentValue > 0 && p.curPrice < 1.0 && p.curPrice > 0.0)
total_value = sum(active_positions.map(|p| p.currentValue))
weight(p) = p.currentValue / total_value
target(p) = min(weight(p) * budget * copy_percentage, max_trade_size)
```

**Important filter:** Exclude positions with `curPrice == 1.0` (resolved/won) and `curPrice == 0.0` (resolved/lost) to only mirror active market positions.

### Dedup Strategy

- Use `transactionHash` as the unique identifier for trades
- 100% unique across tested sample (no duplicates, no missing)
- Store in a `HashSet<String>` for O(1) lookup
- Note: on RTDS, binary market trades produce two messages per `transactionHash` (one per side) — use `transactionHash` + `proxyWallet` as composite key if needed

### Architecture for Phase 2

```
┌──────────────────┐
│  REST /trades    │──── poll every N sec ────┐
│  (user=<addr>)   │                          │
└──────────────────┘                          ▼
                                    ┌────────────────────┐
                                    │  Trade Detector    │
                                    │  (dedup by txHash) │
                                    └────────┬───────────┘
                                             │ new trade detected
                                             ▼
                                    ┌──────────────────┐
                                    │  REST /positions │
                                    │  (re-snapshot)   │
                                    └────────┬─────────┘
                                             │ updated weights
                                             ▼
                                    ┌───────────────────┐
                                    │  Rebalance Engine │
                                    │  (diff + orders)  │
                                    └───────────────────┘
```

### Data Source Not Useful for Copytrading

- **CLOB WS Market channel**: No trader identity in market events. Only useful for order book data.
- **CLOB WS User channel**: Requires the trader's own credentials. Cannot monitor third parties.
- **RTDS `crypto_prices`**: Only crypto price feeds, not relevant.

---

## API Reference Quick Summary

| Source | URL | Auth | Use Case |
|--------|-----|------|----------|
| REST Trades | `data-api.polymarket.com/trades?user=<addr>` | None | Backfill, verification |
| REST Positions | `data-api.polymarket.com/positions?user=<addr>` | None | Portfolio snapshot |
| RTDS WS | `ws-live-data.polymarket.com` | None | Real-time trade detection |
| CLOB WS Market | `ws-subscriptions-clob.polymarket.com/ws/market` | None | Order book data (no identity) |
| CLOB WS User | `ws-subscriptions-clob.polymarket.com/ws/user` | API Key | Own trades only |
