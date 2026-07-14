use std::collections::HashMap;
use std::fmt::Display;

use actix_web::error::JsonPayloadError;
use actix_web::http::StatusCode;
use actix_web::{HttpResponse, ResponseError};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct ErrorResponse {
    pub error: ApiError,
}

#[derive(Default, Debug, Clone, Deserialize, Serialize)]
pub struct ApiError {
    #[serde(skip)]
    pub http_code: StatusCode,
    pub code: u16,
    pub status: String,
    pub message: String,
    #[serde(default)]
    pub details: HashMap<String, String>,
}

pub fn access_denied() -> ApiError {
    let code = ApiErrorCode::AccessDenied;
    ApiError {
        http_code: StatusCode::UNAUTHORIZED,
        code: code as u16,
        status: code.to_string(),
        message: "".into(),
        details: HashMap::new(),
    }
}

pub fn forbidden() -> ApiError {
    let code = ApiErrorCode::Forbidden;
    ApiError {
        http_code: StatusCode::FORBIDDEN,
        code: code as u16,
        status: code.to_string(),
        message: "".into(),
        details: HashMap::new(),
    }
}

pub fn not_found() -> ApiError {
    let code = ApiErrorCode::NotFound;
    ApiError {
        http_code: StatusCode::NOT_FOUND,
        code: code as u16,
        status: code.to_string(),
        message: "".into(),
        details: HashMap::new(),
    }
}

pub fn bad_requests(message: &str) -> ApiError {
    let code = ApiErrorCode::BadInput;
    ApiError {
        http_code: StatusCode::BAD_REQUEST,
        code: code as u16,
        status: code.to_string(),
        message: message.into(),
        details: HashMap::new(),
    }
}

pub fn internal_server_error() -> ApiError {
    let code = ApiErrorCode::InternalError;
    ApiError {
        http_code: StatusCode::INTERNAL_SERVER_ERROR,
        code: code as u16,
        status: code.to_string(),
        message: "something went wrong".into(),
        details: HashMap::new(),
    }
}

impl Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status, self.message)
    }
}

#[derive(Debug, Clone, Copy, num_enum::IntoPrimitive, num_enum::TryFromPrimitive)]
#[repr(u16)]
pub enum ApiErrorCode {
    // all kinds of server-side issues: no connection to DB, or 3rd party api, or...
    #[num_enum(default)]
    InternalError = 500,
    // The server cannot handle the request due to stale indexer,
    // or unreachable BitcoinRPC or DB, or something else.
    ServiceUnavailable = 503,
    // invalid api-key
    AccessDenied = 401,
    // api-key blocked or suspended
    Forbidden = 403,
    // entity for passed params is not found
    NotFound = 1000,
    // user provided bad input
    BadInput = 1001,
    // user provided an invalid address
    InvalidAddress = 1002,
    // collect utxo: not enough balance
    NotEnoughBalance = 1003,
    // collect utxo: increase "max_utxos" parameter
    NeedMoreUtxos = 1004,
}

impl Display for ApiErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let val = match self {
            Self::InternalError => "internal_error",
            Self::ServiceUnavailable => "service_unavailable",
            Self::AccessDenied => "access_denied",
            Self::Forbidden => "access_forbidden",
            Self::NotFound => "not_found",
            Self::BadInput => "bad_input",
            Self::InvalidAddress => "invalid_address",
            Self::NotEnoughBalance => "not_enough_balance",
            Self::NeedMoreUtxos => "not_enough_utxos",
        };
        write!(f, "{val}")
    }
}

impl std::convert::From<ApiError> for HttpResponse {
    fn from(error: ApiError) -> Self {
        let mut resp = HttpResponse::build(error.http_code);
        resp.json(&ErrorResponse { error })
    }
}

impl std::convert::From<&ApiError> for HttpResponse {
    fn from(error: &ApiError) -> Self {
        let mut resp = HttpResponse::build(error.http_code);
        resp.json(&ErrorResponse {
            error: error.clone(),
        })
    }
}

impl From<JsonPayloadError> for ApiError {
    fn from(error: JsonPayloadError) -> Self {
        log::warn!("invalid json: {:#}", error);

        let code = ApiErrorCode::BadInput;
        ApiError {
            http_code: StatusCode::UNPROCESSABLE_ENTITY,
            code: code as u16,
            status: code.to_string(),
            message: "invalid json format".into(),
            details: HashMap::from([("body".to_string(), format!("{error:#}"))]),
        }
    }
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        self.http_code
    }

    fn error_response(&self) -> HttpResponse {
        self.into()
    }
}
