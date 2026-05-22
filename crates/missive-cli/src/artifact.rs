//! A2A artifact persistence, inspection, and safe local export helpers.
//!
//! The public CLI surface for ticket 024 is nested under `missive task artifact`
//! so artifact operations stay tied to durable task state until the broader
//! event/gateway tickets add cross-task artifact workflows.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{ArgAction, Args, Subcommand};
use missive_a2a::protocol::{Artifact, Part, PartContent, Task, TaskArtifactUpdateEvent};
use missive_core::{ContextId, Metadata, MissiveError, Result, TaskId};
use missive_store::{
    ArtifactId, ArtifactKind, ArtifactRecord, ArtifactUpsert, Store, StoreTransaction,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::agent::AgentRegistry;
use crate::output::{OutputMode, redact_text, render_success};

const ARTIFACT_DESCRIPTION_METADATA_KEY: &str = "a2a.description";
const ARTIFACT_EXTENSIONS_METADATA_KEY: &str = "a2a.extensions";
const ARTIFACT_REMOTE_METADATA_KEY: &str = "a2a.metadata";
const ARTIFACT_UPDATE_APPEND_METADATA_KEY: &str = "a2a.update.append";
const ARTIFACT_UPDATE_LAST_CHUNK_METADATA_KEY: &str = "a2a.update.last_chunk";
const ARTIFACT_UPDATE_METADATA_KEY: &str = "a2a.update.metadata";

/// Task-scoped artifact subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum TaskArtifactCommands {
    /// List artifacts persisted for one local task.
    List(TaskArtifactListArgs),
    /// Show one persisted artifact and its metadata/content summary.
    Show(TaskArtifactShowArgs),
    /// Save one persisted artifact to a local file.
    Save(TaskArtifactSaveArgs),
    /// Export all persisted artifacts for one task into a directory.
    Export(TaskArtifactExportArgs),
}

impl TaskArtifactCommands {
    /// Stable subcommand spelling used in structured output messages.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::List(_) => "list",
            Self::Show(_) => "show",
            Self::Save(_) => "save",
            Self::Export(_) => "export",
        }
    }
}

/// Arguments for `missive task artifact list`.
#[derive(Debug, Clone, Args)]
pub struct TaskArtifactListArgs {
    /// A2A task id whose persisted artifacts should be listed.
    pub task_id: String,
}

/// Arguments for `missive task artifact show`.
#[derive(Debug, Clone, Args)]
pub struct TaskArtifactShowArgs {
    /// A2A task id that owns the artifact.
    pub task_id: String,
    /// A2A artifact id.
    pub artifact_id: String,
}

/// Arguments for `missive task artifact save`.
#[derive(Debug, Clone, Args)]
pub struct TaskArtifactSaveArgs {
    /// A2A task id that owns the artifact.
    pub task_id: String,
    /// A2A artifact id.
    pub artifact_id: String,
    /// Destination file path or existing directory.
    #[arg(short = 'o', long = "output", value_name = "PATH")]
    pub output: PathBuf,
    /// Overwrite an existing destination file.
    #[arg(long = "force", action = ArgAction::SetTrue)]
    pub force: bool,
}

/// Arguments for `missive task artifact export`.
#[derive(Debug, Clone, Args)]
pub struct TaskArtifactExportArgs {
    /// A2A task id whose persisted artifacts should be exported.
    pub task_id: String,
    /// Destination directory. It is created when missing.
    #[arg(short = 'o', long = "output-dir", value_name = "DIR")]
    pub output_dir: PathBuf,
    /// Overwrite existing files in the destination directory.
    #[arg(long = "force", action = ArgAction::SetTrue)]
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ArtifactSummaryView {
    pub(crate) artifact_id: String,
    pub(crate) task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mime_type: Option<String>,
    pub(crate) kind: String,
    pub(crate) version: u64,
    pub(crate) part_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) text_preview: Option<String>,
    pub(crate) metadata: Metadata,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ArtifactPartView {
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text_chars: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ArtifactDetailView {
    #[serde(flatten)]
    summary: ArtifactSummaryView,
    parts: Vec<ArtifactPartView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ArtifactListOutput {
    profile: String,
    task_id: String,
    count: usize,
    artifacts: Vec<ArtifactSummaryView>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ArtifactShowOutput {
    profile: String,
    artifact: ArtifactDetailView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ArtifactSavedView {
    artifact_id: String,
    task_id: String,
    path: String,
    bytes_written: u64,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ArtifactSaveOutput {
    profile: String,
    artifact: ArtifactSavedView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ArtifactExportOutput {
    profile: String,
    task_id: String,
    output_dir: String,
    count: usize,
    artifacts: Vec<ArtifactSavedView>,
    message: String,
}

struct ArtifactPayload {
    bytes: Vec<u8>,
    extension: &'static str,
}

/// Executes one nested `missive task artifact` command.
pub(crate) fn execute_task_artifact_command<W>(
    command: &TaskArtifactCommands,
    registry: &mut AgentRegistry,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    match command {
        TaskArtifactCommands::List(args) => list_artifacts(args, registry, mode, writer),
        TaskArtifactCommands::Show(args) => show_artifact(args, registry, mode, writer),
        TaskArtifactCommands::Save(args) => save_artifact(args, registry, mode, writer),
        TaskArtifactCommands::Export(args) => export_artifacts(args, registry, mode, writer),
    }
}

/// Persists all artifacts currently embedded in a returned A2A task payload.
pub(crate) fn persist_task_artifacts(
    transaction: &StoreTransaction<'_>,
    task: &Task,
) -> Result<Vec<ArtifactRecord>> {
    let Some(artifacts) = &task.artifacts else {
        return Ok(Vec::new());
    };
    let task_id = TaskId::new(task.id.clone())?;
    let context_id = ContextId::new(task.context_id.clone())?;
    let mut stored = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        let artifact_id = ArtifactId::new(artifact.artifact_id.clone())?;
        let version = transaction
            .get_artifact(&artifact_id)?
            .map_or(1, |existing| existing.version.saturating_add(1));
        let upsert = artifact_upsert_from_protocol(&task_id, Some(&context_id), artifact, version)?;
        stored.push(transaction.upsert_artifact(&upsert)?);
    }
    Ok(stored)
}

/// Persists one A2A `artifactUpdate` stream event, merging appended chunks when requested.
pub(crate) fn persist_artifact_update(
    transaction: &StoreTransaction<'_>,
    update: &TaskArtifactUpdateEvent,
) -> Result<ArtifactRecord> {
    let task_id = TaskId::new(update.task_id.clone())?;
    let context_id = ContextId::new(update.context_id.clone())?;
    let artifact_id = ArtifactId::new(update.artifact.artifact_id.clone())?;
    let existing = transaction.get_artifact(&artifact_id)?;
    let version = existing
        .as_ref()
        .map_or(1, |artifact| artifact.version.saturating_add(1));
    let artifact = if update.append.unwrap_or(false) {
        merge_incremental_artifact(existing.as_ref(), &update.artifact)?
    } else {
        update.artifact.clone()
    };
    let mut upsert =
        artifact_upsert_from_protocol(&task_id, Some(&context_id), &artifact, version)?;
    upsert.metadata.insert(
        ARTIFACT_UPDATE_APPEND_METADATA_KEY,
        update.append.unwrap_or(false),
    )?;
    if let Some(last_chunk) = update.last_chunk {
        upsert
            .metadata
            .insert(ARTIFACT_UPDATE_LAST_CHUNK_METADATA_KEY, last_chunk)?;
    }
    if let Some(metadata) = &update.metadata {
        upsert
            .metadata
            .insert(ARTIFACT_UPDATE_METADATA_KEY, json!(metadata))?;
    }
    transaction.upsert_artifact(&upsert)
}

impl ArtifactSummaryView {
    pub(crate) fn from_record(record: &ArtifactRecord) -> Self {
        let artifact = artifact_from_record(record).ok();
        Self {
            artifact_id: record.artifact_id.as_str().to_owned(),
            task_id: record.task_id.as_str().to_owned(),
            context_id: record
                .context_id
                .as_ref()
                .map(|context_id| context_id.as_str().to_owned()),
            name: record.name.clone(),
            mime_type: record.mime_type.clone(),
            kind: record.kind.as_str().to_owned(),
            version: record.version,
            part_count: artifact.as_ref().map_or(0, |artifact| artifact.parts.len()),
            text_preview: artifact.as_ref().and_then(first_artifact_text),
            metadata: record.metadata.clone(),
            created_at: record.created_at.to_rfc3339(),
            updated_at: record.updated_at.to_rfc3339(),
        }
    }
}

impl ArtifactDetailView {
    fn from_record(record: &ArtifactRecord) -> Self {
        let artifact = artifact_from_record(record).ok();
        let parts = artifact
            .as_ref()
            .map(|artifact| {
                artifact
                    .parts
                    .iter()
                    .map(ArtifactPartView::from_part)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            summary: ArtifactSummaryView::from_record(record),
            parts,
            content: record.content_json.clone(),
        }
    }
}

impl ArtifactPartView {
    fn from_part(part: &Part) -> Self {
        match &part.content {
            PartContent::Text(text) => Self {
                kind: "text".to_owned(),
                filename: part.filename.clone(),
                media_type: part.media_type.clone(),
                text_chars: Some(text.chars().count()),
                raw_bytes: None,
                url: None,
            },
            PartContent::Raw(bytes) => Self {
                kind: "bytes".to_owned(),
                filename: part.filename.clone(),
                media_type: part.media_type.clone(),
                text_chars: None,
                raw_bytes: Some(bytes.len()),
                url: None,
            },
            PartContent::Url(url) => Self {
                kind: "file".to_owned(),
                filename: part.filename.clone(),
                media_type: part.media_type.clone(),
                text_chars: None,
                raw_bytes: None,
                url: Some(url.clone()),
            },
            PartContent::Data(_) => Self {
                kind: "json".to_owned(),
                filename: part.filename.clone(),
                media_type: part.media_type.clone(),
                text_chars: None,
                raw_bytes: None,
                url: None,
            },
        }
    }
}

fn list_artifacts<W>(
    args: &TaskArtifactListArgs,
    registry: &mut AgentRegistry,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let task_id = TaskId::new(args.task_id.clone())?;
    ensure_task_exists(&registry.store, &task_id)?;
    let artifacts = registry.store.list_artifacts_for_task(&task_id)?;
    let views = artifacts
        .iter()
        .map(ArtifactSummaryView::from_record)
        .collect::<Vec<_>>();
    let output = ArtifactListOutput {
        profile: registry.profile.clone(),
        task_id: task_id.as_str().to_owned(),
        count: views.len(),
        message: format!(
            "Listed {} artifact(s) for task '{}'",
            views.len(),
            task_id.as_str()
        ),
        artifacts: views,
    };
    render_artifact_list(writer, mode, &output)
}

fn show_artifact<W>(
    args: &TaskArtifactShowArgs,
    registry: &mut AgentRegistry,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let task_id = TaskId::new(args.task_id.clone())?;
    let artifact_id = ArtifactId::new(args.artifact_id.clone())?;
    let artifact = get_existing_artifact_for_task(&registry.store, &task_id, &artifact_id)?;
    let detail = ArtifactDetailView::from_record(&artifact);
    let output = ArtifactShowOutput {
        profile: registry.profile.clone(),
        message: format!(
            "Showing artifact '{}' for task '{}'",
            artifact_id.as_str(),
            task_id.as_str()
        ),
        artifact: detail,
    };
    render_artifact_show(writer, mode, &output)
}

fn save_artifact<W>(
    args: &TaskArtifactSaveArgs,
    registry: &mut AgentRegistry,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let task_id = TaskId::new(args.task_id.clone())?;
    let artifact_id = ArtifactId::new(args.artifact_id.clone())?;
    let artifact = get_existing_artifact_for_task(&registry.store, &task_id, &artifact_id)?;
    let path = resolve_single_output_path(&artifact, &args.output)?;
    let saved = write_artifact_to_path(&artifact, &path, args.force)?;
    let output = ArtifactSaveOutput {
        profile: registry.profile.clone(),
        message: format!(
            "Saved artifact '{}' for task '{}' to {}",
            artifact_id.as_str(),
            task_id.as_str(),
            saved.path
        ),
        artifact: saved,
    };
    render_artifact_save(writer, mode, &output)
}

fn export_artifacts<W>(
    args: &TaskArtifactExportArgs,
    registry: &mut AgentRegistry,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let task_id = TaskId::new(args.task_id.clone())?;
    ensure_task_exists(&registry.store, &task_id)?;
    let artifacts = registry.store.list_artifacts_for_task(&task_id)?;
    fs::create_dir_all(&args.output_dir).map_err(|error| {
        MissiveError::io(
            format!(
                "creating artifact export directory {}",
                args.output_dir.display()
            ),
            error,
        )
    })?;
    let mut used_names = BTreeSet::new();
    let mut saved = Vec::with_capacity(artifacts.len());
    for artifact in &artifacts {
        let filename = unique_safe_file_name(artifact, &mut used_names)?;
        let path = args.output_dir.join(filename);
        saved.push(write_artifact_to_path(artifact, &path, args.force)?);
    }
    let output = ArtifactExportOutput {
        profile: registry.profile.clone(),
        task_id: task_id.as_str().to_owned(),
        output_dir: args.output_dir.display().to_string(),
        count: saved.len(),
        message: format!(
            "Exported {} artifact(s) for task '{}' to {}",
            saved.len(),
            task_id.as_str(),
            args.output_dir.display()
        ),
        artifacts: saved,
    };
    render_artifact_export(writer, mode, &output)
}

fn ensure_task_exists(store: &Store, task_id: &TaskId) -> Result<()> {
    store.get_task(task_id)?.map(|_| ()).ok_or_else(|| {
        MissiveError::validation(format!("task {:?} is not known locally", task_id.as_str()))
            .with_help("Fetch or create the task before listing artifacts, for example with 'missive task get --remote --agent <alias>'.")
    })
}

fn get_existing_artifact_for_task(
    store: &Store,
    task_id: &TaskId,
    artifact_id: &ArtifactId,
) -> Result<ArtifactRecord> {
    ensure_task_exists(store, task_id)?;
    let artifact = store.get_artifact(artifact_id)?.ok_or_else(|| {
        MissiveError::validation(format!(
            "artifact {:?} is not known locally",
            artifact_id.as_str()
        ))
        .with_help("Run 'missive task artifact list <task-id>' to see persisted artifacts.")
    })?;
    if artifact.task_id != *task_id {
        return Err(MissiveError::validation(format!(
            "artifact {:?} belongs to task {:?}, not {:?}",
            artifact_id.as_str(),
            artifact.task_id.as_str(),
            task_id.as_str()
        )));
    }
    Ok(artifact)
}

fn artifact_upsert_from_protocol(
    task_id: &TaskId,
    context_id: Option<&ContextId>,
    artifact: &Artifact,
    version: u64,
) -> Result<ArtifactUpsert> {
    let mut upsert = ArtifactUpsert::new(
        ArtifactId::new(artifact.artifact_id.clone())?,
        task_id.clone(),
    );
    upsert.context_id = context_id.cloned();
    upsert.name = artifact.name.clone();
    upsert.mime_type = primary_mime_type(&artifact.parts);
    upsert.kind = infer_artifact_kind(&artifact.parts);
    upsert.version = version.max(1);
    upsert.content_json = Some(serde_json::to_value(artifact).map_err(|error| {
        MissiveError::protocol("encoding A2A artifact for local persistence").with_source(error)
    })?);
    upsert.metadata = artifact_metadata(artifact)?;
    Ok(upsert)
}

fn artifact_metadata(artifact: &Artifact) -> Result<Metadata> {
    let mut metadata = Metadata::new();
    if let Some(description) = &artifact.description {
        metadata.insert_str(ARTIFACT_DESCRIPTION_METADATA_KEY, description.clone())?;
    }
    if let Some(extensions) = &artifact.extensions {
        metadata.insert(ARTIFACT_EXTENSIONS_METADATA_KEY, json!(extensions))?;
    }
    if let Some(remote_metadata) = &artifact.metadata {
        metadata.insert(ARTIFACT_REMOTE_METADATA_KEY, json!(remote_metadata))?;
    }
    Ok(metadata)
}

fn merge_incremental_artifact(
    existing: Option<&ArtifactRecord>,
    update: &Artifact,
) -> Result<Artifact> {
    let Some(existing) = existing else {
        return Ok(update.clone());
    };
    let mut merged = artifact_from_record(existing)?;
    merged.parts.extend(update.parts.clone());
    if update.name.is_some() {
        merged.name = update.name.clone();
    }
    if update.description.is_some() {
        merged.description = update.description.clone();
    }
    if update.metadata.is_some() {
        merged.metadata = update.metadata.clone();
    }
    if update.extensions.is_some() {
        merged.extensions = update.extensions.clone();
    }
    Ok(merged)
}

fn artifact_from_record(record: &ArtifactRecord) -> Result<Artifact> {
    let value = record.content_json.as_ref().ok_or_else(|| {
        MissiveError::validation(format!(
            "artifact {:?} does not contain inline content JSON",
            record.artifact_id.as_str()
        ))
        .with_help("Only artifacts persisted from A2A Task or artifactUpdate payloads can be shown or saved by the current CLI.")
    })?;
    serde_json::from_value::<Artifact>(value.clone()).map_err(|error| {
        MissiveError::protocol(format!(
            "stored artifact {:?} is not valid A2A artifact JSON",
            record.artifact_id.as_str()
        ))
        .with_source(error)
    })
}

fn primary_mime_type(parts: &[Part]) -> Option<String> {
    parts.iter().find_map(|part| part.media_type.clone())
}

fn infer_artifact_kind(parts: &[Part]) -> ArtifactKind {
    if parts
        .iter()
        .any(|part| matches!(part.content, PartContent::Raw(_)))
    {
        ArtifactKind::Bytes
    } else if parts
        .iter()
        .any(|part| matches!(part.content, PartContent::Url(_)))
    {
        ArtifactKind::File
    } else if parts
        .iter()
        .any(|part| matches!(part.content, PartContent::Data(_)))
    {
        ArtifactKind::Json
    } else if parts
        .iter()
        .any(|part| matches!(part.content, PartContent::Text(_)))
    {
        ArtifactKind::Text
    } else {
        ArtifactKind::Unknown
    }
}

fn first_artifact_text(artifact: &Artifact) -> Option<String> {
    artifact
        .parts
        .iter()
        .find_map(|part| part.as_text())
        .map(ToOwned::to_owned)
}

pub(crate) fn first_artifact_text_from_records(artifacts: &[ArtifactRecord]) -> Option<String> {
    artifacts
        .iter()
        .filter_map(|record| artifact_from_record(record).ok())
        .find_map(|artifact| first_artifact_text(&artifact))
}

fn artifact_payload(record: &ArtifactRecord) -> Result<ArtifactPayload> {
    let artifact = artifact_from_record(record)?;
    let raw_parts = artifact
        .parts
        .iter()
        .filter_map(|part| match &part.content {
            PartContent::Raw(bytes) => Some(bytes.as_slice()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !raw_parts.is_empty() {
        let bytes = raw_parts.into_iter().flatten().copied().collect::<Vec<_>>();
        return Ok(ArtifactPayload {
            bytes,
            extension: extension_for_mime(record.mime_type.as_deref()).unwrap_or("bin"),
        });
    }

    let text_parts = artifact
        .parts
        .iter()
        .filter_map(|part| match &part.content {
            PartContent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !text_parts.is_empty() {
        return Ok(ArtifactPayload {
            bytes: text_parts.join("").into_bytes(),
            extension: extension_for_mime(record.mime_type.as_deref()).unwrap_or("txt"),
        });
    }

    let data_parts = artifact
        .parts
        .iter()
        .filter_map(|part| match &part.content {
            PartContent::Data(value) => Some(value.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !data_parts.is_empty() {
        let value = if data_parts.len() == 1 {
            data_parts.into_iter().next().expect("one data part")
        } else {
            Value::Array(data_parts)
        };
        return Ok(ArtifactPayload {
            bytes: json_bytes(&value)?,
            extension: "json",
        });
    }

    let url_parts = artifact
        .parts
        .iter()
        .filter_map(|part| match &part.content {
            PartContent::Url(url) => Some(json!({
                "url": url,
                "filename": part.filename,
                "mediaType": part.media_type,
            })),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !url_parts.is_empty() {
        return Ok(ArtifactPayload {
            bytes: json_bytes(&json!({
                "artifactId": artifact.artifact_id,
                "name": artifact.name,
                "kind": "file_reference",
                "files": url_parts,
            }))?,
            extension: "json",
        });
    }

    Ok(ArtifactPayload {
        bytes: json_bytes(&record.content_json.clone().unwrap_or_else(|| json!({})))?,
        extension: "json",
    })
}

fn json_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        MissiveError::orchestration("encoding artifact JSON for disk export").with_source(error)
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn resolve_single_output_path(record: &ArtifactRecord, output: &Path) -> Result<PathBuf> {
    if output.exists() && output.is_dir() {
        return Ok(output.join(safe_file_name(record)?));
    }
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            MissiveError::io(
                format!(
                    "creating artifact output parent directory {}",
                    parent.display()
                ),
                error,
            )
        })?;
    }
    Ok(output.to_path_buf())
}

fn write_artifact_to_path(
    record: &ArtifactRecord,
    path: &Path,
    force: bool,
) -> Result<ArtifactSavedView> {
    if path.exists() && !force {
        return Err(MissiveError::validation(format!(
            "refusing to overwrite existing artifact output path {}",
            path.display()
        ))
        .with_help("Choose another path or pass --force to overwrite."));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            MissiveError::io(
                format!(
                    "creating artifact output parent directory {}",
                    parent.display()
                ),
                error,
            )
        })?;
    }
    let payload = artifact_payload(record)?;
    fs::write(path, &payload.bytes).map_err(|error| {
        MissiveError::io(format!("writing artifact output {}", path.display()), error)
    })?;
    Ok(ArtifactSavedView {
        artifact_id: record.artifact_id.as_str().to_owned(),
        task_id: record.task_id.as_str().to_owned(),
        path: path.display().to_string(),
        bytes_written: payload.bytes.len() as u64,
        kind: record.kind.as_str().to_owned(),
        mime_type: record.mime_type.clone(),
    })
}

fn safe_file_name(record: &ArtifactRecord) -> Result<String> {
    let payload = artifact_payload(record)?;
    let preferred = record
        .name
        .clone()
        .or_else(|| first_part_filename(record))
        .unwrap_or_else(|| record.artifact_id.as_str().to_owned());
    Ok(sanitize_file_name(
        &preferred,
        record.artifact_id.as_str(),
        payload.extension,
    ))
}

fn unique_safe_file_name(
    record: &ArtifactRecord,
    used_names: &mut BTreeSet<String>,
) -> Result<String> {
    let base = safe_file_name(record)?;
    if used_names.insert(base.clone()) {
        return Ok(base);
    }

    let path = Path::new(&base);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("artifact");
    let extension = path.extension().and_then(|value| value.to_str());
    for suffix in 2_u64.. {
        let candidate = if let Some(extension) = extension {
            format!("{stem}-{suffix}.{extension}")
        } else {
            format!("{stem}-{suffix}")
        };
        if used_names.insert(candidate.clone()) {
            return Ok(candidate);
        }
    }
    unreachable!("unbounded suffix iterator always returns")
}

fn first_part_filename(record: &ArtifactRecord) -> Option<String> {
    artifact_from_record(record)
        .ok()
        .and_then(|artifact| artifact.parts.iter().find_map(|part| part.filename.clone()))
}

fn sanitize_file_name(input: &str, fallback: &str, extension: &str) -> String {
    let leaf = input
        .rsplit(['/', '\\'])
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback);
    let mut sanitized = leaf
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => character,
            _ => '_',
        })
        .collect::<String>()
        .trim_matches('.')
        .to_owned();
    if sanitized.is_empty() {
        sanitized = fallback
            .chars()
            .map(|character| match character {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => character,
                _ => '_',
            })
            .collect();
    }
    if sanitized.is_empty() {
        sanitized = "artifact".to_owned();
    }
    if !extension.is_empty() && Path::new(&sanitized).extension().is_none() {
        sanitized.push('.');
        sanitized.push_str(extension);
    }
    sanitized
}

fn extension_for_mime(mime_type: Option<&str>) -> Option<&'static str> {
    match mime_type.map(|value| value.to_ascii_lowercase()) {
        Some(value) if value == "application/json" || value.ends_with("+json") => Some("json"),
        Some(value) if value == "text/markdown" || value == "text/x-markdown" => Some("md"),
        Some(value) if value.starts_with("text/") => Some("txt"),
        Some(value) if value == "application/octet-stream" => Some("bin"),
        Some(value) if value == "image/png" => Some("png"),
        Some(value) if value == "image/jpeg" => Some("jpg"),
        Some(value) if value == "image/gif" => Some("gif"),
        Some(_) | None => None,
    }
}

fn render_artifact_list<W>(
    writer: &mut W,
    mode: OutputMode,
    output: &ArtifactListOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_artifact_list_human(writer, output),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "task_artifact_list", output, &output.message)
        }
    }
}

fn render_artifact_show<W>(
    writer: &mut W,
    mode: OutputMode,
    output: &ArtifactShowOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_artifact_human(writer, &output.artifact),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "task_artifact_show", output, &output.message)
        }
    }
}

fn render_artifact_save<W>(
    writer: &mut W,
    mode: OutputMode,
    output: &ArtifactSaveOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => writeln!(writer, "{}", redact_text(&output.message))
            .map_err(|error| MissiveError::io("writing artifact save output", error)),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "task_artifact_save", output, &output.message)
        }
    }
}

fn render_artifact_export<W>(
    writer: &mut W,
    mode: OutputMode,
    output: &ArtifactExportOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => writeln!(writer, "{}", redact_text(&output.message))
            .map_err(|error| MissiveError::io("writing artifact export output", error)),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => render_success(
            writer,
            mode,
            "task_artifact_export",
            output,
            &output.message,
        ),
    }
}

fn write_artifact_list_human<W>(writer: &mut W, output: &ArtifactListOutput) -> Result<()>
where
    W: Write,
{
    if output.artifacts.is_empty() {
        return writeln!(
            writer,
            "No artifacts are persisted for task '{}'.",
            redact_text(&output.task_id)
        )
        .map_err(|error| MissiveError::io("writing artifact list output", error));
    }
    writeln!(
        writer,
        "Artifacts for task '{}' ({}):",
        redact_text(&output.task_id),
        output.count
    )
    .map_err(|error| MissiveError::io("writing artifact list output", error))?;
    for artifact in &output.artifacts {
        writeln!(
            writer,
            "  {}  kind={}  version={}  name={}  mime={}",
            redact_text(&artifact.artifact_id),
            redact_text(&artifact.kind),
            artifact.version,
            artifact
                .name
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned()),
            artifact
                .mime_type
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned()),
        )
        .map_err(|error| MissiveError::io("writing artifact list output", error))?;
    }
    Ok(())
}

fn write_artifact_human<W>(writer: &mut W, artifact: &ArtifactDetailView) -> Result<()>
where
    W: Write,
{
    writeln!(
        writer,
        "Artifact {}",
        redact_text(&artifact.summary.artifact_id)
    )
    .map_err(|error| MissiveError::io("writing artifact output", error))?;
    writeln!(writer, "  task: {}", redact_text(&artifact.summary.task_id))
        .map_err(|error| MissiveError::io("writing artifact output", error))?;
    writeln!(writer, "  kind: {}", redact_text(&artifact.summary.kind))
        .map_err(|error| MissiveError::io("writing artifact output", error))?;
    writeln!(writer, "  version: {}", artifact.summary.version)
        .map_err(|error| MissiveError::io("writing artifact output", error))?;
    if let Some(name) = &artifact.summary.name {
        writeln!(writer, "  name: {}", redact_text(name))
            .map_err(|error| MissiveError::io("writing artifact output", error))?;
    }
    if let Some(mime_type) = &artifact.summary.mime_type {
        writeln!(writer, "  mime_type: {}", redact_text(mime_type))
            .map_err(|error| MissiveError::io("writing artifact output", error))?;
    }
    if let Some(preview) = &artifact.summary.text_preview {
        writeln!(writer, "  text: {}", redact_text(preview))
            .map_err(|error| MissiveError::io("writing artifact output", error))?;
    }
    writeln!(writer, "  parts: {}", artifact.parts.len())
        .map_err(|error| MissiveError::io("writing artifact output", error))?;
    for (index, part) in artifact.parts.iter().enumerate() {
        writeln!(
            writer,
            "    {index}: kind={} filename={} mime={} bytes={}",
            redact_text(&part.kind),
            part.filename
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned()),
            part.media_type
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned()),
            part.raw_bytes
                .or(part.text_chars)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_owned())
        )
        .map_err(|error| MissiveError::io("writing artifact output", error))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use missive_store::{AgentUpsert, ContextUpsert, TaskState, TaskUpsert};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn remote_names_are_sanitized_for_export_paths() {
        assert_eq!(
            sanitize_file_name("../../secret/report", "artifact-1", "txt"),
            "report.txt"
        );
        assert_eq!(
            sanitize_file_name("..\\..\\evil.json", "artifact-1", "json"),
            "evil.json"
        );
        assert_eq!(
            sanitize_file_name("../..", "artifact-1", "txt"),
            "artifact-1.txt"
        );
    }

    #[test]
    fn incremental_updates_append_parts_and_increment_version() {
        let mut store = Store::open_in_memory().expect("store");
        let agent = missive_core::AgentAlias::new("echo").expect("agent");
        store
            .upsert_agent(&AgentUpsert::new(agent.clone(), "http://127.0.0.1"))
            .expect("agent");
        let task_id = TaskId::new("task-append").expect("task");
        let context_id = ContextId::new("ctx-append").expect("context");
        store
            .upsert_context(&ContextUpsert::new(context_id))
            .expect("context");
        store
            .upsert_task(&TaskUpsert::new(task_id.clone(), agent, TaskState::Working))
            .expect("task");

        store
            .transaction(|transaction| {
                persist_artifact_update(
                    transaction,
                    &TaskArtifactUpdateEvent {
                        task_id: task_id.as_str().to_owned(),
                        context_id: "ctx-append".to_owned(),
                        artifact: Artifact {
                            artifact_id: "artifact-append".to_owned(),
                            name: Some("unsafe/answer.txt".to_owned()),
                            description: None,
                            parts: vec![Part::text("hello ").with_media_type("text/plain")],
                            metadata: None,
                            extensions: None,
                        },
                        append: Some(false),
                        last_chunk: Some(false),
                        metadata: None,
                    },
                )?;
                persist_artifact_update(
                    transaction,
                    &TaskArtifactUpdateEvent {
                        task_id: task_id.as_str().to_owned(),
                        context_id: "ctx-append".to_owned(),
                        artifact: Artifact {
                            artifact_id: "artifact-append".to_owned(),
                            name: None,
                            description: None,
                            parts: vec![Part::text("world")],
                            metadata: None,
                            extensions: None,
                        },
                        append: Some(true),
                        last_chunk: Some(true),
                        metadata: Some(std::collections::HashMap::from([(
                            "chunk".to_owned(),
                            json!(2),
                        )])),
                    },
                )?;
                Ok(())
            })
            .expect("persist updates");

        let artifact = store
            .get_artifact(&ArtifactId::new("artifact-append").expect("artifact"))
            .expect("get artifact")
            .expect("artifact stored");
        assert_eq!(artifact.version, 2);
        assert_eq!(artifact.kind, ArtifactKind::Text);
        assert_eq!(
            artifact_payload(&artifact).expect("payload").bytes,
            b"hello world"
        );
        assert_eq!(safe_file_name(&artifact).expect("safe name"), "answer.txt");
    }

    #[test]
    fn export_keeps_remote_names_inside_output_directory() {
        let temp = tempdir().expect("tempdir");
        let record = ArtifactRecord {
            artifact_id: ArtifactId::new("artifact-safe").expect("artifact"),
            task_id: TaskId::new("task-safe").expect("task"),
            context_id: None,
            name: Some("../../outside.txt".to_owned()),
            mime_type: Some("text/plain".to_owned()),
            kind: ArtifactKind::Text,
            version: 1,
            content_json: Some(json!({
                "artifactId": "artifact-safe",
                "name": "../../outside.txt",
                "parts": [{"text": "safe"}]
            })),
            bytes_path: None,
            metadata: Metadata::new(),
            created_at: missive_core::MissiveTimestamp::now_utc(),
            updated_at: missive_core::MissiveTimestamp::now_utc(),
        };
        let path = temp.path().join(safe_file_name(&record).expect("filename"));
        write_artifact_to_path(&record, &path, false).expect("write artifact");

        assert_eq!(path.parent(), Some(temp.path()));
        assert_eq!(
            path.file_name().and_then(|value| value.to_str()),
            Some("outside.txt")
        );
        assert_eq!(fs::read_to_string(path).expect("read"), "safe");
    }
}
