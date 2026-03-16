use fedimint_tonic_lnd::lnrpc;

use crate::config::Config;
use crate::error::AppError;

pub struct LndClient {
    client: fedimint_tonic_lnd::Client,
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

}
