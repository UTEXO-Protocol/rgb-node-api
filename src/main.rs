use clap::Parser;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use rgb_node_api::WalletSrv;
use rgb_node_api::api_core::server;
use rgb_node_api::wallet;
use rgb_node_api::wallet::*;

#[derive(Debug, Clone, serde::Deserialize)]
struct Config {
    #[serde(default)]
    pub api: server::Config,
    pub wallet: wallet::Config,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// path to config file
    #[arg(short, long, default_value_t = String::from("config.toml"))]
    config: String,
}

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    env_logger::init();

    let contents = std::fs::read_to_string(args.config)?;
    let cfg: Config = toml::from_str(&contents)?;
    cfg.wallet.net()?;

    let tasker = TaskTracker::new();
    let cancel = CancellationToken::new();

    // Watch-only wallets are registered at runtime via POST /wallet/register
    // (xpub headers); this service never holds mnemonics or signs transactions.
    let ctx = WalletCtx::new(cfg.wallet.clone(), &tasker);

    let mpc = rgb_node_api::mpc::MpcService::new(
        cfg.wallet.clone(),
        std::env::var("RGB_MPC_SERVICE_TOKEN").ok(),
    )?;
    let api_service = WalletSrv { ctx, mpc };
    log::info!("Spawn api jobs");
    tasker.spawn(api_service.spawn_jobs(cancel.clone()));

    log::info!("Run HTTP server");
    server::run_server(cfg.api, cancel.clone(), api_service, None).await?;

    tasker.close();
    cancel.cancel();

    log::info!("Halting indexer API");
    tasker.wait().await;
    log::info!("Application successfully shut down");

    Ok(())
}
