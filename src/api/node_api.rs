use crate::api_core::api_errors::{ApiError, ApiErrorCode, error_with, internal_server_error};
use crate::wallet::*;
use actix::fut::{Ready, ready};
use actix_web::FromRequest;
use actix_web::http::StatusCode;
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
            // Same JSON envelope as every other error the API returns.
            _ => Err(error_with(
                StatusCode::UNAUTHORIZED,
                ApiErrorCode::AccessDenied,
                "missing xpub-van/xpub-col/master-fingerprint header",
            )
            .into()),
        };

        ready(v)
    }
}

/// Turn an `rgb_lib::Error` into an API error.
///
/// rgb-lib validates the request data (fee rate, amounts, recipients, PSBTs,
/// ...) inside the wallet, so most failures are the caller's, not ours: they
/// get a 4xx with the rgb-lib message and a `kind` detail holding the variant
/// name, so clients can react without parsing prose. Only genuine server-side
/// failures stay a 500 with an opaque message.
pub(super) fn map_rgb_error(err: rgb_lib::Error) -> ApiError {
    let class = classify(&err);
    let kind = error_kind(&err);

    if class.is_client_error() {
        log::warn!("{kind}: {err:?}");
    } else {
        log::error!("{kind}: {err:?}");
    }

    let (http_code, code) = match class {
        ErrorClass::NotFound => (StatusCode::NOT_FOUND, ApiErrorCode::NotFound),
        ErrorClass::BadInput => (StatusCode::BAD_REQUEST, ApiErrorCode::BadInput),
        ErrorClass::InvalidAddress => (StatusCode::BAD_REQUEST, ApiErrorCode::InvalidAddress),
        ErrorClass::NotEnoughBalance => (StatusCode::BAD_REQUEST, ApiErrorCode::NotEnoughBalance),
        ErrorClass::NeedMoreUtxos => (StatusCode::BAD_REQUEST, ApiErrorCode::NeedMoreUtxos),
        ErrorClass::NotEnoughAssets => (StatusCode::BAD_REQUEST, ApiErrorCode::NotEnoughAssets),
        ErrorClass::InvalidFeeRate => (StatusCode::BAD_REQUEST, ApiErrorCode::InvalidFeeRate),
        ErrorClass::InvalidPsbt => (StatusCode::BAD_REQUEST, ApiErrorCode::InvalidPsbt),
        ErrorClass::InvalidRecipient => (StatusCode::BAD_REQUEST, ApiErrorCode::InvalidRecipient),
        ErrorClass::Conflict => (StatusCode::CONFLICT, ApiErrorCode::Conflict),
        ErrorClass::Unsupported => (StatusCode::BAD_REQUEST, ApiErrorCode::Unsupported),
        ErrorClass::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::ServiceUnavailable,
        ),
        // Never leak internal details (db paths, stash state) to the client.
        ErrorClass::Internal => return internal_server_error().with_detail("kind", kind),
    };

    error_with(http_code, code, &err.to_string()).with_detail("kind", kind)
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
pub struct AssetBalanceReq {
    /// `assetId` is the pre-unification spelling, accepted for compatibility.
    #[serde(alias = "assetId")]
    pub asset_id: String,
}

#[derive(serde::Deserialize)]
pub struct Psbt {
    /// `signedPsbt` is the pre-unification spelling, accepted for compatibility.
    #[serde(alias = "signedPsbt")]
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

/// Every endpoint answers with a JSON object: a bare string, number, boolean or
/// `null` is awkward to type and parse in statically typed clients.
#[derive(serde::Serialize)]
pub struct PsbtRes {
    /// The unsigned PSBT, base64 encoded, to be signed by the client.
    pub psbt: String,
}

#[derive(serde::Serialize)]
pub struct CreateUtxosRes {
    /// How many UTXOs the finalized transaction created.
    pub created: usize,
}

#[derive(serde::Serialize)]
pub struct FailTransferRes {
    /// Whether any transfer changed to the failed status.
    pub changed: bool,
}

/// An operation that carries no data back. Kept as an (empty) object so fields
/// can be added later without breaking clients.
#[derive(serde::Serialize)]
pub struct EmptyRes {}

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
) -> Result<Json<AddressRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;

    let address = ctx.address().await.map_err(map_rgb_error)?;
    Ok(Json(address))
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
) -> Result<Json<FailTransferRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let changed = ctx
        .fail_transfer(req.batch_transfer_idx, req.no_asset_only, req.skip_sync)
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(FailTransferRes { changed }))
}

pub async fn create_utxos_begin(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<CreateUtxoBeginReq>,
) -> Result<Json<PsbtRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;

    let psbt = ctx.create_utxo_begin(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(PsbtRes { psbt }))
}

pub async fn create_utxos_end(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<Psbt>,
) -> Result<Json<CreateUtxosRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let created = ctx
        .create_utxo_end(req.signed_psbt.clone())
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(CreateUtxosRes { created }))
}

pub async fn issue_bfa(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<IssueBfaReq>,
) -> Result<Json<rgb_lib::wallet::AssetBFA>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    Ok(Json(
        ctx.issue_bfa_token(req.0).await.map_err(map_rgb_error)?,
    ))
}
pub async fn bridge_begin(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<BridgeBeginReq>,
) -> Result<Json<rgb_lib::wallet::BridgeBeginResult>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    Ok(Json(ctx.bridge_begin(req.0).await.map_err(map_rgb_error)?))
}
pub async fn bridge_end(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<SendAssetEndReq>,
) -> Result<Json<rgb_lib::wallet::OperationResult>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    Ok(Json(
        ctx.bridge_end(req.0.signed_psbt)
            .await
            .map_err(map_rgb_error)?,
    ))
}

pub async fn issue_nia(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<IssueNiaReq>,
) -> Result<Json<rgb_lib::wallet::AssetNIA>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let asset = ctx.issue_nia_token(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(asset))
}

pub async fn send_begin(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<SendBeginReq>,
) -> Result<Json<PsbtRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let psbt = ctx.send_begin(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(PsbtRes { psbt }))
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

pub async fn refresh(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
) -> Result<Json<EmptyRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    ctx.refresh_wallet().await.map_err(map_rgb_error)?;
    Ok(Json(EmptyRes {}))
}

pub async fn drop(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
) -> Result<Json<EmptyRes>, ApiError> {
    // Remove the wallet from memory without re-registering it first.
    ctx.with_id(wallet_key.master_fingerprint)
        .drop_wallet()
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(EmptyRes {}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_request_data_is_a_bad_request() {
        let err = map_rgb_error(rgb_lib::Error::InvalidFeeRate {
            details: "value under minimum 1".to_string(),
        });

        assert_eq!(err.http_code, StatusCode::BAD_REQUEST);
        assert_eq!(err.code, ApiErrorCode::InvalidFeeRate as u16);
        assert_eq!(err.message, "Invalid fee rate: value under minimum 1");
        assert_eq!(
            err.details.get("kind").map(String::as_str),
            Some("InvalidFeeRate")
        );
    }

    /// Clients must never get a bare string, number, boolean or `null` back.
    #[test]
    fn every_response_is_a_json_object() {
        let bodies = [
            serde_json::to_string(&PsbtRes {
                psbt: "cHNidP8B".to_string(),
            })
            .unwrap(),
            serde_json::to_string(&CreateUtxosRes { created: 3 }).unwrap(),
            serde_json::to_string(&FailTransferRes { changed: true }).unwrap(),
            serde_json::to_string(&AddressRes {
                address: "bcrt1qxy".to_string(),
            })
            .unwrap(),
            serde_json::to_string(&EmptyRes {}).unwrap(),
        ];

        assert_eq!(
            bodies,
            [
                r#"{"psbt":"cHNidP8B"}"#,
                r#"{"created":3}"#,
                r#"{"changed":true}"#,
                r#"{"address":"bcrt1qxy"}"#,
                r#"{}"#,
            ]
        );
    }

    #[test]
    fn internal_failures_stay_opaque() {
        let err = map_rgb_error(rgb_lib::Error::Database {
            details: "disk full".to_string(),
        });

        assert_eq!(err.http_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.code, ApiErrorCode::InternalError as u16);
        assert!(!err.message.contains("disk full"));
    }
}
