use std::str::FromStr;

use anyhow::{Context, Result};
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::auth::{LocalSigner, Normal, Signer};
use polymarket_client_sdk::clob::types::SignatureType;
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::types::Address;
use polymarket_client_sdk::{POLYGON, derive_safe_wallet};

use crate::CLOB_API_BASE;

/// Concrete signer type produced by `LocalSigner::from_str`.
pub type PrivateKeySigner = LocalSigner<k256::ecdsa::SigningKey>;

/// Authenticated CLOB context for live order execution.
pub struct ClobContext {
    pub client: Client<Authenticated<Normal>>,
    pub signer: PrivateKeySigner,
    pub eoa: Address,
    pub safe: Address,
}

/// Authenticate with the CLOB API using a hex-encoded private key.
pub async fn authenticate(private_key: &str) -> Result<ClobContext> {
    let signer = PrivateKeySigner::from_str(private_key)
        .context("invalid private key")?
        .with_chain_id(Some(POLYGON));

    let eoa = signer.address();
    let safe = derive_safe_wallet(eoa, POLYGON).context("failed to derive Safe address")?;

    let config = Config::builder().use_server_time(true).build();
    let client = Client::new(CLOB_API_BASE, config)?
        .authentication_builder(&signer)
        .signature_type(SignatureType::GnosisSafe)
        .authenticate()
        .await
        .context("CLOB authentication failed")?;

    Ok(ClobContext {
        client,
        signer,
        eoa,
        safe,
    })
}
