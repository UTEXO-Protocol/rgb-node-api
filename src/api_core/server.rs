use std::str::FromStr;

use actix_cors::Cors;
use actix_web::dev::Service;
use actix_web::http::header;
use actix_web::http::header::HeaderName;
use actix_web::middleware::Condition;
use actix_web::{App, HttpResponse, HttpServer, Scope, http, middleware, web};
use actix_web_prom::PrometheusMetricsBuilder;
use prometheus::Registry;
use sentry_actix::Sentry;
use serde::{Deserialize, Serialize};
use tokio::select;
use tokio_util::sync::CancellationToken;

use super::api_errors::ApiError;

#[derive(Default, Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default = "defaults::listen_address")]
    pub listen_address: String,
    #[serde(default = "defaults::api_port")]
    pub port: u16,
    #[serde(default)]
    pub enable_cors: bool,
    #[serde(default)]
    pub cors_domain: String,
    #[serde(default)]
    pub allowed_headers: Vec<String>,
    #[serde(default = "defaults::json_size")]
    pub max_json_size: usize,
}

#[derive(Default, Serialize, Deserialize, Clone, Debug)]
pub struct MetricsConfig {
    #[serde(default)]
    pub enable: bool,
    #[serde(default = "defaults::listen_address")]
    pub listen_address: String,
    #[serde(default = "defaults::metrics_port")]
    pub port: u16,
    #[serde(default = "defaults::metrics_namespace")]
    pub namespace: String,
}

mod defaults {
    pub fn listen_address() -> String {
        "127.0.0.1".into()
    }

    pub fn api_port() -> u16 {
        3000
    }

    pub fn metrics_port() -> u16 {
        9140
    }

    pub fn metrics_namespace() -> String {
        "api".into()
    }

    pub fn json_size() -> usize {
        1024 * 1024 * 5
    }
}

pub trait ApiService: Clone + Send {
    fn name(&self) -> &'static str {
        "api"
    }
    fn service(&self) -> Scope;
}

pub async fn run_server<F>(
    config: Config,
    cancel: CancellationToken,
    api_service: F,
    registry: Option<Registry>,
) -> std::io::Result<()>
where
    F: ApiService + 'static,
{
    let host = format!("{}:{}", config.listen_address, config.port);
    let service_name = api_service.name();

    log::info!("Staring [{service_name}] server at http://{}", host.clone());

    let enable_metrics = registry.is_some();
    let metrics_middleware = if let Some(r) = registry {
        PrometheusMetricsBuilder::new(service_name)
            .registry(r)
            .build()
            .unwrap()
    } else {
        PrometheusMetricsBuilder::new(service_name).build().unwrap()
    };
    let max_json_size = if config.max_json_size > 0 {
        config.max_json_size
    } else {
        defaults::json_size()
    };

    let server = HttpServer::new(move || {
        App::new()
            .wrap(Sentry::new())
            .app_data(
                web::JsonConfig::default()
                    .limit(max_json_size)
                    .error_handler(|err, _| ApiError::from(err).into()),
            )
            .wrap(middleware::Logger::default())
            .wrap(Condition::new(enable_metrics, metrics_middleware.clone()))
            .wrap(Condition::new(
                config.enable_cors,
                cors(&config.cors_domain, &config.allowed_headers),
            ))
            .wrap(middleware::DefaultHeaders::new().add((header::CACHE_CONTROL, "no-store")))
            .wrap_fn(|req, srv| {
                let fut = srv.call(req);
                async {
                    let res = fut.await;
                    match res {
                        Ok(res) => Ok(res),
                        Err(err) => {
                            log::error!("catch response error: {err:?}");
                            Err(err)
                        }
                    }
                }
            })
            .service(api_service.service())
            .default_service(web::to(not_found))
    })
    .bind(host)?;

    select! {
        _ = cancel.cancelled() => {
            log::info!("[{service_name}] server received cancel signal");
        }
        res = server.run() => {
            res?;
        }
    }

    log::info!("[{service_name}] server stopped");
    Ok(())
}

async fn not_found() -> HttpResponse {
    super::api_errors::not_found().into()
}

pub fn cors(cors_domain: &str, allowed_headers: &[String]) -> Cors {
    let mut headers = vec![
        http::header::AUTHORIZATION,
        http::header::ACCEPT,
        http::header::CONTENT_TYPE,
        // sentry headers
        HeaderName::from_static("sentry-trace"),
        HeaderName::from_static("baggage"),
        // our dex headers
        HeaderName::from_static("x-api-key"),
    ];

    let extra: Vec<_> = allowed_headers
        .iter()
        .filter_map(|h| HeaderName::from_str(h.as_str()).ok())
        .collect();
    headers.extend(extra);

    let mut cors = Cors::default()
        .allowed_methods(vec!["GET", "POST", "OPTIONS"])
        .allowed_headers(headers)
        .max_age(3600);

    if cors_domain == "*" || cors_domain.is_empty() {
        cors = cors.allow_any_origin();
    } else {
        cors = cors.allowed_origin(cors_domain).supports_credentials();
    }

    cors
}

pub async fn run_metrics_server(
    config: MetricsConfig,
    cancel: CancellationToken,
    registry: Registry,
) -> std::io::Result<()> {
    let host = format!("{}:{}", config.listen_address, config.port);

    log::info!("Starting metrics HTTP server at http://{}", host.clone());

    let metrics = PrometheusMetricsBuilder::new(&config.namespace)
        .registry(registry)
        .endpoint("/metrics")
        .exclude("/metrics")
        .mask_unmatched_patterns("UNKNOWN")
        .build()
        // It is safe to unwrap when
        // __no other app has the same namespace__
        .unwrap();

    let server = HttpServer::new(move || {
        App::new()
            .wrap(middleware::Logger::default())
            .wrap(metrics.clone())
            .default_service(web::to(default_service))
    })
    .bind(host)?;

    tokio::select! {
        _ = cancel.cancelled() => {
            log::info!("Metrics Server canceled");
        }
        res = server.run() => {
            res?;
        }
    }
    log::info!("Metrics Server stopped");
    Ok(())
}

async fn default_service() -> HttpResponse {
    HttpResponse::Forbidden().finish()
}
