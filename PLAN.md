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
- [RTDS Overview](https://docs.polymarket.com/developers/RTDS/RTDS-overview)
- [Get trades for a user or markets](https://docs.polymarket.com/api-reference/core/get-trades-for-a-user-or-markets)
- [Get current positions for a user](https://docs.polymarket.com/api-reference/core/get-current-positions-for-a-user)
- **Rust SDK:** [Polymarket/rs-clob-client](https://github.com/Polymarket/rs-clob-client) — `polymarket-client-sdk` on [crates.io](https://crates.io/crates/polymarket-client-sdk) / [docs.rs](https://docs.rs/polymarket-client-sdk)

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
-Confirm the RTDS activity/trades channel is accessible
-Test using the official SDK's `subscribe_raw("activity", "trades")` method
-Document the actual message shape and compare to REST response
-Determine if we can filter by `proxyWallet` server-side or must filter client-side
-Measure real-time latency (time from trade execution to WS message arrival)

### 1D — CLOB WebSocket

Explore the CLOB WS channels via the SDK (`ws` feature flag).

Goals:
-Test `subscribe_last_trade_price` on the market channel — confirm it lacks user identity
-Test if there's any other channel that exposes trader identity for arbitrary users
-Document findings

### Exploration Deliverable

Results will be documented in `EXPLORATION.md` with:
- What data is available from each source
- Actual response/message samples
- Latency characteristics
- Recommended primary data source for trade detection

---

## Phase 2: Core Dry-Run

Based on exploration results, implement the core copytrading simulation:

- Project scaffolding (Cargo workspace, dependencies, CLI with `clap`)
- Portfolio snapshot — fetch trader's current positions via `/positions`, compute portfolio weights
- Target state computation — for each market the trader holds, compute `target = min(trader_weight × budget × copy_percentage, max_trade_size)`. This is deterministic and fully derived from the trader's current portfolio.
- Initial replication — diff target state against our current holdings (initially empty), generate buy orders to align
- Trade detection — real-time monitoring of the target trader's activity as a trigger to recompute
- Rebalancing — on each detected trade, recompute target state from the trader's updated positions, diff against our current holdings, generate orders (buy or sell) to close the gap
- Trading state — track our holdings, remaining budget, cumulative spend; skip orders when budget is exhausted
- Structured reporting — per-trade log (detected trade, computed copytrade, running budget) + exit summary (total trades, total spend, P&L). Format as JSON, printed table, or log file.
- Graceful shutdown on Ctrl+C

### CLI Target

```bash
copytrade --dry-run \
  --trader-address <polygon-address> \
  --budget <usd-amount> \
  --copy-percentage <0-100> \
  --max-trade-size <usd-amount>
```

---

## Phase 3: Live Execution

Extend the bot to execute real trades via the CLOB API:

- Account setup command (private key, API credential derivation)
- Order execution via `polymarket-client-sdk`
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

## Phase 5: Multi-Account Copytrading

- Support monitoring multiple trader addresses simultaneously
- Per-trader budget and configuration
- Aggregated reporting across all tracked traders

---

## Phase 6: Documentation and Final Tests

- README with setup/run instructions
- Config examples (no real keys)
- Final testing with a real active trader
