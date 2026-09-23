//! The x402 `exact` scheme on Bitcoin Lightning (`lnbtc`).
//!
//! Implements the server's invoice checks and the facilitator's `/settle`
//! validation from the x402 `scheme_exact_lnbtc` spec. The facilitator role
//! runs in-process: checking a proof needs only the invoice, the preimage and
//! the replay store, not the receiving node.

use std::fmt;
use std::str::FromStr;

use bitcoin::hashes::Hash as _;
use bitcoin::secp256k1::PublicKey;
use lightning_invoice::{Currency, SignedRawBolt11Invoice, TaggedField};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::replay::ReplayStore;
use crate::x402::binding;
use crate::x402::types::{PaymentPayload, PaymentRequirements};

const SCHEME: &str = "exact";
const ASSET: &str = "BTC";
const ASSET_TRANSFER_METHOD: &str = "bolt11";
const PAYMENT_FLOW: &str = "upfront";

/// BOLT11 expiry when the invoice has no `x` field.
const DEFAULT_INVOICE_EXPIRY_SECS: u64 = 3600;

/// How long a consumed payment hash is kept after its invoice can no longer
/// pass validation.
const REPLAY_RETENTION_SECS: u64 = 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Mainnet,
    /// testnet3
    Testnet,
    /// Not in the spec, which defines only mainnet and testnet3. Follows its
    /// BIP-122 convention, so clients must opt in to the identifier. Every
    /// signet shares one genesis block, so custom signets are
    /// indistinguishable from the default one.
    Signet,
}

impl Network {
    /// Maps LND's chain network name. Regtest and testnet4 have no `lnbtc`
    /// identifier.
    pub fn from_lnd(name: &str) -> Option<Self> {
        match name {
            "mainnet" => Some(Network::Mainnet),
            "testnet" => Some(Network::Testnet),
            "signet" => Some(Network::Signet),
            _ => None,
        }
    }

    /// CAIP-2 identifier: `lnbtc:` and the first 32 hex characters of the
    /// genesis block hash.
    pub fn caip2(self) -> &'static str {
        match self {
            Network::Mainnet => "lnbtc:000000000019d6689c085ae165831e93",
            Network::Testnet => "lnbtc:000000000933ea01ad0ee984209779ba",
            Network::Signet => "lnbtc:00000008819873e925422c1ff0f99f7c",
        }
    }

    fn currency(self) -> Currency {
        match self {
            Network::Mainnet => Currency::Bitcoin,
            Network::Testnet => Currency::BitcoinTestnet,
            Network::Signet => Currency::Signet,
        }
    }
}

/// Stable `errorReason` strings from the spec's error vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorReason {
    UnsupportedScheme,
    NetworkMismatch,
    Asset,
    AmountMismatch,
    PayToMismatch,
    MaxTimeoutMismatch,
    RequestBinding,
    RequestMismatch,
    AssetTransferMethod,
    PaymentFlow,
    InvoiceMissing,
    InvoiceDecodeFailed,
    InvoiceDescription,
    InvoiceRequestMismatch,
    InvoicePayeeMismatch,
    InvoiceCurrencyMismatch,
    InvoiceAmountMismatch,
    InvoiceExpiryMismatch,
    InvoiceCreatedInFuture,
    DuplicateSettlement,
    PreimageMissing,
    PreimageMalformed,
    PreimageLength,
    PreimageHashMismatch,
    InvoiceExpired,
}

impl ErrorReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorReason::UnsupportedScheme => "unsupported_scheme",
            ErrorReason::NetworkMismatch => "network_mismatch",
            ErrorReason::Asset => "invalid_exact_lnbtc_asset",
            ErrorReason::AmountMismatch => "invalid_exact_lnbtc_amount_mismatch",
            ErrorReason::PayToMismatch => "invalid_exact_lnbtc_pay_to_mismatch",
            ErrorReason::MaxTimeoutMismatch => "invalid_exact_lnbtc_max_timeout_mismatch",
            ErrorReason::RequestBinding => "invalid_exact_lnbtc_request_binding",
            ErrorReason::RequestMismatch => "invalid_exact_lnbtc_request_mismatch",
            ErrorReason::AssetTransferMethod => "invalid_exact_lnbtc_asset_transfer_method",
            ErrorReason::PaymentFlow => "invalid_exact_lnbtc_payment_flow",
            ErrorReason::InvoiceMissing => "invalid_exact_lnbtc_invoice_missing",
            ErrorReason::InvoiceDecodeFailed => "invalid_exact_lnbtc_invoice_decode_failed",
            ErrorReason::InvoiceDescription => "invalid_exact_lnbtc_invoice_description",
            ErrorReason::InvoiceRequestMismatch => "invalid_exact_lnbtc_invoice_request_mismatch",
            ErrorReason::InvoicePayeeMismatch => "invalid_exact_lnbtc_invoice_payee_mismatch",
            ErrorReason::InvoiceCurrencyMismatch => "invalid_exact_lnbtc_invoice_currency_mismatch",
            ErrorReason::InvoiceAmountMismatch => "invalid_exact_lnbtc_invoice_amount_mismatch",
            ErrorReason::InvoiceExpiryMismatch => "invalid_exact_lnbtc_invoice_expiry_mismatch",
            ErrorReason::InvoiceCreatedInFuture => "invalid_exact_lnbtc_invoice_created_in_future",
            ErrorReason::DuplicateSettlement => "duplicate_settlement",
            ErrorReason::PreimageMissing => "invalid_exact_lnbtc_preimage_missing",
            ErrorReason::PreimageMalformed => "invalid_exact_lnbtc_preimage_malformed",
            ErrorReason::PreimageLength => "invalid_exact_lnbtc_preimage_length",
            ErrorReason::PreimageHashMismatch => "invalid_exact_lnbtc_preimage_hash_mismatch",
            ErrorReason::InvoiceExpired => "invalid_exact_lnbtc_invoice_expired",
        }
    }
}

impl fmt::Display for ErrorReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the server charges for one request, derived from its own
/// configuration and the actual request, never from the client's echo.
#[derive(Debug, Clone)]
pub struct Terms {
    pub network: Network,
    pub pay_to: PublicKey,
    pub amount_msat: u64,
    pub max_timeout_secs: u64,
    pub request_hash: [u8; 32],
    /// `http:1` `requestBindingParams`
    pub binding_params: Value,
    /// Allowance for clock differences with the invoice issuer.
    pub clock_skew_secs: u64,
}

impl Terms {
    pub fn requirements(&self, invoice: &str) -> PaymentRequirements {
        let extra = json!({
            "assetTransferMethod": ASSET_TRANSFER_METHOD,
            "paymentFlow": PAYMENT_FLOW,
            "requestHash": hex::encode(self.request_hash),
            "requestBindingProfile": binding::HTTP_PROFILE,
            "requestBindingParams": self.binding_params,
            "invoice": invoice,
        });
        PaymentRequirements {
            scheme: SCHEME.to_string(),
            network: self.network.caip2().to_string(),
            amount: self.amount_msat.to_string(),
            asset: ASSET.to_string(),
            pay_to: self.pay_to.to_string(),
            max_timeout_seconds: self.max_timeout_secs,
            extra: match extra {
                Value::Object(extra) => extra,
                _ => unreachable!(),
            },
        }
    }
}

/// The fields of a BOLT11 invoice this scheme checks.
struct Invoice {
    currency: Currency,
    amount_msat: Option<u64>,
    payment_hash: [u8; 32],
    /// `None` unless the invoice has exactly one description hash and no
    /// inline description.
    description_hash: Option<[u8; 32]>,
    payee: PublicKey,
    created: u64,
    expiry: u64,
}

impl Invoice {
    /// Strictly decodes `invoice` and verifies its signature.
    ///
    /// Works on the raw signed invoice rather than `Bolt11Invoice` because the
    /// latter also requires BOLT11 feature bits, which this scheme does not
    /// and the spec's own test vector lacks.
    fn decode(invoice: &str) -> Result<Self, ErrorReason> {
        let signed = SignedRawBolt11Invoice::from_str(invoice)
            .map_err(|_| ErrorReason::InvoiceDecodeFailed)?;
        if !signed.check_signature() {
            return Err(ErrorReason::InvoiceDecodeFailed);
        }
        let raw = signed.raw_invoice();

        let mut payment_hashes = Vec::new();
        let mut inline_descriptions = 0;
        let mut description_hashes = Vec::new();
        for field in raw.known_tagged_fields() {
            match field {
                TaggedField::PaymentHash(hash) => payment_hashes.push(hash.0.to_byte_array()),
                TaggedField::Description(_) => inline_descriptions += 1,
                TaggedField::DescriptionHash(hash) => {
                    description_hashes.push(hash.0.to_byte_array())
                }
                _ => {}
            }
        }
        let [payment_hash] = payment_hashes[..] else {
            return Err(ErrorReason::InvoiceDecodeFailed);
        };
        let description_hash = match description_hashes[..] {
            [hash] if inline_descriptions == 0 => Some(hash),
            _ => None,
        };

        // amounts are integral millisatoshis, i.e. whole multiples of 10 pico-BTC
        let amount_msat = match raw.hrp.raw_amount {
            None => None,
            Some(_) => match raw.amount_pico_btc() {
                Some(pico) if pico % 10 == 0 => Some(pico / 10),
                _ => return Err(ErrorReason::InvoiceDecodeFailed),
            },
        };

        let payee = match raw.payee_pub_key() {
            Some(payee) => payee.0,
            None => {
                signed
                    .recover_payee_pub_key()
                    .map_err(|_| ErrorReason::InvoiceDecodeFailed)?
                    .0
            }
        };

        Ok(Invoice {
            currency: raw.currency(),
            amount_msat,
            payment_hash,
            description_hash,
            payee,
            created: raw.data.timestamp.as_unix_timestamp(),
            expiry: raw
                .expiry_time()
                .map_or(DEFAULT_INVOICE_EXPIRY_SECS, |expiry| expiry.as_seconds()),
        })
    }

    /// Unix time at which the invoice expires.
    fn end(&self) -> u64 {
        self.created.saturating_add(self.expiry)
    }

    /// Checks the invoice against `terms` in the spec's order, except for
    /// expiry, whose policy differs between issuing and settling.
    fn check(&self, terms: &Terms, now: u64) -> Result<(), ErrorReason> {
        let description_hash = self
            .description_hash
            .ok_or(ErrorReason::InvoiceDescription)?;
        if description_hash != terms.request_hash {
            return Err(ErrorReason::InvoiceRequestMismatch);
        }
        if self.payee != terms.pay_to {
            return Err(ErrorReason::InvoicePayeeMismatch);
        }
        if self.currency != terms.network.currency() {
            return Err(ErrorReason::InvoiceCurrencyMismatch);
        }
        if self.amount_msat != Some(terms.amount_msat) {
            return Err(ErrorReason::InvoiceAmountMismatch);
        }
        if self.expiry != terms.max_timeout_secs {
            return Err(ErrorReason::InvoiceExpiryMismatch);
        }
        if self.created > now.saturating_add(terms.clock_skew_secs) {
            return Err(ErrorReason::InvoiceCreatedInFuture);
        }
        Ok(())
    }
}

/// Checks a freshly issued invoice before it goes out in a challenge.
pub fn check_issued_invoice(invoice: &str, terms: &Terms, now: u64) -> Result<(), ErrorReason> {
    let invoice = Invoice::decode(invoice)?;
    invoice.check(terms, now)?;
    if now > invoice.end() {
        return Err(ErrorReason::InvoiceExpired);
    }
    Ok(())
}

/// A validated proof of payment that has not yet been consumed.
#[derive(Debug)]
struct Proof {
    payment_hash: [u8; 32],
    /// Earliest time the consumption key may be pruned.
    retain_until: u64,
}

/// Runs the facilitator's `/settle` checks on `payload` against `terms`,
/// treating everything in the payload as untrusted.
fn verify(payload: &PaymentPayload, terms: &Terms, now: u64) -> Result<Proof, ErrorReason> {
    let accepted = &payload.accepted;

    // 1-2: core fields. Our own requirements are valid, so matching them
    // also validates the accepted side.
    if accepted.scheme != SCHEME {
        return Err(ErrorReason::UnsupportedScheme);
    }
    if accepted.network != terms.network.caip2() {
        return Err(ErrorReason::NetworkMismatch);
    }
    if accepted.amount != terms.amount_msat.to_string() {
        return Err(ErrorReason::AmountMismatch);
    }
    if accepted.asset != ASSET {
        return Err(ErrorReason::Asset);
    }
    if accepted.pay_to != terms.pay_to.to_string() {
        return Err(ErrorReason::PayToMismatch);
    }
    if accepted.max_timeout_seconds != terms.max_timeout_secs {
        return Err(ErrorReason::MaxTimeoutMismatch);
    }

    // 3: extra fields
    check_extra(&accepted.extra, terms)?;

    // 4-5: the accepted invoice, which may differ from any newer challenge's
    let invoice = match accepted.extra.get("invoice") {
        Some(Value::String(invoice)) if !invoice.is_empty() => invoice,
        _ => return Err(ErrorReason::InvoiceMissing),
    };
    let invoice = Invoice::decode(invoice)?;
    invoice.check(terms, now)?;

    // 6: the preimage
    let preimage = match payload.payload.get("preimage") {
        None => return Err(ErrorReason::PreimageMissing),
        Some(Value::String(preimage)) => preimage,
        Some(_) => return Err(ErrorReason::PreimageMalformed),
    };
    if !preimage.bytes().all(is_lower_hex_digit) {
        return Err(ErrorReason::PreimageMalformed);
    }
    if preimage.len() != 64 {
        return Err(ErrorReason::PreimageLength);
    }
    let preimage = hex::decode(preimage).map_err(|_| ErrorReason::PreimageMalformed)?;
    if Sha256::digest(&preimage).as_slice() != invoice.payment_hash {
        return Err(ErrorReason::PreimageHashMismatch);
    }

    // 7: a payment that completed just before expiry may still be claimed
    // within the clock skew allowance
    let last_valid = invoice.end().saturating_add(terms.clock_skew_secs);
    if now > last_valid {
        return Err(ErrorReason::InvoiceExpired);
    }

    Ok(Proof {
        payment_hash: invoice.payment_hash,
        retain_until: last_valid.saturating_add(REPLAY_RETENTION_SECS),
    })
}

fn check_extra(extra: &Map<String, Value>, terms: &Terms) -> Result<(), ErrorReason> {
    match extra.get("assetTransferMethod") {
        None => {}
        Some(Value::String(method)) if method == ASSET_TRANSFER_METHOD => {}
        Some(_) => return Err(ErrorReason::AssetTransferMethod),
    }
    match extra.get("paymentFlow") {
        Some(Value::String(flow)) if flow == PAYMENT_FLOW => {}
        _ => return Err(ErrorReason::PaymentFlow),
    }

    // Validate the binding fields' syntax before comparing them, so a
    // malformed binding and a well-formed but different one are told apart
    let request_hash = match extra.get("requestHash") {
        Some(Value::String(hash)) if hash.len() == 64 && hash.bytes().all(is_lower_hex_digit) => {
            hash
        }
        _ => return Err(ErrorReason::RequestBinding),
    };
    match extra.get("requestBindingProfile") {
        Some(Value::String(profile)) if profile == binding::HTTP_PROFILE => {}
        _ => return Err(ErrorReason::RequestBinding),
    }
    let params = extra
        .get("requestBindingParams")
        .filter(|params| binding::is_valid_http_params(params))
        .ok_or(ErrorReason::RequestBinding)?;

    // Valid http:1 params hold only strings, so value equality is JCS equality
    if *request_hash != hex::encode(terms.request_hash) || *params != terms.binding_params {
        return Err(ErrorReason::RequestMismatch);
    }
    Ok(())
}

fn is_lower_hex_digit(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

#[derive(Debug)]
pub enum SettleError {
    Rejected(ErrorReason),
    Store(anyhow::Error),
}

/// A consumed proof: `transaction` is the invoice's payment hash.
#[derive(Debug)]
pub struct Settlement {
    pub transaction: String,
}

/// Validates `payload` and atomically consumes its payment hash. Only after
/// this succeeds may the paid request be served.
pub fn settle(
    store: &ReplayStore,
    payload: &PaymentPayload,
    terms: &Terms,
    now: u64,
) -> Result<Settlement, SettleError> {
    let proof = verify(payload, terms, now).map_err(SettleError::Rejected)?;
    let payment_hash = hex::encode(proof.payment_hash);
    let key = format!("{}:{payment_hash}", terms.network.caip2());

    if store
        .consume(&key, proof.retain_until)
        .map_err(SettleError::Store)?
    {
        Ok(Settlement {
            transaction: payment_hash,
        })
    } else {
        Err(SettleError::Rejected(ErrorReason::DuplicateSettlement))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;
    use std::time::Duration;

    use bitcoin::hashes::sha256;
    use bitcoin::secp256k1::{Secp256k1, SecretKey};
    use lightning_invoice::{InvoiceBuilder, PaymentSecret};

    use super::*;
    use crate::x402::types::PaymentRequirements;

    /// The spec's test vector: `GET https://api.example.com/article/A`,
    /// signed by secret key 1 at unix time 1700000000.
    const SPEC_INVOICE: &str = "lnbc250n1pj48ugqpp54y3u9s8ylemsv8l3ewyzzu0klhujvuvmkl6llchq23vy8rzjsf0qsp5zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygshp5p4nz8am4uqj4q8a87z3sk4x6yk4dv2mvel34epw68qqkwy0xcqvqxqzfvcqpjr4rx6ls6j5rpwknuea64evlk7yfx56wmqcer5eerekdsn9tlv6v4ex9mlz5dtm9qapl3svwlqcf7837dmjkru9z9w4h2rvm0md52w2sqxrwu5f";
    const SPEC_PREIMAGE: &str = "0001020304050607080900010203040506070809000102030405060708090102";
    const SPEC_PAYMENT_HASH: &str =
        "a923c2c0e4fe77061ff1cb882171f6fdf926719bb7f5ffe2e05458438c52825e";
    const SPEC_TIME: u64 = 1_700_000_000;
    const ARTICLE_A: &str = "0d6623f775e025501fa7f0a30b54da25aad62b6ccfe35c85da38016711e6c018";
    const ARTICLE_B: &str = "4a99860f75eed1ea8178a5db488e044173bc570c8a6210f2c8590cdf8622d509";

    pub fn secret_key() -> SecretKey {
        SecretKey::from_slice(&[[0u8; 31].as_slice(), &[1]].concat()).unwrap()
    }

    fn hash32(hex_str: &str) -> [u8; 32] {
        hex::decode(hex_str).unwrap().try_into().unwrap()
    }

    fn spec_terms(request_hash: &str) -> Terms {
        Terms {
            network: Network::Mainnet,
            pay_to: PublicKey::from_secret_key(&Secp256k1::new(), &secret_key()),
            amount_msat: 25_000,
            max_timeout_secs: 300,
            request_hash: hash32(request_hash),
            binding_params: binding::http_params(&[]),
            clock_skew_secs: 60,
        }
    }

    fn payload(accepted: PaymentRequirements, preimage: &str) -> PaymentPayload {
        PaymentPayload {
            x402_version: 2,
            accepted,
            payload: json!({ "preimage": preimage }).as_object().unwrap().clone(),
        }
    }

    fn spec_payload() -> PaymentPayload {
        payload(
            spec_terms(ARTICLE_A).requirements(SPEC_INVOICE),
            SPEC_PREIMAGE,
        )
    }

    fn memory_store() -> ReplayStore {
        ReplayStore::open(Path::new(":memory:")).unwrap()
    }

    /// Signs an invoice for `terms` with `key`, returning it and its preimage.
    pub fn sign_invoice(
        key: &SecretKey,
        currency: Currency,
        amount_msat: u64,
        description_hash: [u8; 32],
        expiry_secs: u64,
        created: u64,
    ) -> (String, [u8; 32]) {
        let preimage: [u8; 32] = rand::random();
        let invoice = InvoiceBuilder::new(currency)
            .description_hash(sha256::Hash::from_byte_array(description_hash))
            .payment_hash(sha256::Hash::from_byte_array(
                Sha256::digest(preimage).into(),
            ))
            .payment_secret(PaymentSecret(rand::random()))
            .duration_since_epoch(Duration::from_secs(created))
            .min_final_cltv_expiry_delta(18)
            .amount_milli_satoshis(amount_msat)
            .expiry_time(Duration::from_secs(expiry_secs))
            .build_signed(|hash| Secp256k1::new().sign_ecdsa_recoverable(hash, key))
            .unwrap();
        (invoice.to_string(), preimage)
    }

    fn rejected(payload: &PaymentPayload, terms: &Terms, now: u64) -> ErrorReason {
        verify(payload, terms, now).unwrap_err()
    }

    #[test]
    fn network_ids_follow_bip122() {
        for (network, bitcoin_network) in [
            (Network::Mainnet, bitcoin::Network::Bitcoin),
            (Network::Testnet, bitcoin::Network::Testnet),
            (Network::Signet, bitcoin::Network::Signet),
        ] {
            let genesis = bitcoin::blockdata::constants::genesis_block(bitcoin_network)
                .block_hash()
                .to_string();
            assert_eq!(network.caip2(), format!("lnbtc:{}", &genesis[..32]));
        }
    }

    #[test]
    fn signet_invoice_settles_only_on_signet() {
        let terms = Terms {
            network: Network::Signet,
            ..spec_terms(ARTICLE_A)
        };
        let key = secret_key();
        let hash = terms.request_hash;

        let (invoice, preimage) =
            sign_invoice(&key, Currency::Signet, 25_000, hash, 300, SPEC_TIME);
        let signet = payload(terms.requirements(&invoice), &hex::encode(preimage));
        let settlement = settle(&memory_store(), &signet, &terms, SPEC_TIME).unwrap();
        assert_eq!(
            settlement.transaction,
            hex::encode(Sha256::digest(preimage))
        );

        // a testnet invoice shares the tb prefix but not the currency
        let (invoice, preimage) =
            sign_invoice(&key, Currency::BitcoinTestnet, 25_000, hash, 300, SPEC_TIME);
        let testnet = payload(terms.requirements(&invoice), &hex::encode(preimage));
        assert_eq!(
            rejected(&testnet, &terms, SPEC_TIME),
            ErrorReason::InvoiceCurrencyMismatch
        );
    }

    #[test]
    fn spec_vector_settles() {
        let store = memory_store();
        let settlement =
            settle(&store, &spec_payload(), &spec_terms(ARTICLE_A), SPEC_TIME).unwrap();
        assert_eq!(settlement.transaction, SPEC_PAYMENT_HASH);
    }

    #[test]
    fn spec_vector_passes_issue_checks() {
        check_issued_invoice(SPEC_INVOICE, &spec_terms(ARTICLE_A), SPEC_TIME).unwrap();
    }

    #[test]
    fn proof_settles_only_once() {
        let store = memory_store();
        let terms = spec_terms(ARTICLE_A);
        settle(&store, &spec_payload(), &terms, SPEC_TIME).unwrap();
        assert!(matches!(
            settle(&store, &spec_payload(), &terms, SPEC_TIME),
            Err(SettleError::Rejected(ErrorReason::DuplicateSettlement))
        ));
    }

    #[test]
    fn proof_for_other_request_is_rejected() {
        // article A's proof presented for article B at the same price
        let terms = spec_terms(ARTICLE_B);
        assert_eq!(
            rejected(&spec_payload(), &terms, SPEC_TIME),
            ErrorReason::RequestMismatch
        );

        // echoing B's hash doesn't help: the invoice still commits to A
        let mut payload = spec_payload();
        payload
            .accepted
            .extra
            .insert("requestHash".into(), json!(ARTICLE_B));
        assert_eq!(
            rejected(&payload, &terms, SPEC_TIME),
            ErrorReason::InvoiceRequestMismatch
        );
    }

    #[test]
    fn missing_or_malformed_binding_is_rejected() {
        let terms = spec_terms(ARTICLE_A);
        for field in [
            "requestHash",
            "requestBindingProfile",
            "requestBindingParams",
        ] {
            let mut payload = spec_payload();
            payload.accepted.extra.remove(field);
            assert_eq!(
                rejected(&payload, &terms, SPEC_TIME),
                ErrorReason::RequestBinding
            );
        }

        for (field, value) in [
            ("requestHash", json!(ARTICLE_A.to_uppercase())),
            ("requestBindingProfile", json!("mcp:1")),
            ("requestBindingParams", json!({})),
            (
                "requestBindingParams",
                json!({ "headers": [], "unknown": [] }),
            ),
        ] {
            let mut payload = spec_payload();
            payload.accepted.extra.insert(field.into(), value);
            assert_eq!(
                rejected(&payload, &terms, SPEC_TIME),
                ErrorReason::RequestBinding
            );
        }

        // a well-formed but different header list is a mismatch
        let mut payload = spec_payload();
        payload.accepted.extra.insert(
            "requestBindingParams".into(),
            json!({ "headers": ["accept"] }),
        );
        assert_eq!(
            rejected(&payload, &terms, SPEC_TIME),
            ErrorReason::RequestMismatch
        );
    }

    #[test]
    fn core_field_mismatches_are_rejected() {
        let terms = spec_terms(ARTICLE_A);
        type Mutation = fn(&mut PaymentRequirements);
        let cases: [(Mutation, ErrorReason); 6] = [
            (|a| a.scheme = "upto".into(), ErrorReason::UnsupportedScheme),
            (
                |a| a.network = Network::Testnet.caip2().into(),
                ErrorReason::NetworkMismatch,
            ),
            (|a| a.amount = "1000".into(), ErrorReason::AmountMismatch),
            (|a| a.asset = "USD".into(), ErrorReason::Asset),
            (|a| a.pay_to = "02".repeat(33), ErrorReason::PayToMismatch),
            (
                |a| a.max_timeout_seconds = 60,
                ErrorReason::MaxTimeoutMismatch,
            ),
        ];
        for (mutate, reason) in cases {
            let mut payload = spec_payload();
            mutate(&mut payload.accepted);
            assert_eq!(rejected(&payload, &terms, SPEC_TIME), reason);
        }
    }

    #[test]
    fn transfer_method_and_flow_are_checked() {
        let terms = spec_terms(ARTICLE_A);

        let mut payload = spec_payload();
        payload.accepted.extra.remove("assetTransferMethod");
        verify(&payload, &terms, SPEC_TIME).unwrap();

        payload
            .accepted
            .extra
            .insert("assetTransferMethod".into(), json!("bolt12"));
        assert_eq!(
            rejected(&payload, &terms, SPEC_TIME),
            ErrorReason::AssetTransferMethod
        );

        let mut payload = spec_payload();
        payload.accepted.extra.remove("paymentFlow");
        assert_eq!(
            rejected(&payload, &terms, SPEC_TIME),
            ErrorReason::PaymentFlow
        );
    }

    #[test]
    fn preimage_is_checked() {
        let terms = spec_terms(ARTICLE_A);
        let accepted = || spec_terms(ARTICLE_A).requirements(SPEC_INVOICE);

        let mut missing = spec_payload();
        missing.payload.clear();
        assert_eq!(
            rejected(&missing, &terms, SPEC_TIME),
            ErrorReason::PreimageMissing
        );

        let cases = [
            ("AB".repeat(32), ErrorReason::PreimageMalformed),
            (
                format!("0x{}", &SPEC_PREIMAGE[2..]),
                ErrorReason::PreimageMalformed,
            ),
            (SPEC_PREIMAGE[2..].to_string(), ErrorReason::PreimageLength),
            ("00".repeat(32), ErrorReason::PreimageHashMismatch),
        ];
        for (preimage, reason) in cases {
            assert_eq!(
                rejected(&payload(accepted(), &preimage), &terms, SPEC_TIME),
                reason
            );
        }
    }

    #[test]
    fn expiry_grace_window() {
        let terms = spec_terms(ARTICLE_A);
        let last_valid = SPEC_TIME + 300 + 60;
        verify(&spec_payload(), &terms, last_valid).unwrap();
        assert_eq!(
            rejected(&spec_payload(), &terms, last_valid + 1),
            ErrorReason::InvoiceExpired
        );

        // but a challenge must not go out with an already expired invoice
        assert_eq!(
            check_issued_invoice(SPEC_INVOICE, &terms, SPEC_TIME + 301),
            Err(ErrorReason::InvoiceExpired)
        );
    }

    #[test]
    fn invoice_created_in_future_is_rejected() {
        let terms = spec_terms(ARTICLE_A);
        verify(&spec_payload(), &terms, SPEC_TIME - 60).unwrap();
        assert_eq!(
            rejected(&spec_payload(), &terms, SPEC_TIME - 61),
            ErrorReason::InvoiceCreatedInFuture
        );
    }

    #[test]
    fn invoice_mismatches_are_rejected() {
        let terms = spec_terms(ARTICLE_A);
        let key = secret_key();
        let hash = terms.request_hash;
        let other_key = SecretKey::from_slice(&[2; 32]).unwrap();

        let cases = [
            (
                sign_invoice(&other_key, Currency::Bitcoin, 25_000, hash, 300, SPEC_TIME),
                ErrorReason::InvoicePayeeMismatch,
            ),
            (
                sign_invoice(&key, Currency::BitcoinTestnet, 25_000, hash, 300, SPEC_TIME),
                ErrorReason::InvoiceCurrencyMismatch,
            ),
            (
                sign_invoice(&key, Currency::Bitcoin, 24_000, hash, 300, SPEC_TIME),
                ErrorReason::InvoiceAmountMismatch,
            ),
            (
                sign_invoice(&key, Currency::Bitcoin, 25_000, hash, 600, SPEC_TIME),
                ErrorReason::InvoiceExpiryMismatch,
            ),
        ];
        for ((invoice, preimage), reason) in cases {
            let payload = payload(terms.requirements(&invoice), &hex::encode(preimage));
            assert_eq!(rejected(&payload, &terms, SPEC_TIME), reason);
        }

        // a correctly signed invoice with its own preimage settles
        let (invoice, preimage) =
            sign_invoice(&key, Currency::Bitcoin, 25_000, hash, 300, SPEC_TIME);
        let payload = payload(terms.requirements(&invoice), &hex::encode(preimage));
        verify(&payload, &terms, SPEC_TIME).unwrap();
    }

    #[test]
    fn inline_description_is_rejected() {
        let terms = spec_terms(ARTICLE_A);
        let key = secret_key();
        let preimage: [u8; 32] = rand::random();
        let invoice = InvoiceBuilder::new(Currency::Bitcoin)
            .description("article A".into())
            .payment_hash(sha256::Hash::from_byte_array(
                Sha256::digest(preimage).into(),
            ))
            .payment_secret(PaymentSecret(rand::random()))
            .duration_since_epoch(Duration::from_secs(SPEC_TIME))
            .min_final_cltv_expiry_delta(18)
            .amount_milli_satoshis(25_000)
            .expiry_time(Duration::from_secs(300))
            .build_signed(|hash| Secp256k1::new().sign_ecdsa_recoverable(hash, &key))
            .unwrap()
            .to_string();

        let payload = payload(terms.requirements(&invoice), &hex::encode(preimage));
        assert_eq!(
            rejected(&payload, &terms, SPEC_TIME),
            ErrorReason::InvoiceDescription
        );
    }

    #[test]
    fn undecodable_invoice_is_rejected() {
        let terms = spec_terms(ARTICLE_A);

        let mut missing = spec_payload();
        missing.accepted.extra.remove("invoice");
        assert_eq!(
            rejected(&missing, &terms, SPEC_TIME),
            ErrorReason::InvoiceMissing
        );

        // flipping a character breaks the checksum or the signature
        let mut tampered = SPEC_INVOICE.to_string();
        tampered.replace_range(
            20..21,
            if &SPEC_INVOICE[20..21] == "q" {
                "p"
            } else {
                "q"
            },
        );
        let payload = payload(terms.requirements(&tampered), SPEC_PREIMAGE);
        assert_eq!(
            rejected(&payload, &terms, SPEC_TIME),
            ErrorReason::InvoiceDecodeFailed
        );
    }
}
