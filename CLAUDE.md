# Polymarket Copytrading Bot

## Project Overview

Rust CLI bot that mirrors a target trader's Polymarket portfolio. Uses portfolio mirroring (not trade-by-trade copying) to proportionally replicate positions. Supports dry-run simulation and live execution via the CLOB API.

See `PLAN.md` for the full implementation plan — consult it for detailed goals, design rationale, API endpoints, CLI targets, and phase-by-phase deliverables.

## Key Concepts

- **copy-percentage** — fraction of budget allocated to replicating the trader's portfolio
- **max-trade-size** — hard cap per market position (USD)
- **budget** — total capital; sell proceeds flow back in

## Architecture

- **Language:** Rust
- **CLI:** `clap`
- **SDK:** `polymarket-client-sdk` (rs-clob-client)
- **Data sources:** REST polling (`data-api.polymarket.com`); RTDS WebSocket planned for Phase 5

## Plan Progress

### Phase 1: Exploration
- [x] 1A — REST Polling (trades endpoint)
- [x] 1B — Positions Endpoint
- [x] 1C — WebSocket (RTDS)
- [x] 1D — CLOB WebSocket
- [x] Write EXPLORATION.md with findings

### Phase 2: Core Dry-Run
- [ ] Project scaffolding (Cargo workspace, deps, CLI)
- [ ] Portfolio snapshot and weight computation
- [ ] Target state computation
- [ ] Initial replication (diff + buy orders)
- [ ] Trade detection (REST polling)
- [ ] Rebalancing logic
- [ ] Trading state tracking (holdings, budget, spend)
- [ ] Structured reporting (JSON/table/log)
- [ ] Graceful shutdown (Ctrl+C)

### Phase 3: Live Execution
- [ ] Account setup command
- [ ] Order execution via SDK
- [ ] Order status tracking
- [ ] Retry with exponential backoff

### Phase 4: Persistent Storage
- [ ] Transaction dedup on restart
- [ ] Budget/spend persistence
- [ ] Copytrade decision records
- [ ] User configuration storage

### Phase 5: WebSocket Trade Detection (RTDS)
- [ ] RTDS activity/trades subscription
- [ ] Client-side proxyWallet filtering
- [ ] Watchdog/reconnect for silent stream death (~20min bug)
- [ ] Hybrid mode: RTDS primary + REST polling fallback

### Phase 6: Multi-Account Copytrading
- [ ] Multiple trader addresses
- [ ] Per-trader budget/config
- [ ] Aggregated reporting

### Phase 7: Documentation and Final Tests
- [ ] README with setup/run instructions
- [ ] Config examples
- [ ] Final testing with real trader

## Conventions

- Exploration probes go in standalone files/binaries before being integrated
- Results and API findings documented in `EXPLORATION.md`
- All secrets/keys kept out of version control
