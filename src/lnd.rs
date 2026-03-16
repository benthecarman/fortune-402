use fedimint_tonic_lnd::lnrpc;

use crate::config::Config;
use crate::error::AppError;

pub struct LndClient {
    client: fedimint_tonic_lnd::Client,
}

pub enum InvoiceState {
    Settled { preimage: Vec<u8> },
    Open,
    Canceled,
    Accepted,
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

    /// Look up an invoice by payment hash to check its state.
    pub async fn lookup_invoice(
        &mut self,
        payment_hash: &[u8],
    ) -> Result<InvoiceState, AppError> {
        let request = lnrpc::PaymentHash {
            r_hash: payment_hash.to_vec(),
            ..Default::default()
        };

        let invoice = self
            .client
            .lightning()
            .lookup_invoice(request)
            .await?
            .into_inner();

        match lnrpc::invoice::InvoiceState::try_from(invoice.state) {
            Ok(lnrpc::invoice::InvoiceState::Settled) => Ok(InvoiceState::Settled {
                preimage: invoice.r_preimage,
            }),
            Ok(lnrpc::invoice::InvoiceState::Open) => Ok(InvoiceState::Open),
            Ok(lnrpc::invoice::InvoiceState::Canceled) => Ok(InvoiceState::Canceled),
            Ok(lnrpc::invoice::InvoiceState::Accepted) => Ok(InvoiceState::Accepted),
            Err(_) => Ok(InvoiceState::Open),
        }
    }
}
