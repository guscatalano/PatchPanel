//! Shared portal state and the error type the API returns.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::db::Db;
use crate::hub::Hub;

pub struct AppState {
    pub db: Db,
    pub hub: Hub,
    /// Shared secret a brand-new agent presents once, to obtain its own token.
    pub enrollment_token: String,
    /// Bearer token for the dashboard and the REST API.
    pub admin_token: String,
    /// When false, `/api/*` is served without authentication. Only sane on a
    /// network you fully trust, because the API can install software on and
    /// reboot every enrolled machine.
    pub require_admin_auth: bool,
    /// Directory holding the agent artefacts this portal will hand out.
    pub agent_dir: std::path::PathBuf,
    /// Fallback address used in generated install scripts when a client sends
    /// no `Host:` header. Normally the request's own Host wins.
    pub public_host: String,
    /// Where received syslog is written, one file per sender. Empty when the
    /// receiver is off.
    pub log_dir: std::path::PathBuf,
    /// Whether anything is listening for syslog at all, so the page can say
    /// "off" rather than "nothing has arrived".
    pub syslog_on: bool,
}

pub type SharedState = Arc<AppState>;

/// Anything a handler can fail with. Internal detail is logged, not returned,
/// except where the status makes it the caller's problem to fix.
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::CONFLICT,
            message: msg.into(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!(error = %format!("{e:#}"), "request failed");
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal error".into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;
