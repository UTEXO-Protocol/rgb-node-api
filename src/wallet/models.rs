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

/// `CreateUtxosBegin` in the API spec.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateUtxoBeginReq {
    #[serde(default)]
    pub up_to: bool,
    #[serde(default = "default_num")]
    pub num: Option<u8>,
    #[serde(default = "default_size")]
    pub size: Option<u32>,
    #[serde(default = "default_fee_rate")]
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
}
