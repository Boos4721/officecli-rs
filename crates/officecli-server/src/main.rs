use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use officecli_core::{OfficeFormat, OfficePackage, PackageSummary};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, RwLock},
};
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    documents: Arc<RwLock<HashMap<Uuid, OfficePackage>>>,
    jobs: Arc<RwLock<HashMap<Uuid, JobRecord>>>,
    data_dir: Arc<PathBuf>,
}

impl AppState {
    fn new() -> std::io::Result<Self> {
        let data_dir = PathBuf::from(
            std::env::var("OFFICECLI_DATA_DIR").unwrap_or_else(|_| "/tmp/officecli-data".into()),
        );
        fs::create_dir_all(&data_dir)?;
        let mut documents = HashMap::new();
        for entry in fs::read_dir(&data_dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let Ok(id) = Uuid::parse_str(stem) else {
                continue;
            };
            let Ok(bytes) = fs::read(&path) else { continue };
            if let Ok(package) = OfficePackage::open(bytes) {
                documents.insert(id, package);
            }
        }
        Ok(Self {
            documents: Arc::new(RwLock::new(documents)),
            jobs: Arc::new(RwLock::new(HashMap::new())),
            data_dir: Arc::new(data_dir),
        })
    }
}

#[derive(Debug, Deserialize)]
struct CreateRequest {
    format: OfficeFormat,
    bytes_base64: Option<String>,
}

#[derive(Debug, Serialize)]
struct DocumentResponse {
    id: Uuid,
    summary: PackageSummary,
}

#[derive(Debug, Deserialize)]
struct PartRequest {
    content_base64: String,
}

#[derive(Debug, Deserialize, Clone)]
struct CommandRequest {
    command: String,
    part: Option<String>,
    path: Option<String>,
    name: Option<String>,
    source: Option<String>,
    target: Option<String>,
    first: Option<String>,
    second: Option<String>,
    content_base64: Option<String>,
    #[serde(default)]
    props: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct JobRequest {
    document_id: Uuid,
    command: CommandRequest,
}

#[derive(Debug, Serialize, Clone)]
struct JobRecord {
    id: Uuid,
    document_id: Uuid,
    status: String,
    error: Option<String>,
}

#[tokio::main]
async fn main() {
    let state = AppState::new().expect("initialize OFFICECLI_DATA_DIR");
    let app = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/v1/office/documents", post(create_document))
        .route("/v1/office/documents/{id}", get(inspect_document))
        .route("/v1/office/documents/{id}/view/{mode}", get(view_document))
        .route("/v1/office/documents/{id}/commands", post(execute_command))
        .route("/v1/office/documents/{id}/batch", post(execute_batch))
        .route("/v1/office/jobs", post(create_job))
        .route("/v1/office/jobs/{id}", get(get_job))
        .route("/v1/office/documents/{id}/parts", get(list_parts))
        .route(
            "/v1/office/documents/{id}/parts/{*part}",
            get(read_part).post(write_part),
        )
        .route("/v1/office/documents/{id}/download", get(download_document))
        .with_state(state);
    let address: SocketAddr = std::env::var("OFFICECLI_BIND")
        .unwrap_or_else(|_| "0.0.0.0:26315".into())
        .parse()
        .expect("valid OFFICECLI_BIND");
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("bind OFFICECLI_BIND");
    axum::serve(listener, app).await.expect("serve officecli");
}

async fn health() -> &'static str {
    "ok"
}

async fn ready() -> &'static str {
    "ready"
}

async fn create_job(State(state): State<AppState>, Json(request): Json<JobRequest>) -> Response {
    let job_id = Uuid::new_v4();
    let mut documents = state.documents.write().expect("document lock");
    let Some(package) = documents.get(&request.document_id) else {
        return not_found();
    };
    let result = apply_command(package, &request.command).and_then(|changed| {
        persist_package(&state, request.document_id, &changed)?;
        let summary = changed.summary().map_err(|error| error.to_string())?;
        documents.insert(request.document_id, changed);
        Ok(summary)
    });
    let record = match result {
        Ok(_) => JobRecord {
            id: job_id,
            document_id: request.document_id,
            status: "succeeded".to_owned(),
            error: None,
        },
        Err(error) => JobRecord {
            id: job_id,
            document_id: request.document_id,
            status: "failed".to_owned(),
            error: Some(error),
        },
    };
    state
        .jobs
        .write()
        .expect("job lock")
        .insert(job_id, record.clone());
    (StatusCode::ACCEPTED, Json(record)).into_response()
}

async fn get_job(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match state.jobs.read().expect("job lock").get(&id).cloned() {
        Some(job) => (StatusCode::OK, Json(job)).into_response(),
        None => not_found(),
    }
}

async fn create_document(
    State(state): State<AppState>,
    Json(request): Json<CreateRequest>,
) -> impl IntoResponse {
    let result = request
        .bytes_base64
        .map(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|error| error.to_string())
        })
        .transpose()
        .and_then(|bytes| {
            bytes.map_or_else(
                || OfficePackage::create(request.format).map_err(|error| error.to_string()),
                |bytes| OfficePackage::open(bytes).map_err(|error| error.to_string()),
            )
        });
    match result.and_then(|package| {
        package
            .summary()
            .map(|summary| (package, summary))
            .map_err(|error| error.to_string())
    }) {
        Ok((package, summary)) => {
            let id = Uuid::new_v4();
            if let Err(error) = persist_package(&state, id, &package) {
                return storage_error(error);
            }
            state
                .documents
                .write()
                .expect("document lock")
                .insert(id, package);
            (StatusCode::CREATED, Json(DocumentResponse { id, summary })).into_response()
        }
        Err(error) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error":{"code":"invalid_document","message":error}})),
        )
            .into_response(),
    }
}

async fn inspect_document(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match state
        .documents
        .read()
        .expect("document lock")
        .get(&id)
        .and_then(|package| package.summary().ok())
    {
        Some(summary) => (StatusCode::OK, Json(DocumentResponse { id, summary })).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error":{"code":"not_found","message":"document not found"}})),
        )
            .into_response(),
    }
}

async fn view_document(
    State(state): State<AppState>,
    Path((id, mode)): Path<(Uuid, String)>,
) -> Response {
    let documents = state.documents.read().expect("document lock");
    let Some(package) = documents.get(&id) else {
        return not_found();
    };
    match mode.as_str() {
        "text" | "annotated" => match package.text_content() {
            Ok(text) => (StatusCode::OK, text).into_response(),
            Err(error) => package_error(error.to_string()),
        },
        "outline" => match package.query_xml(default_xml_part(package.format()), "/") {
            Ok(nodes) => (StatusCode::OK, Json(nodes)).into_response(),
            Err(error) => package_error(error.to_string()),
        },
        "stats" => match package.summary().and_then(|summary| {
            package.text_content().map(|text| {
                serde_json::json!({
                    "format": summary.format,
                    "bytes": summary.bytes,
                    "parts": summary.parts.len(),
                    "characters": text.chars().count(),
                    "words": text.split_whitespace().count()
                })
            })
        }) {
            Ok(stats) => (StatusCode::OK, Json(stats)).into_response(),
            Err(error) => package_error(error.to_string()),
        },
        "html" => match package.text_content() {
            Ok(text) => (
                StatusCode::OK,
                [("content-type", "text/html; charset=utf-8")],
                format!(
                    "<!doctype html><meta charset=\"utf-8\"><title>OfficeCLI preview</title><pre>{}</pre>",
                    quick_xml::escape::escape(&text)
                ),
            )
                .into_response(),
            Err(error) => package_error(error.to_string()),
        },
        _ => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error":{"code":"unknown_view","message":"supported views: text, annotated, outline, stats"}})),
        )
            .into_response(),
    }
}

async fn list_parts(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match state
        .documents
        .read()
        .expect("document lock")
        .get(&id)
        .map(OfficePackage::summary)
    {
        Some(Ok(summary)) => (StatusCode::OK, Json(summary)).into_response(),
        Some(Err(error)) => package_error(error.to_string()),
        None => not_found(),
    }
}

async fn read_part(
    State(state): State<AppState>,
    Path((id, part)): Path<(Uuid, String)>,
) -> Response {
    match state
        .documents
        .read()
        .expect("document lock")
        .get(&id)
        .map(|package| package.read_part(&part))
    {
        Some(Ok(bytes)) => (
            StatusCode::OK,
            [("content-type", "application/octet-stream")],
            bytes,
        )
            .into_response(),
        Some(Err(error)) => package_error(error.to_string()),
        None => not_found(),
    }
}

async fn write_part(
    State(state): State<AppState>,
    Path((id, part)): Path<(Uuid, String)>,
    Json(request): Json<PartRequest>,
) -> Response {
    let content = match base64::engine::general_purpose::STANDARD.decode(request.content_base64) {
        Ok(content) => content,
        Err(error) => return package_error(format!("invalid base64 content: {error}")),
    };
    let mut documents = state.documents.write().expect("document lock");
    let Some(package) = documents.get(&id) else {
        return not_found();
    };
    let changed = match package.with_part(&part, &content) {
        Ok(package) => package,
        Err(error) => return package_error(error.to_string()),
    };
    let summary = match changed.summary() {
        Ok(summary) => summary,
        Err(error) => return package_error(error.to_string()),
    };
    if let Err(error) = persist_package(&state, id, &changed) {
        return storage_error(error);
    }
    documents.insert(id, changed);
    (StatusCode::OK, Json(DocumentResponse { id, summary })).into_response()
}

async fn download_document(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match state.documents.read().expect("document lock").get(&id) {
        Some(package) => (
            StatusCode::OK,
            [("content-type", package.format().content_type())],
            package.bytes().to_vec(),
        )
            .into_response(),
        None => not_found(),
    }
}

async fn execute_command(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(request): Json<CommandRequest>,
) -> Response {
    let mut documents = state.documents.write().expect("document lock");
    let Some(package) = documents.get(&id) else {
        return not_found();
    };
    let changed = match apply_command(package, &request) {
        Ok(package) => package,
        Err(error) => return package_error(error),
    };
    let summary = match changed.summary() {
        Ok(summary) => summary,
        Err(error) => return package_error(error.to_string()),
    };
    if let Err(error) = persist_package(&state, id, &changed) {
        return storage_error(error);
    }
    documents.insert(id, changed);
    (StatusCode::OK, Json(DocumentResponse { id, summary })).into_response()
}

#[derive(Debug, Deserialize)]
struct BatchRequest {
    commands: Vec<CommandRequest>,
}

async fn execute_batch(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(request): Json<BatchRequest>,
) -> Response {
    let mut documents = state.documents.write().expect("document lock");
    let Some(package) = documents.get(&id) else {
        return not_found();
    };
    let mut changed = package.clone();
    for command in &request.commands {
        match apply_command(&changed, command) {
            Ok(next) => changed = next,
            Err(error) => {
                return package_error(format!(
                    "batch command `{}` failed: {error}",
                    command.command
                ));
            }
        }
    }
    let summary = match changed.summary() {
        Ok(summary) => summary,
        Err(error) => return package_error(error.to_string()),
    };
    if let Err(error) = persist_package(&state, id, &changed) {
        return storage_error(error);
    }
    documents.insert(id, changed);
    (StatusCode::OK, Json(DocumentResponse { id, summary })).into_response()
}

fn apply_command(
    package: &OfficePackage,
    request: &CommandRequest,
) -> Result<OfficePackage, String> {
    let part = request
        .part
        .clone()
        .unwrap_or_else(|| default_xml_part(package.format()).to_owned());
    match request.command.as_str() {
        "set" => {
            if request.part.is_none() {
                package
                    .set_path(request.path.as_deref().unwrap_or("/"), &request.props)
                    .map_err(|error| error.to_string())
            } else {
                package
                    .set_xml(
                        &part,
                        request.path.as_deref().unwrap_or("/"),
                        &request.props,
                    )
                    .map_err(|error| error.to_string())
            }
        }
        "add" => decode_content(request).and_then(|content| {
            let fragment = std::str::from_utf8(&content).map_err(|error| error.to_string())?;
            if request.part.is_none() {
                package
                    .insert_path(request.path.as_deref().unwrap_or("/"), fragment)
                    .map_err(|error| error.to_string())
            } else {
                package
                    .insert_xml(&part, request.path.as_deref().unwrap_or("/"), fragment)
                    .map_err(|error| error.to_string())
            }
        }),
        "raw-set" => decode_content(request).and_then(|content| {
            package
                .with_part(&part, &content)
                .map_err(|error| error.to_string())
        }),
        "add-part" => decode_content(request).and_then(|content| {
            package
                .with_part(request.name.as_deref().unwrap_or(&part), &content)
                .map_err(|error| error.to_string())
        }),
        "remove" => match request.path.as_deref() {
            Some(path) if request.part.is_none() => {
                package.remove_path(path).map_err(|error| error.to_string())
            }
            Some(path) => package
                .remove_xml(&part, path)
                .map_err(|error| error.to_string()),
            None => package
                .remove_part(request.name.as_deref().unwrap_or(&part))
                .map_err(|error| error.to_string()),
        },
        "move" => package
            .move_xml(
                &part,
                request
                    .source
                    .as_deref()
                    .or(request.path.as_deref())
                    .ok_or_else(|| "move requires source or path".to_owned())?,
                request
                    .target
                    .as_deref()
                    .ok_or_else(|| "move requires target".to_owned())?,
            )
            .map_err(|error| error.to_string()),
        "swap" => package
            .swap_xml(
                &part,
                request
                    .first
                    .as_deref()
                    .ok_or_else(|| "swap requires first".to_owned())?,
                request
                    .second
                    .as_deref()
                    .ok_or_else(|| "swap requires second".to_owned())?,
            )
            .map_err(|error| error.to_string()),
        "merge" => package
            .merge_text(&request.props)
            .map_err(|error| error.to_string()),
        "validate" => package
            .validate()
            .map(|_| package.clone())
            .map_err(|error| error.to_string()),
        command => Err(format!("unsupported command: {command}")),
    }
}

fn decode_content(request: &CommandRequest) -> Result<Vec<u8>, String> {
    let Some(content) = request.content_base64.as_deref() else {
        return Err("content_base64 is required".to_owned());
    };
    base64::engine::general_purpose::STANDARD
        .decode(content)
        .map_err(|error| format!("invalid base64 content: {error}"))
}

fn default_xml_part(format: OfficeFormat) -> &'static str {
    match format {
        OfficeFormat::Docx => "word/document.xml",
        OfficeFormat::Xlsx => "xl/workbook.xml",
        OfficeFormat::Pptx => "ppt/presentation.xml",
    }
}

fn persist_package(state: &AppState, id: Uuid, package: &OfficePackage) -> Result<(), String> {
    let path = state
        .data_dir
        .join(format!("{id}.{}", package.format().extension()));
    fs::write(path, package.bytes()).map_err(|error| error.to_string())
}

fn storage_error(message: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"error":{"code":"storage_error","message":message}})),
    )
        .into_response()
}

fn package_error(message: String) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({"error":{"code":"invalid_document","message":message}})),
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error":{"code":"not_found","message":"document not found"}})),
    )
        .into_response()
}
