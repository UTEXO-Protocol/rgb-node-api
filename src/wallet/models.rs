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

/// Which kind of RGB invoice a receive creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveKind {
    /// The sender assigns to a blinded UTXO this wallet already holds.
    Blind,
    /// The sender's transaction creates a new output paying this wallet, so
    /// no colorable UTXO is needed up front.
    Witness,
}

/// `SendBtcBeginRequestModel` in the API spec.
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
    /// Recipient's Ed25519 public key (hex), the invoice `pk` field. rgb-lib
    /// requires it when `transport_endpoints` is not empty.
    #[serde(default)]
    pub public_key: Option<String>,
    /// The invoice `enc` field. Per the invoice spec, a missing `enc` means
    /// encryption is required.
    #[serde(default = "default_encryption")]
    pub encryption: rgb_lib::Encryption,
    /// The invoice expiration. rgb-lib requires it when `transport_endpoints`
    /// is not empty.
    #[serde(default)]
    pub expiration_timestamp: Option<u64>,
}

fn default_encryption() -> rgb_lib::Encryption {
    rgb_lib::Encryption::Required
}

impl From<SendRecipient> for rgb_lib::wallet::Recipient {
    fn from(r: SendRecipient) -> Self {
        rgb_lib::wallet::Recipient {
            recipient_id: r.recipient_id,
            witness_data: r.witness_data,
            assignment: rgb_lib::Assignment::Fungible(r.amount),
            transport_endpoints: r.transport_endpoints,
            public_key: r.public_key,
            encryption: r.encryption,
            expiration_timestamp: r.expiration_timestamp,
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
    /// Absolute Unix deadline of the transfer. rgb-lib requires it to be at
    /// least 5 minutes ahead and, for proxy recipients, at least 10 minutes
    /// before the earliest invoice expiration. When omitted, it is derived
    /// from the recipients' invoice expirations.
    #[serde(default)]
    pub expiration_timestamp: Option<u64>,
}

/// rgb-lib's margin between the transfer and the earliest invoice expiration.
const INVOICE_EXPIRATION_MARGIN_SECS: u64 = 600;

impl SendBeginReq {
    /// The transfer deadline: the requested one, or the earliest recipient
    /// invoice expiration minus rgb-lib's margin. Range checks beyond "in the
    /// future" are left to rgb-lib, which knows the exact rules.
    pub fn expiration(&self) -> Result<u64, rgb_lib::Error> {
        let expiration = match self.expiration_timestamp {
            Some(expiration) => expiration,
            None => self
                .recipient_map
                .values()
                .flatten()
                .filter_map(|r| r.expiration_timestamp)
                .min()
                .ok_or(rgb_lib::Error::InvalidExpiration)?
                .saturating_sub(INVOICE_EXPIRATION_MARGIN_SECS),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| rgb_lib::Error::InvalidExpiration)?
            .as_secs();
        if expiration <= now || expiration > i64::MAX as u64 {
            return Err(rgb_lib::Error::InvalidExpiration);
        }
        Ok(expiration)
    }
}

/// `BurnBeginRequestModel` in the API spec.
#[derive(serde::Deserialize)]
pub struct BurnBeginReq {
    pub asset_id: String,
    pub amount: u64,
    /// Hex-encoded 32 bytes committed in the burn (rgb-lib's `burn_reason`),
    /// e.g. the recipient of the released funds. `burn_reason` is accepted too.
    #[serde(default, alias = "burn_reason")]
    pub burn_recipient: Option<String>,
    #[serde(default = "default_fee_rate")]
    pub fee_rate: u64,
    #[serde(default = "default_min_confirmations")]
    pub min_confirmations: u8,
}

impl BurnBeginReq {
    /// The burn reason for rgb-lib: hex without the optional `0x` prefix.
    /// rgb-lib checks the hex and the 32-byte length.
    pub fn burn_reason(&self) -> Option<String> {
        self.burn_recipient
            .as_deref()
            .map(|r| r.strip_prefix("0x").unwrap_or(r).to_owned())
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
    fn burn_reason_drops_the_optional_prefix() {
        assert_eq!(burn_req(None).burn_reason(), None);
        let hex32 = "11".repeat(32);
        assert_eq!(burn_req(Some(&hex32)).burn_reason(), Some(hex32.clone()));
        assert_eq!(
            burn_req(Some(&format!("0x{hex32}"))).burn_reason(),
            Some(hex32.clone())
        );
        let by_new_name: BurnBeginReq = serde_json::from_value(serde_json::json!({
            "asset_id": "rgb:x",
            "amount": 1,
            "burn_reason": hex32,
        }))
        .unwrap();
        assert_eq!(by_new_name.burn_reason(), Some("11".repeat(32)));
    }

    #[test]
    fn send_expiration_defaults_to_the_earliest_invoice_minus_the_margin() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let req = |expiration: Option<u64>, invoices: &[Option<u64>]| -> SendBeginReq {
            let recipients: Vec<_> = invoices
                .iter()
                .map(|exp| {
                    serde_json::json!({
                        "recipient_id": "r",
                        "amount": 1,
                        "transport_endpoints": [],
                        "expiration_timestamp": exp,
                    })
                })
                .collect();
            serde_json::from_value(serde_json::json!({
                "recipient_map": { "rgb:x": recipients },
                "expiration_timestamp": expiration,
            }))
            .unwrap()
        };
        let (early, late) = (now + 3600, now + 7200);
        assert_eq!(
            req(None, &[Some(late), Some(early), None])
                .expiration()
                .unwrap(),
            early - 600
        );
        assert_eq!(
            req(Some(now + 900), &[Some(early)]).expiration().unwrap(),
            now + 900
        );
        for bad in [
            req(None, &[None]),
            req(Some(now), &[]),
            req(None, &[Some(now + 60)]),
        ] {
            assert!(matches!(
                bad.expiration(),
                Err(rgb_lib::Error::InvalidExpiration)
            ));
        }
    }

    #[test]
    fn send_recipient_encryption_defaults_to_required() {
        let r: SendRecipient = serde_json::from_value(serde_json::json!({
            "recipient_id": "r",
            "amount": 1,
            "transport_endpoints": [],
        }))
        .unwrap();
        assert_eq!(r.encryption, rgb_lib::Encryption::Required);
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

#[cfg(test)]
mod request_tests {
    use super::*;

    #[test]
    fn send_btc_requires_address_amount_and_fee_rate() {
        let req: SendBtcReq = serde_json::from_value(serde_json::json!({
            "address": "bcrt1qxy",
            "amount": 1000,
            "fee_rate": 2,
        }))
        .unwrap();
        assert_eq!(
            (req.address.as_str(), req.amount, req.fee_rate),
            ("bcrt1qxy", 1000, 2)
        );
        for missing in ["address", "amount", "fee_rate"] {
            let mut body =
                serde_json::json!({ "address": "bcrt1qxy", "amount": 1000, "fee_rate": 2 });
            body.as_object_mut().unwrap().remove(missing);
            assert!(
                serde_json::from_value::<SendBtcReq>(body).is_err(),
                "{missing}"
            );
        }
    }
}
