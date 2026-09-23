pub type RResult<T> = Result<T, rgb_lib::Error>;

pub fn err_dead_thread() -> rgb_lib::Error {
    rgb_lib::Error::Internal {
        details: "wallet thread is dead".to_string(),
    }
}

fn default_min_confirmations() -> u8 {
    1
}

fn default_fee_rate() -> u64 {
    1
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct Info {
    pub status: bool,
    pub wallets: Vec<WalletInfo>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct WalletInfo {
    pub wallet_id: String,
    pub master_xpub: String,
    pub fingerprint: String,
    pub read_only: bool,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct AddressRes {
    pub address: String,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct BackupPath {
    pub filename: String,
    pub path: String,
}

/// `RgbInvoiceRequestModel` in the API spec.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct ReceiveReq {
    #[serde(default)]
    pub asset_id: Option<String>,
    #[serde(default)]
    pub amount: Option<u64>,
    #[serde(default = "default_duration_seconds")]
    pub duration_seconds: Option<u32>,
    #[serde(default = "default_min_confirmations")]
    pub min_confirmations: u8,
}

fn default_duration_seconds() -> Option<u32> {
    Some(3600)
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct SendBtcReq {
    pub address: String,
    pub amount: u64,
    pub fee_rate: u64,
}

/// `IssueAssetNiaRequestModel` in the API spec.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct IssueNiaReq {
    pub amounts: Vec<u64>,
    pub ticker: String,
    pub name: String,
    #[serde(default)]
    pub precision: u8,
}

/// Operator BFA genesis: allocates bridge rights, never a starting token supply.
#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueBfaReq {
    pub ticker: String,
    pub name: String,
    pub precision: u8,
    pub bridge_rights: u8,
    pub contract_address: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeBeginReq {
    pub asset_id: String,
    pub recipient: SendRecipient,
    pub fee_rate: u64,
    pub min_confirmations: u8,
}

/// `CreateUtxosBegin` in the API spec.
#[derive(serde::Deserialize)]
pub struct CreateUtxoBeginReq {
    /// `upTo` is the pre-unification spelling, accepted for compatibility.
    #[serde(default, alias = "upTo")]
    pub up_to: bool,
    #[serde(default = "default_num")]
    pub num: Option<u8>,
    #[serde(default = "default_size")]
    pub size: Option<u32>,
    /// `feeRate` is the pre-unification spelling, accepted for compatibility.
    #[serde(default = "default_fee_rate", alias = "feeRate")]
    pub fee_rate: u64,
}

fn default_num() -> Option<u8> {
    Some(5)
}

fn default_size() -> Option<u32> {
    Some(1000)
}

/// `Recipient` in the API spec — the amount is expressed as a plain integer and
/// mapped to an `Assignment::Fungible` when building the rgb-lib recipient.
#[derive(Clone, serde::Deserialize)]
pub struct SendRecipient {
    pub recipient_id: String,
    #[serde(default)]
    pub witness_data: Option<rgb_lib::wallet::WitnessData>,
    pub amount: u64,
    pub transport_endpoints: Vec<String>,
}

impl From<SendRecipient> for rgb_lib::wallet::Recipient {
    fn from(r: SendRecipient) -> Self {
        rgb_lib::wallet::Recipient {
            recipient_id: r.recipient_id,
            witness_data: r.witness_data,
            assignment: rgb_lib::Assignment::Fungible(r.amount),
            transport_endpoints: r.transport_endpoints,
        }
    }
}

/// `SendAssetBeginRequestModel` in the API spec.
#[derive(serde::Deserialize)]
pub struct SendBeginReq {
    pub recipient_map: std::collections::HashMap<String, Vec<SendRecipient>>,
    /// Absolute Unix deadline from the recipient invoice (earliest for a batch).
    /// Required by the pinned RGB protocol; an arbitrary default can outlive the invoice.
    pub expiration_timestamp: u64,
    #[serde(default)]
    pub donation: bool,
    #[serde(default = "default_fee_rate")]
    pub fee_rate: u64,
    #[serde(default = "default_min_confirmations")]
    pub min_confirmations: u8,
}

impl SendBeginReq {
    pub fn validate_expiration(&self) -> Result<(), rgb_lib::Error> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| rgb_lib::Error::InvalidExpiration)?
            .as_secs();
        if self.expiration_timestamp <= now || self.expiration_timestamp > i64::MAX as u64 {
            return Err(rgb_lib::Error::InvalidExpiration);
        }
        Ok(())
    }
}

#[cfg(test)]
mod expiration_tests {
    use super::*;

    #[test]
    fn send_requires_a_future_representable_invoice_deadline() {
        assert!(
            serde_json::from_value::<SendBeginReq>(serde_json::json!({ "recipient_map": {} }))
                .is_err()
        );
        for deadline in [0, 1, u64::MAX] {
            let request: SendBeginReq = serde_json::from_value(
                serde_json::json!({ "recipient_map": {}, "expiration_timestamp": deadline }),
            )
            .unwrap();
            assert!(matches!(
                request.validate_expiration(),
                Err(rgb_lib::Error::InvalidExpiration)
            ));
        }
        let deadline = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        let request: SendBeginReq = serde_json::from_value(
            serde_json::json!({ "recipient_map": {}, "expiration_timestamp": deadline }),
        )
        .unwrap();
        assert!(request.validate_expiration().is_ok());
    }
}
