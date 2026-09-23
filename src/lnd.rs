use fedimint_tonic_lnd::lnrpc;

use crate::config::Config;
use crate::error::AppError;

pub struct LndClient {
    client: fedimint_tonic_lnd::Client,
}

/// Identity and chain of the connected LND node.
pub struct NodeInfo {
    /// Compressed secp256k1 node public key, lowercase hex.
    pub pubkey: String,
    /// LND's network name, e.g. `mainnet`, `testnet` (testnet3), `signet`, `regtest`.
    pub network: String,
}

impl LndClient {
    pub async fn connect(config: &Config) -> Result<Self, AppError> {
        let client = fedimint_tonic_lnd::connect(
            config.lnd_address.clone(),
            &config.lnd_cert_path,
            &config.lnd_macaroon_path,
        )
        .await
        .map_err(|e| AppError::LndConnection(e.to_string()))?;

        tracing::info!("Connected to LND at {}", config.lnd_address);
        Ok(LndClient { client })
    }

    /// Create an invoice, returning (payment_hash, bolt11_payment_request).
    pub async fn create_invoice(
        &mut self,
        amount_sats: i64,
        memo: &str,
        expiry: i64,
    ) -> Result<(Vec<u8>, String), AppError> {
        let invoice = lnrpc::Invoice {
            value: amount_sats,
            memo: memo.to_string(),
            expiry,
            ..Default::default()
        };

        let response = self
            .client
            .lightning()
            .add_invoice(invoice)
            .await?
            .into_inner();

        Ok((response.r_hash, response.payment_request))
    }

    pub async fn get_info(&mut self) -> Result<NodeInfo, AppError> {
        let info = self
            .client
            .lightning()
            .get_info(lnrpc::GetInfoRequest {})
            .await?
            .into_inner();

        let network = info
            .chains
            .into_iter()
            .next()
            .map(|chain| chain.network)
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("LND reported no chains")))?;

        Ok(NodeInfo {
            pubkey: info.identity_pubkey,
            network,
        })
    }

    /// Create an invoice whose BOLT11 description hash is `description_hash`,
    /// returning the bolt11 payment request.
    pub async fn create_invoice_with_description_hash(
        &mut self,
        amount_msat: i64,
        description_hash: [u8; 32],
        expiry: i64,
    ) -> Result<String, AppError> {
        let invoice = lnrpc::Invoice {
            value_msat: amount_msat,
            description_hash: description_hash.to_vec(),
            expiry,
            ..Default::default()
        };

        let response = self
            .client
            .lightning()
            .add_invoice(invoice)
            .await?
            .into_inner();

        Ok(response.payment_request)
    }
}
