use super::Json;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use crate::config::store::ConfigStore;
use crate::config::validate::{validate, ValidationReport};
use crate::config::{ConfigFormat, FerrixConfig, UpstreamConfig, VirtualHost};
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

pub async fn get_status(state: &AdminState) -> impl IntoResponse {
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

pub async fn get_metrics(state: &AdminState) -> impl IntoResponse {
    let mut snap = state.metrics.get_metrics_snapshot();
    snap.enabled = state.store.config().metrics.enabled;
    Json(snap)
}

pub async fn get_logs(state: &AdminState, params: LogsQuery) -> impl IntoResponse {
    if !state.store.config().metrics.enabled {
        return Json(Vec::new());
    }
    let limit = params.limit.unwrap_or(100).min(500);
    let logs = state
        .metrics
        .get_recent_logs(limit, params.status, params.q.as_deref());
    Json(logs)
}

pub async fn get_config(state: &AdminState) -> impl IntoResponse {
    Json((*state.store.config()).clone())
}

pub async fn validate_config(candidate: FerrixConfig) -> impl IntoResponse {
    Json(report_payload(&validate(&candidate)))
}

/// Same as `POST /api/config/validate`, for YAML text.
pub async fn validate_config_yaml(body: String) -> axum::response::Response {
    match FerrixConfig::parse(&body, ConfigFormat::Yaml) {
        Ok(candidate) => Json(report_payload(&validate(&candidate))).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "invalid",
                "message": format!("Invalid YAML: {}", e),
            })),
        )
            .into_response(),
    }
}

pub async fn update_config(
    state: &AdminState,
    new_config: FerrixConfig,
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

/// The active configuration as YAML, exactly as it would be written to `ferrix.yaml`
/// (the dashboard's config editor works on this text).
pub async fn get_config_yaml(state: &AdminState) -> impl IntoResponse {
    match state.store.config().render(ConfigFormat::Yaml) {
        Ok(yaml) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/yaml; charset=utf-8")],
            yaml,
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Replaces the configuration from YAML text: parsed, validated, persisted and
/// hot-reloaded, with the same JSON result as `POST /api/config`.
pub async fn update_config_yaml(
    state: &AdminState,
    body: String,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let new_config = FerrixConfig::parse(&body, ConfigFormat::Yaml).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "invalid",
                "message": format!("Invalid YAML: {}", e),
            })),
        )
    })?;
    update_config(state, new_config).await
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

pub async fn route_test(state: &AdminState, req: RouteTestRequest) -> impl IntoResponse {
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

pub async fn get_vhosts(state: &AdminState) -> impl IntoResponse {
    Json(state.store.config().vhosts.clone())
}

pub async fn save_vhost(
    state: &AdminState,
    vhost: VirtualHost,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let mut cfg = (*state.store.config()).clone();

    match cfg.vhosts.iter().position(|v| v.id == vhost.id) {
        Some(pos) => cfg.vhosts[pos] = vhost,
        None => cfg.vhosts.push(vhost),
    }

    apply_validated(state, cfg, "Virtual host saved and configuration reloaded")
}

pub async fn delete_vhost(
    state: &AdminState,
    id: String,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let mut cfg = (*state.store.config()).clone();
    let before = cfg.vhosts.len();
    cfg.vhosts.retain(|v| v.id != id);

    if cfg.vhosts.len() == before {
        return Err(not_found(format!("Virtual host '{}' not found", id)));
    }

    apply_validated(
        state,
        cfg,
        "Virtual host deleted and configuration reloaded",
    )
}

pub async fn get_upstreams(state: &AdminState) -> impl IntoResponse {
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
    state: &AdminState,
    upstream: UpstreamConfig,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let mut cfg = (*state.store.config()).clone();
    cfg.upstreams.insert(upstream.name.clone(), upstream);

    apply_validated(state, cfg, "Upstream saved and configuration reloaded")
}

pub async fn delete_upstream(
    state: &AdminState,
    name: String,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let mut cfg = (*state.store.config()).clone();
    if cfg.upstreams.remove(&name).is_none() {
        return Err(not_found(format!("Upstream '{}' not found", name)));
    }

    apply_validated(state, cfg, "Upstream deleted and configuration reloaded")
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

#[cfg(feature = "nginx-import")]
#[derive(Debug, Deserialize)]
pub struct ImportNginxRequest {
    pub source: String,

    #[serde(default)]
    pub file_name: Option<String>,
}

#[cfg(feature = "nginx-import")]
pub async fn import_nginx(req: ImportNginxRequest) -> impl IntoResponse {
    let file_name = req.file_name.unwrap_or_else(|| "nginx.conf".to_string());

    match crate::nginx::convert::import_str(&req.source, &file_name) {
        Ok(result) => {
            let validation = validate(&result.config);
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "success",
                    "config_yaml": result.config.render(ConfigFormat::Yaml).unwrap_or_default(),
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
