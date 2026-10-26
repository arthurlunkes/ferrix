use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use crate::config::store::ConfigStore;
use crate::config::validate::{validate, ValidationReport};
use crate::config::{FerrixConfig, UpstreamConfig, VirtualHost};
use crate::health::HealthChecker;
use crate::metrics::MetricsCollector;

#[derive(Clone)]
pub struct AdminState {
    pub store: Arc<ConfigStore>,
    pub metrics: Arc<MetricsCollector>,
    pub health_checker: Arc<HealthChecker>,
}

#[derive(Debug, Serialize)]
pub struct ServerStatus {
    pub name: &'static str,
    pub version: &'static str,
    pub uptime_seconds: u64,
    pub start_time_utc: String,
    pub admin_listen: String,
    pub proxy_listen: String,
    pub total_vhosts: usize,
    pub total_upstreams: usize,

    pub admin_auth_enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    pub limit: Option<usize>,
    pub status: Option<u16>,
    pub q: Option<String>,
}

pub async fn get_status(State(state): State<AdminState>) -> impl IntoResponse {
    let cfg = state.store.config();
    let metrics = state.metrics.get_metrics_snapshot();

    Json(ServerStatus {
        name: "Ferrix",
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: metrics.uptime_seconds,
        start_time_utc: state.metrics.start_time_utc().to_rfc3339(),
        admin_listen: cfg.admin.listen.clone(),
        proxy_listen: cfg.http.listen.clone(),
        total_vhosts: cfg.vhosts.len(),
        total_upstreams: cfg.upstreams.len(),
        admin_auth_enabled: cfg.admin.auth.is_enabled(),
    })
}

pub async fn get_metrics(State(state): State<AdminState>) -> impl IntoResponse {
    let mut snap = state.metrics.get_metrics_snapshot();
    snap.enabled = state.store.config().metrics.enabled;
    Json(snap)
}

pub async fn get_logs(
    State(state): State<AdminState>,
    Query(params): Query<LogsQuery>,
) -> impl IntoResponse {
    if !state.store.config().metrics.enabled {
        return Json(Vec::new());
    }
    let limit = params.limit.unwrap_or(100).min(500);
    let logs = state
        .metrics
        .get_recent_logs(limit, params.status, params.q.as_deref());
    Json(logs)
}

pub async fn get_config(State(state): State<AdminState>) -> impl IntoResponse {
    Json((*state.store.config()).clone())
}

pub async fn validate_config(
    State(_state): State<AdminState>,
    Json(candidate): Json<FerrixConfig>,
) -> impl IntoResponse {
    Json(report_payload(&validate(&candidate)))
}

pub async fn update_config(
    State(state): State<AdminState>,
    Json(new_config): Json<FerrixConfig>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let report = validate(&new_config);
    if !report.is_valid() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "invalid",
                "message": "Configuration contains errors and was not applied",
                "report": report_payload(&report),
            })),
        ));
    }

    state.store.persist_and_apply(new_config).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "status": "error", "message": e })),
        )
    })?;

    info!("Configuration validated, persisted, and hot-reloaded successfully");
    Ok(Json(serde_json::json!({
        "status": "success",
        "message": "Configuration updated and hot-reloaded successfully",
        "report": report_payload(&report),
        "config": (*state.store.config()).clone(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct RouteTestRequest {
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct RouteTestResponse {
    pub matched: bool,
    pub vhost_id: Option<String>,
    pub vhost_name: Option<String>,

    pub vhost_reason: Option<String>,
    pub route_id: Option<String>,
    pub route_pattern: Option<String>,

    pub route_reason: Option<String>,
    pub target: Option<String>,
    pub explanation: String,
}

pub async fn route_test(
    State(state): State<AdminState>,
    Json(req): Json<RouteTestRequest>,
) -> impl IntoResponse {
    use crate::config::RouteTargetType;

    let compiled = state.store.compiled();
    let port = req.port.unwrap_or_else(|| {
        compiled
            .config
            .http
            .listen
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse().ok())
            .unwrap_or(8000)
    });

    let Some(vm) = compiled.find_vhost(&req.host, port) else {
        return Json(RouteTestResponse {
            matched: false,
            vhost_id: None,
            vhost_name: None,
            vhost_reason: None,
            route_id: None,
            route_pattern: None,
            route_reason: None,
            target: None,
            explanation: "No configured virtual host matches this request".to_string(),
        });
    };

    let vhost = &compiled.config.vhosts[vm.vhost_index];

    let Some(rm) = compiled.match_route(vm.vhost_index, &req.path) else {
        return Json(RouteTestResponse {
            matched: false,
            vhost_id: Some(vhost.id.clone()),
            vhost_name: Some(vhost.name.clone()),
            vhost_reason: Some(vm.reason.as_str().to_string()),
            route_id: None,
            route_pattern: None,
            route_reason: None,
            target: None,
            explanation: format!(
                "Host '{}' matched by {}, but no route matches '{}' — 404 response",
                vhost.name,
                vm.reason.as_str(),
                req.path
            ),
        });
    };

    let route = &vhost.routes[rm.route_index];
    let target = match route.target_type {
        RouteTargetType::ProxyUpstream => route
            .upstream_name
            .clone()
            .map(|u| format!("upstream '{}'", u)),
        RouteTargetType::StaticFiles => route
            .static_root
            .clone()
            .map(|r| format!("files in '{}'", r)),
        RouteTargetType::Redirect => route
            .redirect_url
            .clone()
            .map(|u| format!("redirect {} to {}", route.redirect_code.unwrap_or(302), u)),
    };

    Json(RouteTestResponse {
        matched: true,
        vhost_id: Some(vhost.id.clone()),
        vhost_name: Some(vhost.name.clone()),
        vhost_reason: Some(vm.reason.as_str().to_string()),
        route_id: Some(route.id.clone()),
        route_pattern: Some(route.path_prefix.clone()),
        route_reason: Some(rm.reason.as_str().to_string()),
        target: target.clone(),
        explanation: format!(
            "Host '{}' chosen by {}; route '{}' matched by {}; target: {}",
            vhost.name,
            vm.reason.as_str(),
            route.path_prefix,
            rm.reason.as_str(),
            target.as_deref().unwrap_or("not configured")
        ),
    })
}

pub async fn get_vhosts(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.store.config().vhosts.clone())
}

pub async fn save_vhost(
    State(state): State<AdminState>,
    Json(vhost): Json<VirtualHost>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let mut cfg = (*state.store.config()).clone();

    match cfg.vhosts.iter().position(|v| v.id == vhost.id) {
        Some(pos) => cfg.vhosts[pos] = vhost,
        None => cfg.vhosts.push(vhost),
    }

    apply_validated(&state, cfg, "Virtual host saved and configuration reloaded")
}

pub async fn delete_vhost(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let mut cfg = (*state.store.config()).clone();
    let before = cfg.vhosts.len();
    cfg.vhosts.retain(|v| v.id != id);

    if cfg.vhosts.len() == before {
        return Err(not_found(format!("Virtual host '{}' not found", id)));
    }

    apply_validated(
        &state,
        cfg,
        "Virtual host deleted and configuration reloaded",
    )
}

pub async fn get_upstreams(State(state): State<AdminState>) -> impl IntoResponse {
    let cfg = state.store.config();
    let health_statuses = state.health_checker.get_all_statuses();

    #[derive(Serialize)]
    struct UpstreamWithHealth {
        name: String,
        config: UpstreamConfig,
        statuses: Vec<crate::health::TargetHealthStatus>,
    }

    let result: Vec<UpstreamWithHealth> = cfg
        .upstreams
        .iter()
        .map(|(name, u)| UpstreamWithHealth {
            name: name.clone(),
            config: u.clone(),
            statuses: health_statuses
                .iter()
                .filter(|s| s.upstream_name == *name)
                .cloned()
                .collect(),
        })
        .collect();

    Json(result)
}

pub async fn save_upstream(
    State(state): State<AdminState>,
    Json(upstream): Json<UpstreamConfig>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let mut cfg = (*state.store.config()).clone();
    cfg.upstreams.insert(upstream.name.clone(), upstream);

    apply_validated(&state, cfg, "Upstream saved and configuration reloaded")
}

pub async fn delete_upstream(
    State(state): State<AdminState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let mut cfg = (*state.store.config()).clone();
    if cfg.upstreams.remove(&name).is_none() {
        return Err(not_found(format!("Upstream '{}' not found", name)));
    }

    apply_validated(&state, cfg, "Upstream deleted and configuration reloaded")
}

fn apply_validated(
    state: &AdminState,
    cfg: FerrixConfig,
    success_log: &str,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let report = validate(&cfg);
    if !report.is_valid() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "invalid",
                "message": "Change would leave configuration in an invalid state and was not applied",
                "report": report_payload(&report),
            })),
        ));
    }

    state.store.persist_and_apply(cfg).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "status": "error", "message": e })),
        )
    })?;

    info!("{}", success_log);
    Ok(Json(serde_json::json!({
        "status": "success",
        "report": report_payload(&report),
    })))
}

fn report_payload(report: &ValidationReport) -> serde_json::Value {
    serde_json::json!({
        "valid": report.is_valid(),
        "issues": report.issues,
        "summary": report.summary(),
    })
}

fn not_found(message: String) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "status": "error", "message": message })),
    )
}

#[derive(Debug, Deserialize)]
pub struct ImportNginxRequest {
    pub source: String,

    #[serde(default)]
    pub file_name: Option<String>,
}

pub async fn import_nginx(Json(req): Json<ImportNginxRequest>) -> impl IntoResponse {
    let file_name = req.file_name.unwrap_or_else(|| "nginx.conf".to_string());

    match crate::nginx::convert::import_str(&req.source, &file_name) {
        Ok(result) => {
            let validation = validate(&result.config);
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "success",
                    "config": result.config,
                    "report": result.report,
                    "validation": report_payload(&validation),
                })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "message": format!("Syntax error: {}", e),
            })),
        )
            .into_response(),
    }
}
