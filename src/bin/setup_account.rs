//! setup-account — First-time setup for the Polymarket copytrade bot.
//!
//! Expects `config.toml` to already exist (copied from `config.toml.template`).
//! Validates the private key, authenticates with the CLOB API,
//! prints account info (EOA, Safe wallet, USDC balance),
//! and updates the private key in the existing config file.
//!
//! By default, reads the private key interactively (hidden input) to avoid
//! leaking it into shell history. Use `--private-key` only for scripted/CI use.

use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::Parser;
use polymarket_client_sdk::auth::{LocalSigner, Signer};
use polymarket_client_sdk::clob::types::request::BalanceAllowanceRequest;
use polymarket_client_sdk::clob::types::SignatureType;
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::{POLYGON, derive_safe_wallet};
use rust_decimal::prelude::ToPrimitive;

use polymarket_copytrade::CLOB_API_BASE;
use polymarket_copytrade::config::{AppConfig, CONFIG_PATH};

#[derive(Parser)]
#[command(
    name = "setup-account",
    about = "Validate auth, print account info, and save private key to config.toml"
)]
struct Cli {
    /// Hex-encoded private key (with or without 0x prefix).
    /// If omitted, reads interactively with hidden input (recommended).
    #[arg(long)]
    private_key: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = Path::new(CONFIG_PATH);

    // Load existing config
    let mut app_config = AppConfig::load(config_path).with_context(|| {
        format!(
            "{} not found — copy config.toml.template to config.toml first",
            config_path.display()
        )
    })?;

    println!("=== Polymarket Copytrade — Account Setup ===\n");

    // ── Step 1: Read private key ───────────────────────────────────
    let private_key = match cli.private_key {
        Some(key) => key,
        None => {
            let key = rpassword::prompt_password("Enter private key (hex): ")
                .context("failed to read private key")?;
            if key.trim().is_empty() {
                bail!("private key cannot be empty");
            }
            key.trim().to_string()
        }
    };

    // ── Step 2: Validate private key ───────────────────────────────
    println!("Validating private key...");
    let signer = LocalSigner::from_str(&private_key)
        .context("invalid private key — expected hex-encoded (with or without 0x prefix)")?
        .with_chain_id(Some(POLYGON));

    let eoa = signer.address();
    println!("  EOA address:  {eoa}");

    let safe = derive_safe_wallet(eoa, POLYGON).context("failed to derive Safe wallet address")?;
    println!("  Safe address: {safe}");
    println!();

    // ── Step 3: Authenticate with CLOB ─────────────────────────────
    println!("Authenticating with CLOB API...");
    let config = Config::builder().use_server_time(true).build();
    let client = Client::new(CLOB_API_BASE, config)?
        .authentication_builder(&signer)
        .signature_type(SignatureType::GnosisSafe)
        .authenticate()
        .await
        .context("CLOB authentication failed — check your private key")?;
    println!("  Authentication successful");
    println!();

    // ── Step 4: Check balance ──────────────────────────────────────
    println!("Checking USDC balance...");
    let bal = client
        .balance_allowance(BalanceAllowanceRequest::default())
        .await
        .context("failed to fetch balance")?;

    // Balance is in raw USDC units (6 decimals)
    let balance_usd = bal.balance.to_f64().unwrap_or(0.0) / 1_000_000.0;
    println!("  USDC balance: ${balance_usd:.2}");
    if balance_usd < 1.0 {
        println!("  WARNING: Balance is very low — you'll need to deposit USDC to your Safe wallet to trade");
    }
    println!();

    // ── Step 5: Update private key in config.toml ──────────────────
    println!("Updating private key in {}...", config_path.display());
    app_config.account.private_key = private_key;
    app_config.save(config_path)?;
    println!("  Config updated successfully");
    println!();

    // ── Summary ────────────────────────────────────────────────────
    println!("=== Setup Complete ===");
    println!();
    println!("Account:");
    println!("  EOA:     {eoa}");
    println!("  Safe:    {safe}");
    println!("  Balance: ${balance_usd:.2}");
    println!();
    println!("Next steps:");
    println!("  cargo run --bin copytrade -- --dry-run \\");
    println!("    --trader-address <proxy_wallet> \\");
    println!("    --budget 1000 --copy-percentage 50 --max-trade-size 30");

    Ok(())
}
