use crate::api_core::api_errors::{ApiError, internal_server_error};
use crate::wallet::*;
use actix::fut::{Ready, ready};
use actix_web::FromRequest;
use actix_web::web::{Data, Json};
use serde::{Deserialize, Serialize};

const HEADER_XPUB_VAN: &str = "xpub-van";
const HEADER_XPUB_COL: &str = "xpub-col";
const HEADER_MASTER_FINGERPRINT: &str = "master-fingerprint";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct XWalletKey {
    xpub_col: String,
    xpub_van: String,
    master_fingerprint: String,
}

impl FromRequest for XWalletKey {
    type Error = actix_web::Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(
        req: &actix_web::HttpRequest,
        _payload: &mut actix_web::dev::Payload,
    ) -> Self::Future {
        let xpub_van = req
            .headers()
            .get(HEADER_XPUB_VAN)
            .and_then(|v| v.to_str().ok());
        let xpub_col = req
            .headers()
            .get(HEADER_XPUB_COL)
            .and_then(|v| v.to_str().ok());
        let mf = req
            .headers()
            .get(HEADER_MASTER_FINGERPRINT)
            .and_then(|v| v.to_str().ok());

        let v = match (xpub_col, xpub_van, mf) {
            (Some(col), Some(van), Some(mf)) => Ok(XWalletKey {
                xpub_col: col.to_string(),
                xpub_van: van.to_string(),
                master_fingerprint: mf.to_string(),
            }),
            _ => Err(actix_web::error::ErrorUnauthorized(
                "Missing xpub-van/xpub-col/master-fingerprint header",
            )),
        };

        ready(v)
    }
}

fn map_rgb_error(err: rgb_lib::Error) -> ApiError {
    log::error!("{:?}", err);
    internal_server_error()
}

async fn derive_wctx(ctx: &WalletCtx, wallet_key: XWalletKey) -> Result<WalletCtx, ApiError> {
    ctx.for_ro_wallet(
        wallet_key.xpub_col,
        wallet_key.xpub_van,
        wallet_key.master_fingerprint,
    )
    .await
    .map_err(map_rgb_error)
}

#[derive(serde::Serialize)]
pub struct RegisterRes {
    pub address: String,
    pub btc_balance: rgb_lib::wallet::BtcBalance,
}

#[derive(serde::Deserialize)]
pub struct AssetId {
    pub asset_id: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetBalanceReq {
    pub asset_id: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Psbt {
    pub signed_psbt: String,
}

#[derive(serde::Deserialize)]
pub struct SendAssetEndReq {
    pub signed_psbt: String,
}

#[derive(serde::Deserialize)]
pub struct FailTransfer {
    pub batch_transfer_idx: Option<i32>,
    #[serde(default)]
    pub no_asset_only: bool,
    #[serde(default)]
    pub skip_sync: bool,
}

#[derive(serde::Serialize)]
pub struct SendResult {
    pub txid: String,
    pub batch_transfer_idx: i32,
}

pub async fn register(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
) -> Result<Json<RegisterRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;

    let address = ctx.address().await.map_err(map_rgb_error)?;
    let btc_balance = ctx.balance_btc().await.map_err(map_rgb_error)?;

    Ok(Json(RegisterRes {
        address: address.address,
        btc_balance,
    }))
}

pub async fn address(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
) -> Result<Json<String>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;

    let address = ctx.address().await.map_err(map_rgb_error)?;
    Ok(Json(address.address))
}

pub async fn btc_balance(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
) -> Result<Json<rgb_lib::wallet::BtcBalance>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let btc_balance = ctx.balance_btc().await.map_err(map_rgb_error)?;
    Ok(Json(btc_balance))
}

pub async fn asset_balance(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<AssetBalanceReq>,
) -> Result<Json<rgb_lib::wallet::Balance>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;

    let balance = ctx
        .balance_token(&req.asset_id)
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(balance))
}

pub async fn list_unspents(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
) -> Result<Json<Vec<rgb_lib::wallet::Unspent>>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;

    let list = ctx.list_unspends().await.map_err(map_rgb_error)?;
    Ok(Json(list))
}

pub async fn list_assets(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
) -> Result<Json<rgb_lib::wallet::Assets>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let list = ctx.assets().await.map_err(map_rgb_error)?;
    Ok(Json(list))
}

pub async fn list_transfers(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<AssetId>,
) -> Result<Json<Vec<rgb_lib::wallet::Transfer>>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;

    let list = ctx
        .transfers_by_asset(&req.asset_id)
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(list))
}

pub async fn list_transactions(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
) -> Result<Json<Vec<rgb_lib::wallet::Transaction>>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let list = ctx.transactions().await.map_err(map_rgb_error)?;
    Ok(Json(list))
}

pub async fn fail_transfers(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<FailTransfer>,
) -> Result<Json<bool>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let list = ctx
        .fail_transfer(req.batch_transfer_idx, req.no_asset_only, req.skip_sync)
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(list))
}
pub async fn create_utxos_begin(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<CreateUtxoBeginReq>,
) -> Result<Json<String>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;

    let psbt = ctx.create_utxo_begin(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(psbt))
}

pub async fn create_utxos_end(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<Psbt>,
) -> Result<Json<usize>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let n = ctx
        .create_utxo_end(req.signed_psbt.clone())
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(n))
}

pub async fn issue_nia(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<IssueNiaReq>,
) -> Result<Json<rgb_lib::wallet::AssetNIA>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    match ctx.issue_nia_token(req.0).await {
        Ok(v) => Ok(Json(v)),
        Err(e) => {
            log::error!("{:?}", e);
            Err(internal_server_error())
        }
    }
}

pub async fn send_begin(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<SendBeginReq>,
) -> Result<Json<String>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let res = ctx.send_begin(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(res))
}

pub async fn send_end(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<SendAssetEndReq>,
) -> Result<Json<SendResult>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let res = ctx
        .send_end(req.signed_psbt.clone())
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(SendResult {
        txid: res.txid,
        batch_transfer_idx: res.batch_transfer_idx,
    }))
}

pub async fn blind_receive(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<ReceiveReq>,
) -> Result<Json<rgb_lib::wallet::ReceiveData>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let res = ctx.receive_token(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(res))
}

pub async fn refresh(ctx: Data<WalletCtx>, wallet_key: XWalletKey) -> Result<Json<()>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    ctx.refresh_wallet().await.map_err(map_rgb_error)?;
    Ok(Json(()))
}

pub async fn drop(ctx: Data<WalletCtx>, wallet_key: XWalletKey) -> Result<Json<()>, ApiError> {
    // Remove the wallet from memory without re-registering it first.
    ctx.with_id(wallet_key.master_fingerprint)
        .drop_wallet()
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(()))
}
