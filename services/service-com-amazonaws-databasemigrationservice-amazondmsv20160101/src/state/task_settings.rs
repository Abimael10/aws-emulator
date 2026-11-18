// SPDX-License-Identifier: CC0-1.0

//! The task-settings document a replication task runs with.
//!
//! `CreateReplicationTask$ReplicationTaskSettings` is OPTIONAL, and a request that carries
//! one may name only the settings the caller cares about. DMS resolves everything the
//! request left unsaid against its own service defaults when it creates the task, and from
//! then on reports the RESOLVED document on every `ReplicationTask` it answers with — which
//! is what makes "read the task's settings back" the documented way to discover the
//! logging, error behaviour and buffering the task will actually run with, and what makes a
//! settings round trip keep the defaults the caller never named.
//!
//! This module owns that resolution, so the ONE projection every operation answers a task
//! through ([`super::ReplicationTaskRecord::to_shape`]) has a full document to report:
//!
//! * [`default_task_settings`] — the document a task created with no settings at all runs
//!   with, with one [`LOG_COMPONENTS`] entry per DMS log component;
//! * [`resolve_task_settings`] — a caller's document merged OVER those defaults, section by
//!   section and member by member, so a partial section keeps every default it did not
//!   name.

use ::serde_json::{Map, Value};

/// The severity every log component is logged at until a task's settings say otherwise:
/// the `Logging` task setting's own default severity.
pub const LOG_SEVERITY_DEFAULT: &str = "LOGGER_SEVERITY_DEFAULT";

/// The log components a DMS task's `Logging` setting carries one entry per.
///
/// The catalog DMS publishes for the `Logging.LogComponents` task setting: a task's default
/// settings document names every one of them, each at [`LOG_SEVERITY_DEFAULT`], which is
/// what lets a caller raise the severity of one component by naming only that component.
pub const LOG_COMPONENTS: &[&str] = &[
    "DATA_STRUCTURE",
    "COMMUNICATION",
    "IO",
    "COMMON",
    "FILE_FACTORY",
    "FILE_TRANSFER",
    "REST_SERVER",
    "ADDONS",
    "TARGET_LOAD",
    "TARGET_APPLY",
    "SOURCE_UNLOAD",
    "SOURCE_CAPTURE",
    "TRANSFORMATION",
    "SORTER",
    "TASK_MANAGER",
    "TABLES_MANAGER",
    "METADATA_MANAGER",
    "PERFORMANCE",
    "VALIDATOR_EXT",
];

/// Where the log-component list is spliced into [`DEFAULT_TASK_SETTINGS_TEMPLATE`].
const LOG_COMPONENTS_PLACEHOLDER: &str = "<log-components>";

/// The default task-settings document, less the log-component list.
///
/// The sections and the values are the task-settings defaults DMS publishes for a task
/// created without settings: every section a default document carries is present, and a
/// section DMS fills in only when a task asks for it is reported as `null` rather than
/// left out. The document is compact JSON, which is how the member travels on the wire —
/// `ReplicationTaskSettings` is a `String`, not a structure.
const DEFAULT_TASK_SETTINGS_TEMPLATE: &str = concat!(
    r#"{"Logging":{"EnableLogging":false,"EnableLogContext":false,"LogComponents":"#,
    "<log-components>",
    r#","CloudWatchLogGroup":null,"CloudWatchLogStream":null},"#,
    r#""StreamBufferSettings":{"StreamBufferCount":3,"StreamBufferSizeInMB":8,"#,
    r#""CtrlStreamBufferSizeInMB":5},"#,
    r#""ErrorBehavior":{"DataErrorPolicy":"LOG_ERROR","DataTruncationErrorPolicy":"LOG_ERROR","#,
    r#""DataErrorEscalationPolicy":"SUSPEND_TABLE","DataErrorEscalationCount":0,"#,
    r#""TableErrorPolicy":"SUSPEND_TABLE","TableErrorEscalationPolicy":"STOP_TASK","#,
    r#""TableErrorEscalationCount":0,"RecoverableErrorCount":-1,"RecoverableErrorInterval":5,"#,
    r#""RecoverableErrorThrottling":true,"RecoverableErrorThrottlingMax":1800,"#,
    r#""RecoverableErrorFailOnTruncationDdl":false,"ApplyErrorDeletePolicy":"IGNORE_RECORD","#,
    r#""ApplyErrorInsertPolicy":"LOG_ERROR","ApplyErrorUpdatePolicy":"LOG_ERROR","#,
    r#""ApplyErrorEscalationPolicy":"LOG_ERROR","ApplyErrorEscalationCount":0,"#,
    r#""ApplyErrorFailOnTruncationDdl":false,"FullLoadIgnoreConflicts":true,"#,
    r#""FailOnTransactionConsistencyBreached":false,"FailOnNoTablesCaptured":true},"#,
    r#""ChangeProcessingDdlHandlingPolicy":{"HandleSourceTableDropped":true,"#,
    r#""HandleSourceTableTruncated":true,"HandleSourceTableAltered":true},"#,
    r#""ValidationSettings":{"EnableValidation":false,"ValidationMode":"ROW_LEVEL","#,
    r#""ThreadCount":5,"PartitionSize":10000,"FailureMaxCount":10000,"#,
    r#""RecordFailureDelayInMinutes":5,"RecordFailureDelayLimitInMinutes":0,"#,
    r#""RecordSuspendDelayInMinutes":30,"MaxKeyColumnSize":8096,"TableFailureMaxCount":1000,"#,
    r#""ValidationOnly":false,"HandleCollationDiff":false,"SkipLobColumns":false,"#,
    r#""ValidationPartialLobSize":0,"ValidationQueryCdcDelaySeconds":0},"#,
    r#""TTSettings":{"EnableTT":false,"TTS3Settings":null,"TTRecordSettings":null},"#,
    r#""FailTaskWhenCleanTaskResourceFailed":false,"#,
    r#""FullLoadSettings":{"TargetTablePrepMode":"DO_NOTHING","CreatePkAfterFullLoad":false,"#,
    r#""StopTaskCachedChangesApplied":false,"StopTaskCachedChangesNotApplied":false,"#,
    r#""MaxFullLoadSubTasks":8,"TransactionConsistencyTimeout":600,"CommitRate":10000},"#,
    r#""TargetMetadata":{"TargetSchema":"","SupportLobs":true,"FullLobMode":false,"#,
    r#""LobChunkSize":64,"LimitedSizeLobMode":true,"LobMaxSize":32,"InlineLobMaxSize":0,"#,
    r#""LoadMaxFileSize":0,"ParallelLoadThreads":0,"ParallelLoadBufferSize":0,"#,
    r#""ParallelLoadQueuesPerThread":0,"ParallelApplyThreads":0,"ParallelApplyBufferSize":0,"#,
    r#""ParallelApplyQueuesPerThread":0,"BatchApplyEnabled":false,"#,
    r#""TaskRecoveryTableEnabled":false},"#,
    r#""BeforeImageSettings":null,"#,
    r#""ControlTablesSettings":{"ControlSchema":"","HistoryTimeslotInMinutes":5,"#,
    r#""HistoryTableEnabled":false,"SuspendedTablesTableEnabled":false,"#,
    r#""StatusTableEnabled":false,"FullLoadExceptionTableEnabled":false},"#,
    r#""LoopbackPreventionSettings":null,"#,
    r#""CharacterSetSettings":null,"#,
    r#""ChangeProcessingTuning":{"BatchApplyPreserveTransaction":true,"BatchApplyTimeoutMin":1,"#,
    r#""BatchApplyTimeoutMax":30,"BatchApplyMemoryLimit":500,"BatchSplitSize":0,"#,
    r#""MinTransactionSize":1000,"CommitTimeout":1,"MemoryLimitTotal":1024,"MemoryKeepTime":60,"#,
    r#""StatementCacheSize":50},"#,
    r#""PostProcessingRules":null}"#,
);

/// The settings document a replication task created with none of its own runs with.
///
/// One `Logging.LogComponents` entry per component in [`LOG_COMPONENTS`], each at
/// [`LOG_SEVERITY_DEFAULT`], spliced into the rest of the defaults.
pub fn default_task_settings() -> String {
    DEFAULT_TASK_SETTINGS_TEMPLATE.replace(LOG_COMPONENTS_PLACEHOLDER, &log_components())
}

/// The `Logging.LogComponents` list of a default document.
fn log_components() -> String {
    let entries: Vec<String> = LOG_COMPONENTS
        .iter()
        .map(|id| format!(r#"{{"Severity":"{LOG_SEVERITY_DEFAULT}","Id":"{id}"}}"#))
        .collect();
    format!("[{}]", entries.join(","))
}

/// The settings a task created with `supplied` runs with: the document the caller sent
/// resolved against the service defaults.
///
/// A request that names no settings gets the defaults whole. One that names some gets them
/// merged over the defaults section by section and member by member — so naming one
/// member of one section changes that member and nothing else — and a member the defaults
/// do not publish is kept as the caller wrote it, since task settings are an open document.
///
/// `Err` carries the reason a document could not be resolved at all: settings that are not
/// a JSON object name no setting, so there is nothing to merge.
pub fn resolve_task_settings(supplied: Option<&str>) -> Result<String, String> {
    let Some(supplied) = supplied else {
        return Ok(default_task_settings());
    };
    let overlay = match ::serde_json::from_str::<Value>(supplied) {
        Ok(Value::Object(overlay)) => overlay,
        Ok(other) => {
            return Err(format!(
                "ReplicationTaskSettings must be a JSON object of task settings, and the \
                 document supplied is {}",
                json_kind(&other)
            ))
        }
        Err(error) => {
            return Err(format!(
                "ReplicationTaskSettings must be a readable JSON document: {error}"
            ))
        }
    };
    let mut resolved = default_object();
    merge(&mut resolved, overlay);
    Ok(Value::Object(resolved).to_string())
}

/// The defaults as a JSON object.
///
/// [`DEFAULT_TASK_SETTINGS_TEMPLATE`] is a constant of this module and the splice only
/// fills in a list, so the document always reads as an object; an empty map is the reading
/// of a document that somehow did not, which leaves a caller's own settings intact rather
/// than refusing a create over this module's own constant.
fn default_object() -> Map<String, Value> {
    match ::serde_json::from_str::<Value>(&default_task_settings()) {
        Ok(Value::Object(document)) => document,
        _ => Map::new(),
    }
}

/// Merge `from` over `into`: a member both documents hold as an object is merged member by
/// member, and every other member the caller named replaces the default outright.
///
/// The depth is bounded by the parse that produced `from`, which refuses a document nested
/// deeper than the reader's own recursion limit.
fn merge(into: &mut Map<String, Value>, from: Map<String, Value>) {
    for (key, overlay) in from {
        match (into.remove(&key), overlay) {
            (Some(Value::Object(mut section)), Value::Object(overlay)) => {
                merge(&mut section, overlay);
                into.insert(key, Value::Object(section));
            }
            (_, overlay) => {
                into.insert(key, overlay);
            }
        }
    }
}

/// What a document that is not an object is, for the refusal that names it.
fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default document reads as a JSON object, carries every section a default
    /// document has, and logs every component of the catalog at the default severity.
    #[test]
    fn the_default_document_carries_every_default_section() {
        let document: Value = ::serde_json::from_str(&default_task_settings())
            .expect("the default task-settings document is readable JSON");
        let object = document
            .as_object()
            .expect("the default task-settings document is a JSON object");
        for section in [
            "Logging",
            "StreamBufferSettings",
            "ErrorBehavior",
            "ChangeProcessingDdlHandlingPolicy",
            "ValidationSettings",
            "TTSettings",
            "FullLoadSettings",
            "TargetMetadata",
            "BeforeImageSettings",
            "ControlTablesSettings",
            "LoopbackPreventionSettings",
            "CharacterSetSettings",
            "ChangeProcessingTuning",
            "PostProcessingRules",
        ] {
            assert!(
                object.contains_key(section),
                "case {section:?}: the default document must carry the section, got {document}"
            );
        }

        let logged: Vec<String> = document
            .pointer("/Logging/LogComponents")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        format!(
                            "{}:{}",
                            entry.get("Id").and_then(Value::as_str).unwrap_or("<none>"),
                            entry
                                .get("Severity")
                                .and_then(Value::as_str)
                                .unwrap_or("<none>")
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let expected: Vec<String> = LOG_COMPONENTS
            .iter()
            .map(|id| format!("{id}:{LOG_SEVERITY_DEFAULT}"))
            .collect();
        assert_eq!(
            logged, expected,
            "case \"log components\": every component must be logged at the default \
             severity, got {document}"
        );
    }

    /// A caller's document is merged OVER the defaults: the members it names change, the
    /// members of the same section it does not name keep their defaults, and a member no
    /// default publishes is kept.
    #[test]
    fn a_supplied_document_keeps_the_defaults_it_does_not_name() {
        let supplied = r#"{"TargetMetadata":{"LobChunkSize":128},"ExtraSetting":{"Deep":1}}"#;
        let resolved: Value = ::serde_json::from_str(
            &resolve_task_settings(Some(supplied)).expect("a readable document resolves"),
        )
        .expect("the resolved document is readable JSON");

        for (pointer, expected) in [
            ("/TargetMetadata/LobChunkSize", Value::from(128)),
            ("/TargetMetadata/LobMaxSize", Value::from(32)),
            ("/TargetMetadata/SupportLobs", Value::from(true)),
            ("/ExtraSetting/Deep", Value::from(1)),
            ("/Logging/EnableLogging", Value::from(false)),
            ("/ErrorBehavior/DataErrorPolicy", Value::from("LOG_ERROR")),
        ] {
            assert_eq!(
                resolved.pointer(pointer).cloned().unwrap_or(Value::Null),
                expected,
                "case {pointer:?}: expected {expected}, got {resolved}"
            );
        }
    }

    /// Settings that are not a JSON object name no setting, so they are refused rather
    /// than stored as the caller wrote them.
    #[test]
    fn a_document_that_is_not_an_object_is_refused() {
        for case in [
            "not json at all",
            "[]",
            "7",
            "\"a string\"",
            "{\"unbalanced\":",
        ] {
            assert!(
                resolve_task_settings(Some(case)).is_err(),
                "case {case:?}: expected a refusal, got {:?}",
                resolve_task_settings(Some(case))
            );
        }
    }
}
