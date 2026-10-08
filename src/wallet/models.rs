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

/// `IssueAssetIfaRequestModel` in the API spec.
#[derive(serde::Deserialize)]
pub struct IssueIfaReq {
    pub ticker: String,
    pub name: String,
    #[serde(default)]
    pub precision: u8,
    pub amounts: Vec<u64>,
    /// Rights allocated at issuance, limiting how many more tokens can be created.
    #[serde(default)]
    pub inflation_amounts: Vec<u64>,
    #[serde(default)]
    pub reject_list_url: Option<String>,
}

/// `InflateBeginRequestModel` in the API spec.
#[derive(serde::Deserialize)]
pub struct InflateBeginReq {
    pub asset_id: String,
    /// Newly created tokens, one allocation per amount, paid to this wallet.
    pub inflation_amounts: Vec<u64>,
    #[serde(default = "default_fee_rate")]
    pub fee_rate: u64,
    #[serde(default = "default_min_confirmations")]
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
    #[serde(default)]
    pub donation: bool,
    #[serde(default = "default_fee_rate")]
    pub fee_rate: u64,
    #[serde(default = "default_min_confirmations")]
    pub min_confirmations: u8,
    /// Absolute Unix deadline from the recipient invoice (earliest for a batch).
    /// Required by the pinned RGB protocol; an arbitrary default can outlive the invoice.
    pub expiration_timestamp: u64,
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

/// `BurnBeginRequestModel` in the API spec.
#[derive(serde::Deserialize)]
pub struct BurnBeginReq {
    pub asset_id: String,
    pub amount: u64,
    /// Hex-encoded 32-byte recipient of the released funds, required for BFA.
    #[serde(default)]
    pub burn_recipient: Option<String>,
    #[serde(default = "default_fee_rate")]
    pub fee_rate: u64,
    #[serde(default = "default_min_confirmations")]
    pub min_confirmations: u8,
}

impl BurnBeginReq {
    /// Decode the hex recipient; the 32-byte length is enforced by rgb-lib.
    pub fn burn_recipient_bytes(&self) -> Result<Option<Vec<u8>>, rgb_lib::Error> {
        self.burn_recipient
            .as_deref()
            .map(|r| {
                hex::decode(r.strip_prefix("0x").unwrap_or(r)).map_err(|e| {
                    rgb_lib::Error::InvalidDetails {
                        details: format!("burn_recipient is not valid hex: {e}"),
                    }
                })
            })
            .transpose()
    }
}

/// `GetConsignmentRequestModel` in the API spec.
#[derive(serde::Deserialize)]
pub struct GetConsignmentReq {
    pub asset_id: String,
    /// TXID of the send or burn that produced the consignment.
    pub txid: String,
}

impl GetConsignmentReq {
    /// The TXID names a directory under the wallet: only accept a real TXID.
    pub fn validate_txid(&self) -> Result<(), rgb_lib::Error> {
        if self.txid.len() != 64 || !self.txid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(rgb_lib::Error::InvalidTxid);
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
pub struct GetConsignmentRes {
    /// Raw consignment file, hex encoded.
    pub bytes_hex: String,
}

#[cfg(test)]
mod burn_tests {
    use super::*;

    fn burn_req(recipient: Option<&str>) -> BurnBeginReq {
        serde_json::from_value(serde_json::json!({
            "asset_id": "rgb:x",
            "amount": 1,
            "burn_recipient": recipient,
        }))
        .unwrap()
    }

    #[test]
    fn burn_recipient_is_hex_with_optional_prefix() {
        assert_eq!(burn_req(None).burn_recipient_bytes().unwrap(), None);
        let hex32 = "11".repeat(32);
        assert_eq!(
            burn_req(Some(&hex32)).burn_recipient_bytes().unwrap(),
            Some(vec![0x11; 32])
        );
        assert_eq!(
            burn_req(Some(&format!("0x{hex32}")))
                .burn_recipient_bytes()
                .unwrap(),
            Some(vec![0x11; 32])
        );
        assert!(matches!(
            burn_req(Some("zz")).burn_recipient_bytes(),
            Err(rgb_lib::Error::InvalidDetails { .. })
        ));
    }

    #[test]
    fn consignment_lookup_only_accepts_a_txid() {
        let req = |txid: &str| GetConsignmentReq {
            asset_id: "rgb:x".into(),
            txid: txid.into(),
        };
        assert!(req(&"ab".repeat(32)).validate_txid().is_ok());
        for bad in ["", "../../etc", &"ab".repeat(31), &"zz".repeat(32)] {
            assert!(matches!(
                req(bad).validate_txid(),
                Err(rgb_lib::Error::InvalidTxid)
            ));
        }
    }
}
