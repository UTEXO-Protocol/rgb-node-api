pub type RResult<T> = Result<T, rgb_lib::Error>;

pub fn err_dead_thread() -> rgb_lib::Error {
    rgb_lib::Error::Internal {
        details: "wallet thread is dead".to_string(),
    }
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

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct ReceiveReq {
    pub asset_id: Option<String>,
    pub amount: u64,
    pub duration_seconds: Option<u32>,
    pub min_confirmations: u8,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct BackupReq {
    pub password: String,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct SendBtcReq {
    pub address: String,
    pub amount: u64,
    pub fee_rate: u64,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct SendTokenReq {
    pub asset_id: String,
    pub amount: u64,
    pub invoice: String,
    pub fee_rate: u64,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct SendPairReq {
    pub asset_id: String,
    pub amount: u64,
    pub recipient_id: String,
    pub btc_address: String,
    pub btc_amount: u64,
    pub fee_rate: u64,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct IssueNiaReq {
    pub ticker: String,
    pub name: String,
    pub precision: u8,
    pub premine: u64,
    pub fee_rate: u64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateUtxoBeginReq {
    pub up_to: bool,
    pub num: Option<u8>,
    pub size: Option<u32>,
    pub fee_rate: u64,
}

#[derive(serde::Deserialize)]
pub struct SendBeginReq {
    pub recipient_map: std::collections::HashMap<String, Vec<rgb_lib::wallet::Recipient>>,
    pub donation: bool,
    pub fee_rate: u64,
    pub min_confirmations: u8,
}
