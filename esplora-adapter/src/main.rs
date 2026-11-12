mod config;
mod electrum;
mod limit;
mod routes;
mod types;

use actix_cors::Cors;
use actix_web::body::MessageBody;
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::middleware::Next;
use actix_web::{App, Error, HttpResponse, HttpServer, middleware, web};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about = "Esplora REST facade over an Electrum server")]
struct Args {
    /// path to config file (optional; ESPLORA_<FIELD> env vars override it)
    #[arg(short, long, default_value_t = String::from("esplora-adapter.toml"))]
    config: String,
}

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    env_logger::init();

    let cfg = config::Config::load(&args.config)?;
    let state = web::Data::new(routes::State {
        backend: electrum::Backend::new(
            cfg.electrum_address.clone(),
            cfg.electrum_connections,
            cfg.tx_cache_size,
        ),
        net: cfg.net()?,
        permits: std::sync::Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrent.max(1))),
        timeout: std::time::Duration::from_secs(cfg.request_timeout_secs.max(1)),
    });
    let limiter = web::Data::new(limit::Limiter::new(
        cfg.rate_limit_per_sec,
        cfg.rate_limit_burst,
    ));
    let trust_xff = cfg.trust_forwarded_for;
    if cfg.enable_cors && cfg.cors_domain.is_empty() {
        log::warn!("CORS allows any origin; set cors_domain before exposing this publicly");
    }

    let addr = format!("{}:{}", cfg.listen_address, cfg.port);
    log::info!(
        "esplora-adapter on http://{addr} -> {}",
        cfg.electrum_address
    );

    HttpServer::new(move || {
        let cors = if !cfg.enable_cors {
            Cors::default()
        } else if cfg.cors_domain.is_empty() {
            Cors::permissive()
        } else {
            Cors::default()
                .allowed_origin(&cfg.cors_domain)
                .allow_any_method()
                .allow_any_header()
        };
        App::new()
            .app_data(state.clone())
            .app_data(limiter.clone())
            .app_data(web::PayloadConfig::new(8 * 1024 * 1024))
            .wrap(middleware::Logger::default())
            .wrap(middleware::from_fn(move |req, next| {
                rate_limit(trust_xff, req, next)
            }))
            .wrap(cors)
            .configure(routes::configure)
    })
    .bind(addr)?
    .run()
    .await?;
    Ok(())
}

/// Per-IP rate limit. Runs inside CORS so refusals still carry CORS headers.
async fn rate_limit(
    trust_xff: bool,
    req: ServiceRequest,
    next: Next<impl MessageBody + 'static>,
) -> Result<ServiceResponse<impl MessageBody>, Error> {
    let ip = if trust_xff {
        req.headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse().ok())
    } else {
        None
    }
    .or_else(|| req.peer_addr().map(|a| a.ip()));
    if let (Some(ip), Some(lim)) = (ip, req.app_data::<web::Data<limit::Limiter>>())
        && let Err(wait) = lim.check(ip)
    {
        let resp = HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", wait.to_string()))
            .body("rate limit exceeded");
        return Err(actix_web::error::InternalError::from_response("rate limited", resp).into());
    }
    next.call(req).await
}
