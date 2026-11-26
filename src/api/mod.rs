use crate::api_core::server::ApiService;
use crate::wallet::*;
use actix_web::Scope;
use actix_web::web::{Data, get, post, resource, scope};
use tokio_util::sync::CancellationToken;

mod node_api;
mod swagger;

#[derive(Clone)]
pub struct WalletSrv {
    pub ctx: WalletCtx,
}

impl WalletSrv {
    pub fn spawn_jobs(&self, cancel: CancellationToken) {
        // This task don't have any persistent state.
        // So, we don't care about gracefull shutdown of this task.
        let ctx = self.ctx.clone();

        log::info!("refresh wallet state");
        tokio::spawn(async move { ctx.refresh_task(cancel).await });
    }
}

impl ApiService for WalletSrv {
    fn name(&self) -> &'static str {
        "rgbnodeapi"
    }

    fn service(&self) -> Scope {
        scope("")
            .app_data(Data::new(self.ctx.clone()))
            .service(resource("/healthcheck").route(get().to(healthcheck)))
            .service(resource("/version").route(get().to(version)))
            .service(resource("/_swagger").route(get().to(swagger::ui)))
            .service(resource("/_swagger/swagger.yaml").route(get().to(swagger::spec)))
            // remove this in future
            .service(resource("/blindreceive").route(post().to(node_api::blind_receive)))
            .service(
                signing_routes(scope("/wallet"))
                    .service(resource("/decodeinvoice").route(post().to(node_api::decode_invoice)))
                    .service(resource("/register").route(post().to(node_api::register)))
                    .service(resource("/listunspents").route(post().to(node_api::list_unspents)))
                    .service(
                        resource("/createutxosbegin")
                            .route(post().to(node_api::create_utxos_begin)),
                    )
                    .service(
                        resource("/createutxosend").route(post().to(node_api::create_utxos_end)),
                    )
                    .service(resource("/listassets").route(post().to(node_api::list_assets)))
                    .service(resource("/btcbalance").route(post().to(node_api::btc_balance)))
                    .service(resource("/address").route(post().to(node_api::address)))
                    .service(resource("/issueassetnia").route(post().to(node_api::issue_nia)))
                    .service(resource("/issueassetifa").route(post().to(node_api::issue_ifa)))
                    .service(resource("/issueassetbfa").route(post().to(node_api::issue_bfa)))
                    .service(resource("/mintbegin").route(post().to(node_api::mint_begin)))
                    .service(resource("/mintend").route(post().to(node_api::mint_end)))
                    .service(resource("/linkbegin").route(post().to(node_api::link_begin)))
                    .service(resource("/linkend").route(post().to(node_api::link_end)))
                    .service(resource("/inflatebegin").route(post().to(node_api::inflate_begin)))
                    .service(resource("/inflateend").route(post().to(node_api::inflate_end)))
                    .service(resource("/assetbalance").route(post().to(node_api::asset_balance)))
                    .service(
                        resource("/getassetmetadata")
                            .route(post().to(node_api::get_asset_metadata)),
                    )
                    .service(
                        resource("/getreceivedmint").route(post().to(node_api::get_received_mint)),
                    )
                    .service(resource("/inspectpsbt").route(post().to(node_api::inspect_psbt)))
                    .service(resource("/sendbegin").route(post().to(node_api::send_begin)))
                    .service(resource("/sendend").route(post().to(node_api::send_end)))
                    .service(resource("/burnbegin").route(post().to(node_api::burn_begin)))
                    .service(resource("/burnend").route(post().to(node_api::burn_end)))
                    .service(
                        resource("/getconsignment").route(post().to(node_api::get_consignment)),
                    )
                    .service(resource("/blindreceive").route(post().to(node_api::blind_receive)))
                    .service(
                        resource("/witnessreceive").route(post().to(node_api::witness_receive)),
                    )
                    .service(resource("/sendbtcbegin").route(post().to(node_api::send_btc_begin)))
                    .service(resource("/sendbtcend").route(post().to(node_api::send_btc_end)))
                    .service(resource("/failtransfers").route(post().to(node_api::fail_transfers)))
                    .service(
                        resource("/listtransactions").route(post().to(node_api::list_transactions)),
                    )
                    .service(resource("/listtransfers").route(post().to(node_api::list_transfers)))
                    .service(resource("/refresh").route(post().to(node_api::refresh)))
                    .service(resource("/drop").route(post().to(node_api::drop))),
            )
    }
}

async fn healthcheck() -> impl actix_web::Responder {
    actix_web::HttpResponse::Ok().finish()
}

async fn version() -> impl actix_web::Responder {
    let info = get_app_info();
    actix_web::HttpResponse::Ok().json(info)
}

#[derive(serde::Serialize)]
pub struct AppInfo {
    pub app: &'static str,
    pub version: &'static str,
    pub build: &'static str,
    pub commit: &'static str,
}

fn get_app_info() -> AppInfo {
    const APP: &str = env!("CARGO_CRATE_NAME");
    const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

    #[inline]
    fn git_version() -> &'static str {
        option_env!("GIT_VERSION").unwrap_or("n/a")
    }

    #[inline]
    fn git_commit() -> &'static str {
        option_env!("GIT_COMMIT").unwrap_or("n/a")
    }

    AppInfo {
        app: APP,
        version: PKG_VERSION,
        build: git_version(),
        commit: git_commit(),
    }
}

#[cfg(feature = "server-signing")]
/// Routes that keep keys in the node and sign there. Only built with the
/// `server-signing` feature. Each one is behind the signing gate.
fn signing_routes(wallet: actix_web::Scope) -> actix_web::Scope {
    use actix_web::middleware::from_fn;
    let gated = |path: &str, handler: actix_web::Route| {
        resource(path)
            .wrap(from_fn(node_api::signing_gate))
            .route(handler)
    };
    wallet
        .service(gated("/init", post().to(node_api::init)))
        .service(gated("/send", post().to(node_api::send)))
        .service(gated("/burn", post().to(node_api::burn)))
        .service(gated("/inflate", post().to(node_api::inflate)))
        .service(gated("/sendbtc", post().to(node_api::send_btc)))
        .service(gated("/createutxos", post().to(node_api::create_utxos)))
}

#[cfg(not(feature = "server-signing"))]
fn signing_routes(wallet: actix_web::Scope) -> actix_web::Scope {
    wallet
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_core::api_errors::ApiError;
    use actix_web::{App, http::StatusCode, test, web::JsonConfig};
    use serde_json::{Value, json};
    use tokio_util::task::TaskTracker;

    #[actix_web::test]
    async fn decode_invoice_without_wallet_headers() {
        let tasks = TaskTracker::new();
        let ctx = WalletCtx::new(Config::default(), &tasks);
        let app =
            test::init_service(App::new().service(WalletSrv { ctx: ctx.clone() }.service())).await;
        let recipient = "bc:utxob:4vm1CX2Z-K8hMo59-e7dgGBS-Jka7mYn-Xe~yP85-yUiHHxr-aVlYa";
        let public_key = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
        let invoice = format!(
            "rgb:~/~/~/{recipient}?expiry=1&enc=1&pk={public_key}&endpoints=rpc://localhost:3000/json-rpc&order=test"
        );
        let req = test::TestRequest::post()
            .uri("/wallet/decodeinvoice")
            .set_json(json!({"invoice": invoice}))
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body: Value = test::read_body_json(res).await;
        assert_eq!(
            body,
            json!({
                "recipient_id": recipient,
                "asset_schema": null,
                "asset_id": null,
                "assignment": "Any",
                "assignment_name": null,
                "network": "Mainnet",
                "expiration_timestamp": 1,
                "transport_endpoints": ["rpc://localhost:3000/json-rpc"],
                "public_key": public_key,
                "encryption": "Optional",
                "unknown_query_params": {"order": "test"}
            })
        );
        ctx.stop().await.unwrap();
        tasks.close();
        tasks.wait().await;
    }

    #[cfg(feature = "server-signing")]
    /// The status of a response, or of the error a middleware or handler
    /// returned (the real server turns errors into responses).
    #[cfg(feature = "server-signing")]
    async fn status_of<S, R, B>(app: &S, req: R) -> StatusCode
    where
        S: actix_web::dev::Service<
                R,
                Response = actix_web::dev::ServiceResponse<B>,
                Error = actix_web::Error,
            >,
    {
        match test::try_call_service(app, req).await {
            Ok(res) => res.status(),
            Err(err) => err.error_response().status(),
        }
    }

    #[cfg(feature = "server-signing")]
    const SIGNING_PATHS: [&str; 6] = [
        "/wallet/init",
        "/wallet/send",
        "/wallet/burn",
        "/wallet/inflate",
        "/wallet/sendbtc",
        "/wallet/createutxos",
    ];

    #[cfg(feature = "server-signing")]
    #[actix_web::test]
    async fn signing_routes_are_off_by_default() {
        let tasks = TaskTracker::new();
        let ctx = WalletCtx::new(Config::default(), &tasks);
        let app =
            test::init_service(App::new().service(WalletSrv { ctx: ctx.clone() }.service())).await;
        for path in SIGNING_PATHS {
            // No wallet headers and a body that is not JSON: the gate answers
            // before anything is parsed, so this is a 403, not a 400/401/415.
            let req = test::TestRequest::post()
                .uri(path)
                .set_payload("not json")
                .to_request();
            assert_eq!(status_of(&app, req).await, StatusCode::FORBIDDEN, "{path}");
        }
        ctx.stop().await.unwrap();
        tasks.close();
        tasks.wait().await;
    }

    #[cfg(feature = "server-signing")]
    #[actix_web::test]
    async fn signing_routes_need_the_token_before_anything_is_parsed() {
        let tasks = TaskTracker::new();
        let cfg = Config {
            allow_server_signing: true,
            server_signing_token: Some("0123456789abcdef".into()),
            network: "regtest".into(),
            ..Config::default()
        };
        let ctx = WalletCtx::new(cfg, &tasks);
        let app =
            test::init_service(App::new().service(WalletSrv { ctx: ctx.clone() }.service())).await;
        for path in SIGNING_PATHS {
            for token in [None, Some("0123456789abcdeX"), Some("0123456789abcde")] {
                let mut req = test::TestRequest::post().uri(path).set_payload("not json");
                if let Some(token) = token {
                    req = req.insert_header(("x-signing-token", token));
                }
                assert_eq!(
                    status_of(&app, req.to_request()).await,
                    StatusCode::UNAUTHORIZED,
                    "{path} {token:?}"
                );
            }
        }
        ctx.stop().await.unwrap();
        tasks.close();
        tasks.wait().await;
    }

    #[cfg(feature = "server-signing")]
    #[actix_web::test]
    async fn init_rejects_bad_input_and_never_falls_back_to_new_keys() {
        let tasks = TaskTracker::new();
        let cfg = Config {
            allow_server_signing: true,
            server_signing_token: Some("0123456789abcdef".into()),
            network: "regtest".into(),
            ..Config::default()
        };
        let ctx = WalletCtx::new(cfg, &tasks);
        let app =
            test::init_service(App::new().service(WalletSrv { ctx: ctx.clone() }.service())).await;
        for payload in [
            "not json".to_string(),
            r#"{"mnemonic": 5}"#.to_string(),
            r#"{"mnemonic": "not a valid phrase"}"#.to_string(),
        ] {
            let req = test::TestRequest::post()
                .uri("/wallet/init")
                .insert_header(("x-signing-token", "0123456789abcdef"))
                .set_payload(payload.clone())
                .to_request();
            assert_eq!(
                status_of(&app, req).await,
                StatusCode::BAD_REQUEST,
                "{payload}"
            );
        }
        ctx.stop().await.unwrap();
        tasks.close();
        tasks.wait().await;
    }

    #[actix_web::test]
    async fn list_transfers_rejects_unknown_fields() {
        let tasks = TaskTracker::new();
        let ctx = WalletCtx::new(Config::default(), &tasks);
        let app = test::init_service(
            App::new()
                .app_data(JsonConfig::default().error_handler(|err, _| ApiError::from(err).into()))
                .service(WalletSrv { ctx: ctx.clone() }.service()),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/wallet/listtransfers")
            .insert_header(("xpub-van", "x"))
            .insert_header(("xpub-col", "x"))
            .insert_header(("master-fingerprint", "x"))
            .set_json(json!({"assetid": "typo"}))
            .to_request();
        let res = test::call_service(&app, req).await;
        assert!(res.status().is_client_error(), "{}", res.status());
        ctx.stop().await.unwrap();
        tasks.close();
        tasks.wait().await;
    }

    #[actix_web::test]
    async fn decode_invoice_rejects_invalid_input() {
        let tasks = TaskTracker::new();
        let ctx = WalletCtx::new(Config::default(), &tasks);
        let app = test::init_service(
            App::new()
                .app_data(JsonConfig::default().error_handler(|err, _| ApiError::from(err).into()))
                .service(WalletSrv { ctx: ctx.clone() }.service()),
        )
        .await;
        for (input, status, code) in [
            (json!({"invoice": "invalid"}), StatusCode::BAD_REQUEST, 1008),
            (json!({}), StatusCode::UNPROCESSABLE_ENTITY, 1001),
            (
                json!({"invoice": 42}),
                StatusCode::UNPROCESSABLE_ENTITY,
                1001,
            ),
        ] {
            let req = test::TestRequest::post()
                .uri("/wallet/decodeinvoice")
                .set_json(input)
                .to_request();
            let res = test::call_service(&app, req).await;
            assert_eq!(res.status(), status);
            let body: Value = test::read_body_json(res).await;
            assert_eq!(body["error"]["code"], code);
            if code == 1008 {
                assert_eq!(body["error"]["details"]["kind"], "InvalidInvoice");
            }
        }
        ctx.stop().await.unwrap();
        tasks.close();
        tasks.wait().await;
    }

    #[actix_web::test]
    async fn wallet_endpoints_require_headers_and_request_fields() {
        let tasks = TaskTracker::new();
        let ctx = WalletCtx::new(Config::default(), &tasks);
        let app = test::init_service(
            App::new()
                .app_data(JsonConfig::default().error_handler(|err, _| ApiError::from(err).into()))
                .service(WalletSrv { ctx: ctx.clone() }.service()),
        )
        .await;
        let headers = ["xpub-van", "xpub-col", "master-fingerprint"];
        for (path, field, input) in [
            (
                "/wallet/getassetmetadata",
                "asset_id",
                json!({"asset_id": "unused"}),
            ),
            (
                "/wallet/getreceivedmint",
                "recipient_id",
                json!({"recipient_id": "unused"}),
            ),
            ("/wallet/inspectpsbt", "psbt", json!({"psbt": "unused"})),
            (
                "/wallet/issueassetbfa",
                "ticker",
                json!({"ticker": "BFA", "name": "BFA", "mint_rights": 1, "chain_id": 31337, "contract_address": "unused"}),
            ),
            (
                "/wallet/mintbegin",
                "asset_id",
                json!({"asset_id": "unused", "before_block": 1000, "recipient": {"recipient_id": "unused", "assignment": {"Fungible": 1}, "transport_endpoints": []}}),
            ),
            (
                "/wallet/mintend",
                "signed_psbt",
                json!({"signed_psbt": "unused"}),
            ),
            (
                "/wallet/linkbegin",
                "asset_id",
                json!({"asset_id": "unused", "linked_to_contract": "unused"}),
            ),
            (
                "/wallet/linkend",
                "signed_psbt",
                json!({"signed_psbt": "unused"}),
            ),
        ] {
            for missing in headers {
                let mut req = test::TestRequest::post().uri(path);
                for header in headers {
                    if header != missing {
                        req = req.insert_header((header, "unused"));
                    }
                }
                let res = test::call_service(&app, req.set_json(&input).to_request()).await;
                assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{path}: {missing}");
                let body: Value = test::read_body_json(res).await;
                assert_eq!(body["error"]["code"], 401);
            }
            for input in [json!({}), json!({field: 42})] {
                let mut req = test::TestRequest::post().uri(path).set_json(input);
                for header in headers {
                    req = req.insert_header((header, "unused"));
                }
                let res = test::call_service(&app, req.to_request()).await;
                assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY, "{path}");
                let body: Value = test::read_body_json(res).await;
                assert_eq!(body["error"]["code"], 1001);
            }
        }
        ctx.stop().await.unwrap();
        tasks.close();
        tasks.wait().await;
    }
}
