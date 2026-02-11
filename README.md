# Polymarket Copytrade Bot

A Rust CLI bot that mirrors a target trader's Polymarket portfolio using proportional position
replication.

> **Disclaimer:** This is pre-alpha software provided "as is" without warranty of any kind. It was
> created as a quick experiment for educational purposes. Understand that bugs may result in loss of
> assets. Use at your own risk.

## Overview

This bot copies a target trader's portfolio by computing position weights and replicating them
proportionally within your budget. It supports both dry-run simulation (paper trading) and live
execution via the Polymarket CLOB API.

**Key concepts:**

- **Portfolio mirroring** — instead of copying individual trades, the bot snapshots the trader's
  current portfolio and continuously rebalances to stay aligned
- **copy-percentage** — fraction of your running capital allocated to replicating the trader's
  portfolio (0-100%)
- **max-trade-size** — maximum percentage of running capital in any single market position (0-100%)
- **budget** — initial capital; running budget floats with P&L as `budget_remaining + holdings_value`

## Prerequisites

- **Rust 1.88+** (2024 edition)
- **Polymarket account** with a funded Safe wallet (USDC on Polygon)
- A target trader's proxy wallet address to copy

## Quick Start

```bash
# Clone the repository
git clone https://github.com/flemosr/polymarket-copytrade.git
cd polymarket-copytrade

# Create config from template
cp config.toml.template config.toml

# First-time setup — validates auth, prints addresses + balance, saves private key
cargo run --bin setup-account

# Dry-run (simulated orders, no real trades)
cargo run --bin copytrade -- --dry-run \
  --trader-address 0x<proxy_wallet> \
  --budget 1000 \
  --copy-percentage 50 \
  --max-trade-size 30

# Live execution (real trades on Polymarket CLOB)
cargo run --bin copytrade -- --live \
  --trader-address 0x<proxy_wallet> \
  --budget 1000 \
  --copy-percentage 50 \
  --max-trade-size 30
```

JSON events stream to stdout; tracing logs to stderr. Press Ctrl+C for a graceful shutdown
with an exit summary.

## CLI Reference

### copytrade

```
copytrade --dry-run|--live [OPTIONS]

Required (exactly one):
  --dry-run                 Simulate trades without executing
  --live                    Execute real trades via CLOB API

Required:
  --trader-address <ADDR>   Trader's proxy wallet address
  --budget <USD>            Initial capital in USD
  --copy-percentage <0-100> Fraction of budget to allocate (%)
  --max-trade-size <0-100>  Max per-market position (% of budget)
```

### setup-account

```
setup-account [--private-key <HEX>]

  Without --private-key: prompts interactively (hidden input)
  With --private-key:    uses the provided hex key (scripted use)
```

Validates CLOB authentication, prints derived EOA and Safe wallet addresses, checks USDC balance,
and writes the private key to `config.toml`.

## How It Works

1. **Initial snapshot** — fetches the target trader's active positions via the data API,
   computes portfolio weights by value
2. **Target computation** — for each market, computes
   `target = min(weight * budget * copy_pct, max_trade_pct * budget)` and derives the target
   share count
3. **Order generation** — diffs target state against current holdings; sells first (to free
   budget), then buys (capped by available budget); buys below $1 notional are skipped; sells
   have no minimum
4. **Trade detection** — polls the data API for new trades (deduped by transaction hash); on
   detection, recomputes the full portfolio and rebalances
5. **Exit detection** — when a held position leaves the target set (trader exits or market
   resolves), generates a sell order using gamma API pricing
6. **Budget dynamics** — running budget = `budget_remaining + holdings_market_value`; losses
   shrink position sizes, gains grow them

In live mode, orders are placed as GTC limit orders on the CLOB with retry logic (exponential
backoff for transient failures). Resting orders are tracked to prevent duplicates and are cancelled
on shutdown.

## Configuration

`config.toml` (gitignored) holds account settings. Copy from the template:

```toml
[account]
private_key = ""          # Set by setup-account

[settings]
poll_interval_secs = 10   # Trade detection polling interval
```

Copytrade parameters (trader address, budget, copy percentage, max trade size) are passed as CLI
arguments.

Set `RUST_LOG` to control log verbosity (e.g., `RUST_LOG=info` or `RUST_LOG=debug`).

## Finding Traders

Use the Polymarket leaderboard API to find active traders:

```bash
curl -s 'https://data-api.polymarket.com/v1/leaderboard?limit=15&orderBy=vol&timePeriod=day' | jq
```

The returned addresses are proxy wallet addresses suitable for `--trader-address`.

## Architecture

| Module                 | Purpose                                            |
|------------------------|----------------------------------------------------|
| `config.rs`            | Config loading                                     |
| `types.rs`             | Domain types                                       |
| `api.rs`               | SDK wrappers (positions, trades, gamma pricing)    |
| `engine.rs`            | Portfolio math (weights, targets, orders)          |
| `state.rs`             | Holdings, budget, P&L, resting order tracking      |
| `auth.rs`              | CLOB authentication                                |
| `executor.rs`          | Live order execution (retry, balance guard)        |
| `reporter.rs`          | JSON event output and exit summary                 |
| `bin/copytrade.rs`     | Main binary — CLI, polling loop, shutdown          |
| `bin/setup_account.rs` | First-time account setup                           |

All modules live under `src/`.

## Running Tests

```bash
cargo test
```

Tests cover the two core modules:
- **Engine** — weight computation, target allocation, order generation (sells-before-buys, budget
  caps, minimum order sizes, exit detection)
- **State** — budget tracking, holdings management, resting order lifecycle, execution result
  processing, exit summary P&L

## License

This project is licensed under the [Apache License (Version 2.0)](LICENSE).

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion by
you, shall be licensed as Apache-2.0, without any additional terms or conditions.
