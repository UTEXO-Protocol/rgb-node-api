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
        "uwallet"
    }

    fn service(&self) -> Scope {
        scope("")
            .app_data(Data::new(self.ctx.clone()))
            .service(resource("/healthcheck").route(get().to(healthcheck)))
            .service(resource("/version").route(get().to(version)))
            .service(resource("/_swagger").route(get().to(swagger::ui)))
            .service(resource("/_swagger/swagger.yaml").route(get().to(swagger::spec)))
            .service(
                scope("/wallet")
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
                    .service(resource("/issuenia").route(post().to(node_api::issue_nia)))
                    .service(resource("/assetbalance").route(post().to(node_api::asset_balance)))
                    .service(resource("/sendbegin").route(post().to(node_api::send_begin)))
                    .service(resource("/sendend").route(post().to(node_api::send_end)))
                    .service(resource("/blindreceive").route(post().to(node_api::blind_receive)))
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
