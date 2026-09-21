// DYNAMIC_EMBEDDED_POC: only SendRequest/SendView, /sends routes and send handlers are Dynamic-only.
// Registration, witness invoices, refresh and ownership checks are shared with Vault.
use crate::{
    api_core::api_errors::{ApiError, ApiErrorCode, error_with, internal_server_error},
    mpc::{
        MpcError, MpcService, Owner, Registration, SendRequest, SendView, WalletView,
        WitnessInvoice, WitnessRequest,
    },
};
use actix_web::{
    FromRequest, HttpRequest, Scope,
    dev::Payload,
    http::StatusCode,
    web::{self, Data, Json, Path, Query},
};
use std::future::{Ready, ready};
use uuid::Uuid;

fn map_error(error: MpcError) -> ApiError {
    match error {
        MpcError::Disabled => error_with(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::ServiceUnavailable,
            "MPC service is disabled",
        ),
        MpcError::Unauthorized => error_with(
            StatusCode::UNAUTHORIZED,
            ApiErrorCode::AccessDenied,
            "Gateway service authorization required",
        ),
        MpcError::NotFound => error_with(
            StatusCode::NOT_FOUND,
            ApiErrorCode::NotFound,
            "Wallet not found",
        ),
        MpcError::Invalid(message) => {
            error_with(StatusCode::BAD_REQUEST, ApiErrorCode::BadInput, message)
        }
        MpcError::Conflict(message) => {
            error_with(StatusCode::CONFLICT, ApiErrorCode::Conflict, message)
        }
        MpcError::Internal => internal_server_error(),
        MpcError::Rgb(error) => super::node_api::map_rgb_error(error),
    }
}

struct GatewayCaller(Owner);
impl FromRequest for GatewayCaller {
    type Error = ApiError;
    type Future = Ready<Result<Self, Self::Error>>;
    fn from_request(request: &HttpRequest, _: &mut Payload) -> Self::Future {
        let result = (|| {
            let service = request
                .app_data::<Data<MpcService>>()
                .ok_or_else(internal_server_error)?;
            let header = |name: &str| {
                request
                    .headers()
                    .get(name)
                    .and_then(|value| value.to_str().ok())
            };
            let owner = Owner {
                tenant_id: header("x-tenant-id").unwrap_or_default().into(),
                user_id: header("x-user-id").unwrap_or_default().into(),
            };
            service
                .authorize(header("authorization"), owner)
                .map(GatewayCaller)
                .map_err(map_error)
        })();
        ready(result)
    }
}

fn wallet_id(path: Path<String>) -> Result<Uuid, ApiError> {
    Uuid::parse_str(&path).map_err(|_| map_error(MpcError::Invalid("Invalid wallet UUID")))
}

pub(super) fn scope(service: MpcService) -> Scope {
    web::scope("/internal/mpc")
        .app_data(Data::new(service))
        .route("/wallets", web::post().to(register))
        .route("/wallets/{wallet_id}", web::get().to(wallet))
        .route(
            "/wallets/{wallet_id}/witness-invoices",
            web::post().to(witness),
        )
        .route("/wallets/{wallet_id}/assets", web::get().to(assets))
        .route("/wallets/{wallet_id}/transfers", web::get().to(transfers))
        .route("/wallets/{wallet_id}/refresh", web::post().to(refresh))
        // DYNAMIC_EMBEDDED_POC BEGIN: return transaction routes.
        .route(
            "/wallets/{wallet_id}/sends/prepare",
            web::post().to(prepare_send),
        )
        .route(
            "/wallets/{wallet_id}/sends/{request_id}",
            web::get().to(send_status),
        )
        .route(
            "/wallets/{wallet_id}/sends/{request_id}/finish",
            web::post().to(finish_send),
        )
        .route(
            "/wallets/{wallet_id}/sends/{request_id}/cancel",
            web::post().to(cancel_send),
        )
    // DYNAMIC_EMBEDDED_POC END
}

// DYNAMIC_EMBEDDED_POC BEGIN: constrained return handlers.
async fn prepare_send(
    service: Data<MpcService>,
    GatewayCaller(owner): GatewayCaller,
    id: Path<String>,
    request: Json<SendRequest>,
) -> Result<Json<SendView>, ApiError> {
    Ok(Json(
        service
            .prepare_send(owner, wallet_id(id)?, request.0)
            .await
            .map_err(map_error)?,
    ))
}
fn send_ids(path: Path<(String, String)>) -> Result<(Uuid, Uuid), ApiError> {
    let (wallet, request) = path.into_inner();
    Ok((
        Uuid::parse_str(&wallet)
            .map_err(|_| map_error(MpcError::Invalid("Invalid wallet UUID")))?,
        Uuid::parse_str(&request)
            .map_err(|_| map_error(MpcError::Invalid("Invalid request UUID")))?,
    ))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FinishSend {
    signed_psbt: String,
}
async fn finish_send(
    service: Data<MpcService>,
    GatewayCaller(owner): GatewayCaller,
    path: Path<(String, String)>,
    request: Json<FinishSend>,
) -> Result<Json<SendView>, ApiError> {
    let (id, request_id) = send_ids(path)?;
    Ok(Json(
        service
            .finish_send(owner, id, request_id, request.signed_psbt.clone())
            .await
            .map_err(map_error)?,
    ))
}
async fn cancel_send(
    service: Data<MpcService>,
    GatewayCaller(owner): GatewayCaller,
    path: Path<(String, String)>,
) -> Result<Json<SendView>, ApiError> {
    let (id, request_id) = send_ids(path)?;
    Ok(Json(
        service
            .cancel_send(owner, id, request_id)
            .await
            .map_err(map_error)?,
    ))
}
async fn send_status(
    service: Data<MpcService>,
    GatewayCaller(owner): GatewayCaller,
    path: Path<(String, String)>,
) -> Result<Json<SendView>, ApiError> {
    let (id, request_id) = send_ids(path)?;
    Ok(Json(
        service
            .send_status(owner, id, request_id)
            .await
            .map_err(map_error)?,
    ))
}

// DYNAMIC_EMBEDDED_POC END

async fn register(
    service: Data<MpcService>,
    GatewayCaller(owner): GatewayCaller,
    request: Json<Registration>,
) -> Result<Json<WalletView>, ApiError> {
    Ok(Json(
        service
            .register(owner, request.0)
            .await
            .map_err(map_error)?,
    ))
}
async fn wallet(
    service: Data<MpcService>,
    GatewayCaller(owner): GatewayCaller,
    id: Path<String>,
) -> Result<Json<WalletView>, ApiError> {
    Ok(Json(
        service
            .wallet(owner, wallet_id(id)?)
            .await
            .map_err(map_error)?,
    ))
}
async fn witness(
    service: Data<MpcService>,
    GatewayCaller(owner): GatewayCaller,
    id: Path<String>,
    request: Json<WitnessRequest>,
) -> Result<Json<WitnessInvoice>, ApiError> {
    Ok(Json(
        service
            .witness(owner, wallet_id(id)?, request.0)
            .await
            .map_err(map_error)?,
    ))
}
async fn assets(
    service: Data<MpcService>,
    GatewayCaller(owner): GatewayCaller,
    id: Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let assets = service
        .assets(owner, wallet_id(id)?)
        .await
        .map_err(map_error)?;
    let assets: Vec<_> = assets
        .nia
        .unwrap_or_default()
        .into_iter()
        .map(|asset| {
            serde_json::json!({
                "asset_id":asset.asset_id, "schema":"nia", "ticker":asset.ticker, "name":asset.name,
                "precision":asset.precision, "balance": balance_view(&asset.balance)
            })
        })
        .collect();
    Ok(Json(serde_json::json!({"assets":assets})))
}
fn balance_view(balance: &rgb_lib::wallet::Balance) -> serde_json::Value {
    serde_json::json!({"settled":balance.settled.to_string(), "future":balance.future.to_string(), "spendable":balance.spendable.to_string()})
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferQuery {
    asset_id: Option<String>,
}

async fn transfers(
    service: Data<MpcService>,
    GatewayCaller(owner): GatewayCaller,
    id: Path<String>,
    query: Query<TransferQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let asset_id = query.into_inner().asset_id;
    let transfers: Vec<_> = service
        .transfers_for_asset(owner, wallet_id(id)?, asset_id.clone())
        .await
        .map_err(map_error)?
        .into_iter()
        .map(transfer_view)
        .collect();
    Ok(Json(
        serde_json::json!({"transfers":transfers, "asset_id":asset_id}),
    ))
}

fn transfer_view(transfer: rgb_lib::wallet::Transfer) -> serde_json::Value {
    // Outgoing assignments describe our change. The recipient amount is stored
    // separately; incoming amounts must instead reflect validated allocations.
    let assignments = if transfer.kind == rgb_lib::wallet::TransferKind::Send {
        transfer.requested_assignment.as_slice()
    } else {
        &transfer.assignments
    };
    let amounts: Vec<_> = assignments
        .iter()
        .filter_map(|assignment| match assignment {
            rgb_lib::Assignment::Fungible(amount) => Some(amount.to_string()),
            _ => None,
        })
        .collect();
    serde_json::json!({
        "idx": transfer.idx,
        "batch_transfer_idx": transfer.batch_transfer_idx,
        "created_at": transfer.created_at,
        "status": transfer.status,
        "kind": transfer.kind,
        "txid": transfer.txid,
        "recipient_id": transfer.recipient_id,
        "proxy_recipient_id": transfer.proxy_recipient_id,
        "amounts": amounts,
        "expiration_timestamp": transfer.expiration_timestamp,
    })
}

async fn refresh(
    service: Data<MpcService>,
    GatewayCaller(owner): GatewayCaller,
    id: Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    service
        .refresh(owner, wallet_id(id)?)
        .await
        .map_err(map_error)?;
    Ok(Json(serde_json::json!({"refreshed": true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{App, test};

    #[actix_web::test]
    async fn internal_routes_require_a_service_token_and_gateway_identity() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::wallet::Config {
            data_dir: dir.path().to_string_lossy().into(),
            network: "regtest".into(),
            ..Default::default()
        };
        let token = "test-only-service-token-with-32-bytes";
        let service = MpcService::new(config, Some(token.into())).unwrap();
        let app = test::init_service(App::new().service(scope(service))).await;
        let path = format!("/internal/mpc/wallets/{}", Uuid::new_v4());
        for authorization in [None, Some("Bearer wrong")] {
            let mut request = test::TestRequest::get().uri(&path);
            if let Some(value) = authorization {
                request = request.insert_header(("authorization", value));
            }
            assert_eq!(
                test::call_service(&app, request.to_request())
                    .await
                    .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        let request = test::TestRequest::get()
            .uri(&path)
            .insert_header(("authorization", format!("Bearer {token}")))
            .to_request();
        assert_eq!(
            test::call_service(&app, request).await.status(),
            StatusCode::BAD_REQUEST
        );
        let request = test::TestRequest::get()
            .uri(&path)
            .insert_header(("authorization", format!("Bearer {token}")))
            .insert_header(("x-tenant-id", "poc"))
            .insert_header(("x-user-id", "alice"))
            .to_request();
        assert_eq!(
            test::call_service(&app, request).await.status(),
            StatusCode::NOT_FOUND
        );
        let request = test::TestRequest::post()
            .uri("/internal/mpc/wallets")
            .insert_header(("authorization", format!("Bearer {token}")))
            .insert_header(("x-tenant-id", "poc"))
            .insert_header(("x-user-id", "alice"))
            .set_json(serde_json::json!({"mnemonic":"must-never-be-accepted"}))
            .to_request();
        assert_eq!(
            test::call_service(&app, request).await.status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[actix_web::test]
    async fn amounts_are_decimal_strings_without_javascript_precision_loss() {
        let value = balance_view(&rgb_lib::wallet::Balance {
            settled: u64::MAX,
            future: 9_007_199_254_740_993,
            spendable: 0,
        });
        assert_eq!(value["settled"], "18446744073709551615");
        assert_eq!(value["future"], "9007199254740993");
        assert_eq!(value["spendable"], "0");
    }

    fn transfer_fixture(kind: rgb_lib::wallet::TransferKind) -> rgb_lib::wallet::Transfer {
        serde_json::from_value(serde_json::json!({
            "idx": 1, "batch_transfer_idx": 1, "created_at": 1, "updated_at": 1,
            "status": "Settled", "kind": kind,
            "requested_assignment": {"Fungible": 1},
            "assignments": [{"Fungible": 2}],
            "transport_endpoints": [],
            "consignment_path": "/private/wallet/consignment",
            "psbt_path": "/private/wallet/unsigned.psbt",
        }))
        .unwrap()
    }

    #[actix_web::test]
    async fn outgoing_history_reports_recipient_amount_instead_of_change() {
        let mut transfer = transfer_fixture(rgb_lib::wallet::TransferKind::Send);
        assert_eq!(
            transfer_view(transfer.clone())["amounts"],
            serde_json::json!(["1"])
        );
        transfer.requested_assignment = Some(rgb_lib::Assignment::Fungible(u64::MAX));
        let view = transfer_view(transfer);
        assert_eq!(view["amounts"], serde_json::json!(["18446744073709551615"]));
        assert!(view.get("consignment_path").is_none());
        assert!(view.get("psbt_path").is_none());
    }

    #[actix_web::test]
    async fn incoming_history_reports_validated_amount_instead_of_invoice_request() {
        for kind in [
            rgb_lib::wallet::TransferKind::ReceiveBlind,
            rgb_lib::wallet::TransferKind::ReceiveWitness,
        ] {
            assert_eq!(
                transfer_view(transfer_fixture(kind))["amounts"],
                serde_json::json!(["2"])
            );
        }
    }

    #[actix_web::test]
    async fn outgoing_history_does_not_substitute_change_for_an_unknown_amount() {
        let mut transfer = transfer_fixture(rgb_lib::wallet::TransferKind::Send);
        transfer.requested_assignment = None;
        assert_eq!(transfer_view(transfer)["amounts"], serde_json::json!([]));
    }
}
