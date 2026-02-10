# Polymarket Copytrading Bot — Implementation Plan

## Overview

Build a CLI copytrading bot for Polymarket that mirrors a target trader's portfolio. Supports dry-run simulation and live execution via the CLOB API.

**Language:** Rust

### Design Decisions

**Portfolio mirroring vs trade-by-trade copying**

We evaluated two approaches:

- **Trade-by-trade** — detect each individual trade and copy it proportionally. copy-percentage scales each trade relative to the trader's size. Simple for buy-only, but sell mirroring becomes complex: we must track which positions we observed being built, handle pre-existing positions we never copied, and compute exit fractions from partial knowledge of the trader's portfolio.

- **Portfolio mirroring** — on startup, snapshot the trader's current portfolio and replicate it proportionally. Then detect new trades and apply them to stay aligned. copy-percentage represents the fraction of our budget allocated to replicating this trader.

We chose **portfolio mirroring** because full alignment ensures our returns match the trader's returns (proportionally) from the moment we start copying — this is the actual goal of copytrading. Trade-by-trade is initially simpler for buy-only, but portfolio mirroring is more suitable when considering the full feature set (buys + sells + position tracking).

### Key Concepts

- **copy-percentage** — proportion of our budget allocated to replicating the target trader's portfolio. If the trader has 40% of their portfolio in market X, we allocate 40% of `budget × copy_percentage` to market X. This ensures proportional alignment with the trader's conviction across all positions.
- **max-trade-size** — hard cap on the amount allocated to any single market position (in USD). In our portfolio mirroring approach, each "trade" is a position adjustment to stay aligned with the target trader — max-trade-size caps the total size of that position, not individual orders.
- **budget** — total capital allocated to copytrading. Sell proceeds flow back into the budget.

---

## Resources

- [Polymarket Developer Quickstart](https://docs.polymarket.com/quickstart/overview)
- [CLOB API Documentation](https://docs.polymarket.com/developers/CLOB/introduction)
- [CLOB WebSocket Overview](https://docs.polymarket.com/developers/CLOB/websocket/wss-overview)
- [RTDS Overview](https://docs.polymarket.com/developers/RTDS/RTDS-overview) (incomplete — only 3 of 8 topics documented; see `EXPLORATION.md`)
- [Proxy Wallet Documentation](https://docs.polymarket.com/developers/proxy-wallet)
- [Get trades for a user or markets](https://docs.polymarket.com/api-reference/core/get-trades-for-a-user-or-markets)
- [Get current positions for a user](https://docs.polymarket.com/api-reference/core/get-current-positions-for-a-user)
- **Gamma API:** `gamma-api.polymarket.com` — market metadata and pricing by token ID, condition ID, or slug. Used for exit pricing (resolved/exited positions). SDK: `polymarket-client-sdk` `gamma` feature → `gamma::Client`
- **Rust SDK:** [Polymarket/rs-clob-client](https://github.com/Polymarket/rs-clob-client) — `polymarket-client-sdk` on [crates.io](https://crates.io/crates/polymarket-client-sdk) / [docs.rs](https://docs.rs/polymarket-client-sdk)
- **RTDS TypeScript SDK:** [Polymarket/real-time-data-client](https://github.com/Polymarket/real-time-data-client) — authoritative source for all 8 RTDS topics
- **Leaderboard API:** `GET https://data-api.polymarket.com/v1/leaderboard?limit=15&orderBy=vol&timePeriod=day` — find active traders by volume

---

## Phase 1: Exploration

Before committing to a detailed architecture, we need to validate our assumptions about the available data sources. We'll write small standalone programs to probe the APIs.

### 1A — REST Polling

Probe `GET https://data-api.polymarket.com/trades?user=<addr>` with a known active trader address.

Goals:
-Confirm the endpoint works and returns trade data
-Document the actual response shape (fields, types, nesting)
-Test pagination (`limit`, `offset`) and filtering (`side=BUY`)
-Measure latency between a trade occurring and it appearing in the REST API
-Determine a reliable deduplication strategy (timestamp? transactionHash?)

### 1B — Positions Endpoint

Probe `GET https://data-api.polymarket.com/positions?user=<addr>` to fetch a trader's current portfolio.

Goals:
- Confirm the endpoint returns current positions with size, avgPrice, currentValue
- Document the response shape and available fields
- Test filtering and pagination parameters
- Determine how to compute portfolio weights from the response

### 1C — WebSocket (RTDS)

Connect to `wss://ws-live-data.polymarket.com` and subscribe to `topic: "activity"`, `type: "trades"`.

Goals:
- Confirm the RTDS activity/trades channel is accessible
- Document the correct subscription format and available topics
- Document the actual message shape and compare to REST response
- Determine if we can filter by `proxyWallet` server-side or must filter client-side
- Measure real-time latency (time from trade execution to WS message arrival)

### 1D — CLOB WebSocket

Connect to `wss://ws-subscriptions-clob.polymarket.com/ws/market` and subscribe to an active market.

Goals:
- Document the subscription format and available event types
- Confirm whether market data includes trader identity (maker/taker/proxyWallet)
- Document findings

### Exploration Deliverable

Results will be documented in `EXPLORATION.md` with:
- What data is available from each source
- Actual response/message samples
- Latency characteristics
- Recommended primary data source for trade detection

---

## Phase 2: Core Dry-Run

Based on exploration results, implement the core copytrading simulation. Uses `polymarket-client-sdk` with `data` + `gamma` features — `data` for typed REST access (positions, trades), `gamma` for market price lookups on exit. Single crate layout (not a workspace).

- Project scaffolding (single crate, dependencies, CLI with `clap`)
- Portfolio snapshot — fetch trader's current positions via SDK `client.positions()`, compute portfolio weights. Active position filter: `current_value > 0 && 0 < cur_price < 1` — this excludes resolved markets (price at 0 or 1), fully-exited positions (value = 0), and unredeemed settled shares.
- Target state computation — for each market the trader holds, compute `target = min(trader_weight × budget × copy_percentage, max_trade_size)`. Targets always use the full original budget, not the remaining budget — this ensures proportional alignment even after spending.
- Initial replication — diff target state against our current holdings (initially empty), generate buy orders to align
- Trade detection — REST polling via SDK `client.trades()` with `transaction_hash.to_string()` dedup (B256 → String in a HashSet) as a trigger to recompute
- Rebalancing — on each detected trade, recompute target state from the trader's updated positions, diff against our current holdings. Process sells first (freeing budget), then buys (consuming budget). Buys are capped by available budget with partial fill support. Orders below $0.01 are skipped.
- Trading state — track holdings, remaining budget, cumulative spend, realized P&L; sell proceeds flow back into budget
- Structured reporting — JSON event lines to stdout (one per rebalancing cycle) + exit summary (pretty JSON with holdings, P&L, totals). Tracing logs to stderr. Configurable via `POLL_INTERVAL_SECS` env var (default 10s, set in `.env`).
- Exit pricing — when a held position leaves the active target set (trader exits or market resolves), the engine resolves its price via a two-layer lookup: (1) active positions from the data API, (2) gamma API (`markets?clob_token_ids=<id>`) for assets not found in layer 1. This covers resolved markets (price 0 or 1) and voluntary exits where the position disappears from the filtered response. Gamma errors propagate — no silent fallbacks. Exit events are logged with reason (`resolved` vs `trader exited`) and a short trader ID (last 6 chars of address) for future multi-trader support.
- Graceful shutdown on Ctrl+C — fetches latest prices (with gamma enrichment for missing assets) and reports exit summary with unrealized P&L

### CLI Target

```bash
copytrade --dry-run \
  --trader-address <proxy-wallet-address> \
  --budget <usd-amount> \
  --copy-percentage <0-100> \
  --max-trade-size <usd-amount>
```

---

## Phase 3: Live Execution

Extend the bot to execute real trades via the CLOB API. Phase 2 already uses the SDK's `data` feature for read-only access; Phase 3 adds the `clob` feature for authenticated order placement (signing, API keys, order execution).

- Account setup command (private key, API credential derivation)
- Order execution via `polymarket-client-sdk` CLOB client
- Order status tracking and logging
- Retry with exponential backoff for order placement and API calls

### CLI Target

```bash
copytrade setup-account --private-key <key>

copytrade --live \
  --trader-address <polygon-address> \
  --budget <usd-amount> \
  --copy-percentage <0-100> \
  --max-trade-size <usd-amount>
```

---

## Phase 4: Persistent Storage

Session state and copytrade records, enabling resume across restarts:

- Processed transaction hashes (deduplication on restart)
- Remaining budget and cumulative spend
- Copytrade decisions (computed size, cost, skip reason, order status)
- User configuration (trader addresses, budget, percentages)

---

## Phase 5: WebSocket Trade Detection (RTDS)

Upgrade trade detection from REST polling to RTDS WebSocket for lower latency and better scalability with multiple traders.

Phase 2 testing showed REST polling at 5s intervals is already effective for bursty traders (crypto bots trade in concentrated bursts at 15-min boundaries). WebSocket is primarily valuable for: (a) latency-sensitive copying of human traders who make sporadic single trades, and (b) scaling to many traders without proportional API load.

- Subscribe to RTDS `activity`/`trades` firehose (all platform trades in real-time)
- Client-side filtering by `proxyWallet` to isolate target trader(s)
- Watchdog/reconnect logic to handle the known ~20-minute silent stream death bug
- Hybrid mode: RTDS as primary trigger, REST polling as fallback safety net
- This becomes essential when monitoring many traders (REST polling doesn't scale)

See `EXPLORATION.md` for RTDS findings, message format, and reliability notes.

---

## Phase 6: Multi-Account Copytrading

- Support monitoring multiple trader addresses simultaneously
- Per-trader budget and configuration
- Aggregated reporting across all tracked traders

---

## Phase 7: Documentation and Final Tests

- README with setup/run instructions
- Config examples (no real keys)
- Final testing with a real active trader
