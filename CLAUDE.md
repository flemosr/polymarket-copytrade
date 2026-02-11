# Polymarket Copytrading Bot

## Project Overview

Rust CLI bot that mirrors a target trader's Polymarket portfolio. Uses portfolio mirroring (not trade-by-trade copying) to proportionally replicate positions. Supports dry-run simulation and live execution via the CLOB API.

See `PLAN.md` for the full implementation plan — consult it for detailed goals, design rationale, API endpoints, CLI targets, and phase-by-phase deliverables.

## Key Concepts

- **copy-percentage** — fraction of budget allocated to replicating the trader's portfolio
- **max-trade-size** — max percentage of running budget per market position (0–100)
- **budget** — initial capital; running budget is `budget_remaining + holdings value`, floats with P&L

## Architecture

- **Language:** Rust
- **CLI:** `clap`
- **SDK:** `polymarket-client-sdk` v0.3 with `data` + `gamma` features; CLOB client (always available, no feature flag); `k256` for `PrivateKeySigner` type
- **Data sources:** REST polling (`data-api.polymarket.com`), gamma API for exit pricing (`gamma-api.polymarket.com`), CLOB API for live order execution (`clob.polymarket.com`); RTDS WebSocket planned for Phase 5
- **Output:** JSON events to stdout, tracing logs to stderr
- **Config:** `config.toml` (TOML) for private key + poll interval; copytrade params via CLI args; `RUST_LOG` via env

### Module Structure

| Module | Purpose |
|--------|---------|
| `src/config.rs` | Config loading/saving (`AppConfig`, `AccountConfig`, `SettingsConfig`) |
| `src/types.rs` | Domain types (`MarketPosition`, `TargetAllocation`, `SimulatedOrder`, `HeldPosition`, `RestingOrder`, `CopytradeEvent`, `ExitSummary`, `ExecutionResult`, `ExecutionStatus`) |
| `src/api.rs` | SDK wrappers (`fetch_active_positions`, `fetch_recent_trades`, `fetch_gamma_prices`, `build_exit_price_map`) |
| `src/engine.rs` | Portfolio math (`compute_weights`, `compute_target_state`, `compute_orders`) |
| `src/state.rs` | `TradingState` — holdings, budget, P&L tracking, resting order tracking, `effective_held_shares()` |
| `src/auth.rs` | CLOB authentication (`ClobContext`, `authenticate()`) |
| `src/executor.rs` | Live order execution (`execute_orders`, `check_resting_orders`, retry, balance guard) |
| `src/reporter.rs` | JSON output (event lines + pretty exit summary) |
| `src/bin/copytrade.rs` | Main binary — CLI, initial replication, polling loop, shutdown |
| `src/bin/setup_account.rs` | First-time setup — validate auth, print account info, update private key in `config.toml` |
| `src/bin/probe_*.rs` | Exploration probes (Phase 1 + Phase 3) |

## Plan Progress

### Phase 1: Exploration
- [x] 1A — REST Polling (trades endpoint)
- [x] 1B — Positions Endpoint
- [x] 1C — WebSocket (RTDS)
- [x] 1D — CLOB WebSocket
- [x] Write EXPLORATION.md with findings

### Phase 2: Core Dry-Run
- [x] Project scaffolding (Cargo workspace, deps, CLI)
- [x] Portfolio snapshot and weight computation
- [x] Target state computation
- [x] Initial replication (diff + buy orders)
- [x] Trade detection (REST polling)
- [x] Rebalancing logic
- [x] Trading state tracking (holdings, budget, spend)
- [x] Structured reporting (JSON/table/log)
- [x] Graceful shutdown (Ctrl+C)
- [x] Accurate exit pricing (gamma API fallback for exited/resolved positions)
- [x] Dynamic budget: target sizing based on running capital (budget_remaining + holdings value)

**Testing findings:** Tested 30 min against a crypto up/down bot (`0xe594...`). These bots trade in concentrated bursts at 15-min window boundaries — REST polling at 5s catches them reliably. 8 rebalancing events, 191 simulated orders, +$14.17 simulated P&L. Sells-before-buys rebalancing and partial fills confirmed correct.

**Exit pricing:** When a held position leaves the active target set (trader exits or market resolves), the engine looks up its current price via a two-layer map: (1) active positions from the data API, (2) gamma API (`gamma::Client`, `markets?clob_token_ids=<id>`) for any assets not found in layer 1. Gamma errors propagate — no silent fallbacks. Exit sells always execute regardless of proceeds (positions resolved at price 0 must still be removed from holdings). Exit events are logged at INFO with reason (`resolved` for price 0/1, `trader exited` otherwise) and a short trader ID (last 6 chars of address). Market resolutions typically lag 5-20 minutes behind the scheduled close time due to UMA oracle settlement.

### Phase 3: Live Execution
- [x] 3A — CLOB auth & order probe (`probe_clob_trade`, `probe_my_positions`)
- [x] 3A — Engine: $1 minimum for buys, no minimum for sells (`MIN_ORDER_USD`)
- [x] 3B — Config file (`config.toml`, TOML format) replacing `.env` for all settings
- [x] 3B — `setup-account` binary (validate auth, print addresses + balance, update config; hidden interactive input via `rpassword`)
- [x] 3C — Auth module integration (`src/auth.rs` — `ClobContext`, `authenticate()`)
- [x] 3C — Order executor (`src/executor.rs` — `execute_orders()`, GTC limit orders)
- [x] 3C — Order status tracking (partial fills, resting, failed, skipped)
- [x] 3C — Resting order tracking (`RestingOrder` in `TradingState`, re-checked each poll cycle)
- [x] 3C — Retry with exponential backoff (rebuild+re-sign on each retry, 500ms/1s/2s)
- [x] 3C — `--live` mode in main binary (`--live` flag, conflicts with `--dry-run`)
- [x] 3C — Holdings seeding from Safe wallet on restart (fetches actual positions, prevents duplicate orders)
- [x] 3C — Balance guard (bail at startup if cash + holdings < budget; skip all buys mid-run if < $1 USDC)
- [x] 3C — Cancel stale resting orders on startup (`cancel_all_orders`) and on shutdown (`cancel_orders` for tracked resting orders, resolve state)
- [x] 3C — PartialFill remainder tracking (when an order partially fills, track the unfilled remainder as a resting order to prevent duplicate orders)

**Holdings seeding design:** On live startup, fetches ALL active positions from the Safe wallet (not just trader-related ones). This is intentional — the bot manages the full account and rebalances toward the target trader's portfolio, selling any positions the trader doesn't hold.

**Resting order design:** When an order rests on the CLOB book (not filled within 2s), it is tracked in `TradingState::resting_orders`. Budget is reserved immediately for resting buys. `effective_held_shares(asset)` returns `holdings + resting_buys - resting_sells` — the engine uses this to avoid duplicate orders. `effective_capital()` includes resting buy value at current market price. Each poll cycle calls `executor::check_resting_orders()` which queries `client.order(&id)` for each tracked order: filled → moves to holdings, cancelled → returns budget, still resting → no change.

**3C testing findings:** Dry-run regression confirmed: `execution_results` field correctly omitted from JSON via `skip_serializing_if`. Live smoke test: auth works, balance correctly converted from raw USDC (÷1,000,000), startup guard bails with clear message when balance < budget. Tested with gmpm trader (`0x14964...`, single Canada hockey position): $4 budget, 8.79 shares @ $0.45 placed as GTC limit order, filled on book. On restart: holdings seeded correctly from Safe wallet ($3.96 committed), engine saw existing shares and produced zero orders (no duplicate). Resting orders reserve budget and are tracked via `effective_held_shares()`; cancelled on shutdown.

**CLOB probe findings:** GnosisSafe (type 2) auth works with `Config::builder().use_server_time(true)` to avoid clock drift. Key import paths: `polymarket_client_sdk::auth::{LocalSigner, Signer}`, `clob::{Client, Config}`, `clob::types::{SignatureType, Side, Amount, OrderType}`. Minimum order size is $1 notional (size * price >= $1.00) for buys only — sells (closing positions) have no minimum and work below $1. Balance is returned in raw USDC units (6 decimals, e.g. `5000000` = $5). Limit orders at unfillable prices ($0.01) can be placed and cancelled without funds. Market orders use `Amount::usdc(dec!(2.00))?` with `OrderType::FAK`. Safe address derived via `derive_safe_wallet(eoa, POLYGON)`. Tested end-to-end: placed a $2 FAK market buy on Brazil presidential election (Lula Yes), received 3.85 shares at ~$0.52, position confirmed via both data API and SDK `data::Client::positions()`. Companion probe `probe_my_positions` fetches the Safe wallet's positions using the typed SDK data client.

### Phase 4: Tests and Documentation
- [ ] Unit tests for engine math
- [ ] Integration tests for state tracking
- [ ] README with setup/run instructions
- [ ] Config examples
- [ ] Final testing with real trader

### Phase 5: WebSocket Trade Detection (RTDS)
- [ ] RTDS activity/trades subscription
- [ ] Client-side proxyWallet filtering
- [ ] Watchdog/reconnect for silent stream death (~20min bug)
- [ ] Hybrid mode: RTDS primary + REST polling fallback

### Phase 6: Multi-Account Copytrading
- [ ] Multiple trader addresses
- [ ] Per-trader budget/config in `config.toml`
- [ ] Aggregated reporting
- [ ] Update docs for multi-account usage

## Running

```bash
# First-time setup (prompts for private key with hidden input)
cp config.toml.template config.toml
cargo run --bin setup-account

# Dry-run (private key from config.toml, copytrade params as CLI args)
cargo run --bin copytrade -- --dry-run \
  --trader-address 0x<proxy_wallet> \
  --budget 1000 \
  --copy-percentage 50 \
  --max-trade-size 30

# Live execution
cargo run --bin copytrade -- --live \
  --trader-address 0x<proxy_wallet> \
  --budget 1000 \
  --copy-percentage 50 \
  --max-trade-size 30
```

JSON events stream to stdout; logs to stderr. Ctrl+C triggers an exit summary.

To find active traders: `GET https://data-api.polymarket.com/v1/leaderboard?limit=15&orderBy=vol&timePeriod=day`

## Conventions

- Exploration probes go in standalone files/binaries before being integrated
- Results and API findings documented in `EXPLORATION.md`
- All secrets/keys kept out of version control
- `config.toml.template` is the canonical config reference; `config.toml` is gitignored
- Temporary/debug logs go in `log/` (gitignored)
