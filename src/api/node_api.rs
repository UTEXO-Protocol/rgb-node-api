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

impl XWalletKey {
    fn from_headers(req: &actix_web::HttpRequest) -> Result<Self, ApiError> {
        let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok());
        match (
            header(HEADER_XPUB_COL),
            header(HEADER_XPUB_VAN),
            header(HEADER_MASTER_FINGERPRINT),
        ) {
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
            )),
        }
    }
}

impl FromRequest for XWalletKey {
    type Error = actix_web::Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(
        req: &actix_web::HttpRequest,
        _payload: &mut actix_web::dev::Payload,
    ) -> Self::Future {
        ready(Self::from_headers(req).map_err(Into::into))
    }
}

/// Turn an `rgb_lib::Error` into an API error.
///
/// rgb-lib validates the request data (fee rate, amounts, recipients, PSBTs,
/// ...) inside the wallet, so most failures are the caller's, not ours: they
/// get a 4xx with the rgb-lib message and a `kind` detail holding the variant
/// name, so clients can react without parsing prose. Only genuine server-side
/// failures stay a 500 with an opaque message.
fn map_rgb_error(err: rgb_lib::Error) -> ApiError {
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

#[derive(serde::Deserialize)]
pub struct DecodeInvoiceReq {
    pub invoice: String,
}

pub async fn decode_invoice(
    req: Json<DecodeInvoiceReq>,
) -> Result<Json<rgb_lib::wallet::InvoiceData>, ApiError> {
    let invoice = rgb_lib::wallet::Invoice::new(req.0.invoice).map_err(map_rgb_error)?;
    Ok(Json(invoice.invoice_data()))
}

#[cfg(feature = "server-signing")]
const HEADER_SIGNING_TOKEN: &str = "x-signing-token";

#[cfg(feature = "server-signing")]
/// The routes that keep keys and sign in the node are for private deployments:
/// they need the switch on and the shared secret in `x-signing-token`.
fn check_server_signing(
    ctx: &WalletCtx,
    headers: &actix_web::http::header::HeaderMap,
) -> Result<(), ApiError> {
    if !ctx.server_signing_enabled() {
        return Err(error_with(
            StatusCode::FORBIDDEN,
            ApiErrorCode::AccessDenied,
            "server-side signing is disabled (set allow_server_signing)",
        ));
    }
    let token = headers
        .get(HEADER_SIGNING_TOKEN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !ctx.signing_token_matches(token) {
        return Err(error_with(
            StatusCode::UNAUTHORIZED,
            ApiErrorCode::AccessDenied,
            "missing or wrong x-signing-token header",
        ));
    }
    Ok(())
}

#[cfg(feature = "server-signing")]
/// Runs before the other extractors, so a refused caller never makes the
/// server read a body or look at the wallet headers.
pub async fn signing_gate(
    req: actix_web::dev::ServiceRequest,
    next: actix_web::middleware::Next<impl actix_web::body::MessageBody + 'static>,
) -> Result<actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>, actix_web::Error> {
    let ctx = req
        .app_data::<Data<WalletCtx>>()
        .ok_or_else(|| actix_web::error::ErrorInternalServerError("no wallet context"))?;
    check_server_signing(ctx, req.headers())?;
    next.call(req).await
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

/// Both filters are optional. With neither, every transfer is listed. With
/// both, a transfer must match both. Unknown fields are an error, so a typo
/// does not silently list everything.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListTransfersReq {
    #[serde(default)]
    pub asset_id: Option<String>,
    /// Only the transfers of this batch transfer.
    #[serde(default)]
    pub batch_transfer_idx: Option<i32>,
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
pub struct SendBtcRes {
    /// TXID of the broadcast transaction.
    pub txid: String,
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
    req: Json<ListTransfersReq>,
) -> Result<Json<Vec<rgb_lib::wallet::Transfer>>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;

    let list = ctx
        .transfers_filtered(req.asset_id.clone(), req.batch_transfer_idx)
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

pub async fn issue_nia(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<IssueNiaReq>,
) -> Result<Json<rgb_lib::wallet::AssetNIA>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let asset = ctx.issue_nia_token(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(asset))
}

pub async fn issue_ifa(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<IssueIfaReq>,
) -> Result<Json<rgb_lib::wallet::AssetIFA>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let asset = ctx.issue_ifa_token(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(asset))
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

pub async fn mint_begin(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<MintBeginReq>,
) -> Result<Json<rgb_lib::wallet::MintBeginResult>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    Ok(Json(ctx.mint_begin(req.0).await.map_err(map_rgb_error)?))
}

pub async fn mint_end(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<Psbt>,
) -> Result<Json<SendResult>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let res = ctx
        .mint_end(req.0.signed_psbt)
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(SendResult {
        txid: res.txid,
        batch_transfer_idx: res.batch_transfer_idx,
    }))
}

pub async fn link_begin(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<LinkBeginReq>,
) -> Result<Json<rgb_lib::wallet::LinkBeginResult>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    Ok(Json(ctx.link_begin(req.0).await.map_err(map_rgb_error)?))
}

pub async fn link_end(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<Psbt>,
) -> Result<Json<SendResult>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let res = ctx
        .link_end(req.0.signed_psbt)
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(SendResult {
        txid: res.txid,
        batch_transfer_idx: res.batch_transfer_idx,
    }))
}

pub async fn inflate_begin(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<InflateBeginReq>,
) -> Result<Json<PsbtRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let psbt = ctx.inflate_begin(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(PsbtRes { psbt }))
}

pub async fn inflate_end(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<Psbt>,
) -> Result<Json<SendResult>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let res = ctx
        .inflate_end(req.0.signed_psbt)
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(SendResult {
        txid: res.txid,
        batch_transfer_idx: res.batch_transfer_idx,
    }))
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

pub async fn burn_begin(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<BurnBeginReq>,
) -> Result<Json<PsbtRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let psbt = ctx.burn_begin(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(PsbtRes { psbt }))
}

pub async fn burn_end(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<Psbt>,
) -> Result<Json<SendResult>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let res = ctx
        .burn_end(req.signed_psbt.clone())
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(SendResult {
        txid: res.txid,
        batch_transfer_idx: res.batch_transfer_idx,
    }))
}

#[cfg(feature = "server-signing")]
/// Body of `/wallet/init`. The mnemonic is optional: without it the node makes one.
#[derive(serde::Deserialize, Default)]
struct InitReq {
    #[serde(default)]
    mnemonic: Option<String>,
}

#[cfg(feature = "server-signing")]
/// Make a wallet that keeps its keys in the node's memory and signs there.
/// With `mnemonic` it restores that wallet (call it again after a restart),
/// without one it makes new keys. The response holds the mnemonic and the three
/// values for the `xpub-van`, `xpub-col` and `master-fingerprint` headers.
pub async fn init(
    ctx: Data<WalletCtx>,
    body: actix_web::web::Bytes,
) -> Result<actix_web::HttpResponse, ApiError> {
    // No body at all means "make new keys". A body that does not parse is an
    // error, never a silent switch to new keys.
    let req: InitReq = if body.iter().all(u8::is_ascii_whitespace) {
        InitReq::default()
    } else {
        serde_json::from_slice(&body).map_err(|_| {
            error_with(
                StatusCode::BAD_REQUEST,
                ApiErrorCode::BadInput,
                "body must be a JSON object with an optional string field: mnemonic",
            )
        })?
    };
    let keys = ctx
        .init_full_wallet(req.mnemonic)
        .await
        .map_err(map_rgb_error)?;
    // The body holds a mnemonic: no cache may keep it.
    Ok(actix_web::HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
        .json(keys))
}

#[cfg(feature = "server-signing")]
/// Send assets in one call: begin, sign inside the node, end.
pub async fn send(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<SendBeginReq>,
) -> Result<Json<SendResult>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let res = ctx.send_signed(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(SendResult {
        txid: res.txid,
        batch_transfer_idx: res.batch_transfer_idx,
    }))
}

#[cfg(feature = "server-signing")]
/// Burn assets in one call: begin, sign inside the node, end.
pub async fn burn(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<BurnBeginReq>,
) -> Result<Json<SendResult>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let res = ctx.burn_signed(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(SendResult {
        txid: res.txid,
        batch_transfer_idx: res.batch_transfer_idx,
    }))
}

#[cfg(feature = "server-signing")]
/// Inflate an IFA asset in one call: begin, sign inside the node, end.
pub async fn inflate(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<InflateBeginReq>,
) -> Result<Json<SendResult>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let res = ctx.inflate_signed(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(SendResult {
        txid: res.txid,
        batch_transfer_idx: res.batch_transfer_idx,
    }))
}

#[cfg(feature = "server-signing")]
/// Send BTC in one call: begin, sign inside the node, end.
pub async fn send_btc(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<SendBtcReq>,
) -> Result<Json<SendBtcRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let txid = ctx.send_btc_signed(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(SendBtcRes { txid }))
}

#[cfg(feature = "server-signing")]
/// Create UTXOs in one call: begin, sign inside the node, end.
pub async fn create_utxos(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<CreateUtxoBeginReq>,
) -> Result<Json<CreateUtxosRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let created = ctx
        .create_utxos_signed(req.0)
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(CreateUtxosRes {
        created: usize::from(created),
    }))
}

pub async fn get_asset_metadata(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<AssetId>,
) -> Result<Json<rgb_lib::wallet::Metadata>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let metadata = ctx.asset_info(&req.asset_id).await.map_err(map_rgb_error)?;
    Ok(Json(metadata))
}

pub async fn get_received_mint(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<GetReceivedMintReq>,
) -> Result<Json<GetReceivedMintRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let mint = ctx
        .get_received_mint(req.0.recipient_id)
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(GetReceivedMintRes { mint }))
}

pub async fn inspect_psbt(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<InspectPsbtReq>,
) -> Result<Json<rgb_lib::wallet::PsbtInspection>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let inspection = ctx.inspect_psbt(req.0.psbt).await.map_err(map_rgb_error)?;
    Ok(Json(inspection))
}

pub async fn get_consignment(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<GetConsignmentReq>,
) -> Result<Json<GetConsignmentRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let res = ctx.get_consignment(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(res))
}

pub async fn blind_receive(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<ReceiveReq>,
) -> Result<Json<rgb_lib::wallet::ReceiveData>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let res = ctx
        .receive_token(ReceiveKind::Blind, req.0)
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(res))
}

pub async fn witness_receive(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<ReceiveReq>,
) -> Result<Json<rgb_lib::wallet::ReceiveData>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let res = ctx
        .receive_token(ReceiveKind::Witness, req.0)
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(res))
}

pub async fn send_btc_begin(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<SendBtcReq>,
) -> Result<Json<PsbtRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let psbt = ctx.send_btc_begin(req.0).await.map_err(map_rgb_error)?;
    Ok(Json(PsbtRes { psbt }))
}

pub async fn send_btc_end(
    ctx: Data<WalletCtx>,
    wallet_key: XWalletKey,
    req: Json<Psbt>,
) -> Result<Json<SendBtcRes>, ApiError> {
    let ctx = derive_wctx(&ctx, wallet_key).await?;
    let txid = ctx
        .send_btc_end(req.0.signed_psbt)
        .await
        .map_err(map_rgb_error)?;
    Ok(Json(SendBtcRes { txid }))
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
            serde_json::to_string(&SendBtcRes {
                txid: "f00d".to_string(),
            })
            .unwrap(),
            serde_json::to_string(&FailTransferRes { changed: true }).unwrap(),
            serde_json::to_string(&AddressRes {
                address: "bcrt1qxy".to_string(),
            })
            .unwrap(),
            serde_json::to_string(&EmptyRes {}).unwrap(),
            serde_json::to_string(&GetReceivedMintRes { mint: None }).unwrap(),
        ];

        assert_eq!(
            bodies,
            [
                r#"{"psbt":"cHNidP8B"}"#,
                r#"{"created":3}"#,
                r#"{"txid":"f00d"}"#,
                r#"{"changed":true}"#,
                r#"{"address":"bcrt1qxy"}"#,
                r#"{}"#,
                r#"{"mint":null}"#,
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
