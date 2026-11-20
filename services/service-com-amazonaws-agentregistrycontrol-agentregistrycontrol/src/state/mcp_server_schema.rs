// SPDX-License-Identifier: CC0-1.0

//! Whether an MCP server descriptor's content is a document one of the MCP server schema
//! versions this service supports accepts, and how this service words a refusal of one that
//! matches none.
//!
//! `McpServerDescriptor` is "Descriptor that defines the content of an MCP (Model Context
//! Protocol) server registry record, including the server definition and its tool
//! definitions. The content is validated against the MCP protocol schema", and its
//! `dataSchemaVersion` is "The schema version of the descriptor payload". So the payload is
//! matched against the MCP server schema VERSIONS this service supports before the record is
//! written: a payload no version accepts describes no MCP server, so it is refused rather
//! than stored and published to consumers that would then fail to resolve it.
//!
//! A version is told from the others by the members its schema REQUIRES — the MCP server
//! definition's identity ([`SUPPORTED_VERSIONS`] names the revision each one is published
//! under) — so matching is deciding whether the payload satisfies one of those schemas, and
//! matching none is one refusal rather than one per version: nothing in the payload says
//! which version it was written for.
//!
//! The documents are JSON Schemas of this module's own and the evaluator is the one a
//! registry's custom-metadata schema is applied with ([`crate::state`]'s `custom_metadata`),
//! so no schema here is caller-supplied, carries a reference, or is read from anywhere
//! outside this file; the only caller-supplied document is the payload itself, whose size the
//! model bounds (`DescriptorData`, length 1..102400).
//!
//! The payload is `@sensitive`, so nothing of it is written into the refusal or into a log:
//! the refusal names the descriptor path whose content was matched and says that no version
//! matched, and nothing else.

use ::serde_json::Value;

use crate::ops::types::Descriptors;

/// The path this service names the MCP server descriptor's content by when it refuses it —
/// the descriptor's own protocol family and the member of it that was matched, not anything
/// taken from the request.
const DESCRIPTOR_PATH: &str = "mcp.server";

/// How this service refuses an MCP server payload that no supported schema version accepts.
/// The ONE place this sentence is written, so a create and an edit word the same error class
/// the same way. It is fixed text throughout: the payload is `@sensitive` and no single
/// version is the one it failed, so there is nothing of the request to state.
fn version_mismatch_message() -> String {
    format!("{DESCRIPTOR_PATH} data does not match any supported version")
}

/// One version of the MCP server schema a payload is matched against.
struct SupportedVersion {
    /// The revision the Model Context Protocol publishes this version of its server schema
    /// under, as the protocol dates its revisions. Reported in a log line when a payload
    /// matches, so a developer can see WHICH version admitted it.
    revision: &'static str,
    /// The schema itself, as a JSON Schema document: the members this revision of the MCP
    /// server schema requires of a server definition, and the types it declares for them. A
    /// member the revision does not require is left unconstrained — a server definition
    /// carries the optional members of its own revision (its packages, remotes, repository
    /// and the rest) and a payload is matched on what makes it that revision's definition.
    schema: &'static str,
}

/// The MCP server schema versions this service supports, newest revision first — the order
/// a payload is matched in, so the newest version that accepts it is the one reported.
///
/// Both revisions require a server definition to carry a name, a description and a version;
/// they differ in where the version sits, which the protocol moved out of a nested
/// `version_detail` object into a `version` member of the definition itself.
const SUPPORTED_VERSIONS: [SupportedVersion; 2] = [
    SupportedVersion {
        revision: "2025-09-29",
        schema: r#"{
            "type": "object",
            "required": ["name", "description", "version"],
            "properties": {
                "name": { "type": "string", "minLength": 1 },
                "description": { "type": "string", "minLength": 1 },
                "version": { "type": "string", "minLength": 1 }
            }
        }"#,
    },
    SupportedVersion {
        revision: "2025-07-09",
        schema: r#"{
            "type": "object",
            "required": ["name", "description", "version_detail"],
            "properties": {
                "name": { "type": "string", "minLength": 1 },
                "description": { "type": "string", "minLength": 1 },
                "version_detail": {
                    "type": "object",
                    "required": ["version"],
                    "properties": { "version": { "type": "string", "minLength": 1 } }
                }
            }
        }"#,
    },
];

/// The refusal message for the MCP server descriptor of `descriptors` when its content
/// matches no supported MCP server schema version, or `None` when it matches one — and when
/// the set carries no MCP server descriptor, or one carrying no content at all (a
/// source-only descriptor, whose content is synchronized from the source rather than
/// supplied).
///
/// Called with the descriptors a request would leave on the record, so a create and an edit
/// admit the same payloads. A payload that is not even JSON is refused by the parse this
/// module's caller runs first ([`super::descriptor_payload`]): it matches no schema either,
/// but what a caller has to be told about it is where it stopped parsing.
pub fn refusal(descriptors: Option<&Descriptors>) -> Option<String> {
    let payload = descriptors?.mcp_server.as_ref()?.data.as_deref()?;
    let content = ::serde_json::from_str::<Value>(payload).ok()?;
    match matched_revision(&content) {
        Some(revision) => {
            tracing::debug!(
                mcp_server_schema_revision = revision,
                "an MCP server descriptor's content matched a supported schema version"
            );
            None
        }
        None => Some(version_mismatch_message()),
    }
}

/// The revision of the first supported version whose schema accepts `content`, or `None`
/// where none of them does.
fn matched_revision(content: &Value) -> Option<&'static str> {
    compiled()
        .iter()
        .find(|ready| ready.schemas.validate(content, ready.index).is_ok())
        .map(|ready| ready.revision)
}

/// One supported version's schema, compiled.
struct CompiledVersion {
    /// The revision it is the schema of.
    revision: &'static str,
    /// The compiled document.
    schemas: ::boon::Schemas,
    /// Where it sits in `schemas`.
    index: ::boon::SchemaIndex,
}

/// Every supported version's schema, compiled ONCE for the process: the documents are this
/// file's own and never change, so compiling them per request would be work no request
/// decides anything about.
fn compiled() -> &'static [CompiledVersion] {
    static COMPILED: ::std::sync::OnceLock<Vec<CompiledVersion>> = ::std::sync::OnceLock::new();
    COMPILED.get_or_init(|| SUPPORTED_VERSIONS.iter().filter_map(compile).collect())
}

/// One supported version's schema, compiled, or `None` where this emulator's own document
/// does not compile.
///
/// A schema of this module that does not compile accepts nothing: it is this service's own
/// document, so a version that cannot be applied is a defect here rather than anything the
/// request did, and claiming a payload matched a version that was never applied would be a
/// verdict nothing gave.
fn compile(supported: &'static SupportedVersion) -> Option<CompiledVersion> {
    let Ok(document) = ::serde_json::from_str::<Value>(supported.schema) else {
        tracing::error!(
            mcp_server_schema_revision = supported.revision,
            "this emulator's own MCP server schema document is not JSON"
        );
        return None;
    };
    let url = schema_url(supported.revision);
    let mut compiler = ::boon::Compiler::new();
    if compiler.add_resource(&url, document).is_err() {
        tracing::error!(
            mcp_server_schema_revision = supported.revision,
            "this emulator's own MCP server schema document is no schema"
        );
        return None;
    }
    let mut schemas = ::boon::Schemas::new();
    let Ok(index) = compiler.compile(&url, &mut schemas) else {
        tracing::error!(
            mcp_server_schema_revision = supported.revision,
            "this emulator's own MCP server schema document does not compile"
        );
        return None;
    };
    Some(CompiledVersion {
        revision: supported.revision,
        schemas,
        index,
    })
}

/// The URL one supported version's schema is compiled as a resource under: the descriptor
/// path and the revision it is the schema of, in the same namespace a registry's
/// custom-metadata schema is compiled under. It is a NAME, not a location: the document is
/// the one this file holds, it carries no reference, and nothing is ever resolved or fetched
/// for it.
fn schema_url(revision: &str) -> String {
    format!("https://agent-registry.amazonaws.com/descriptors/{DESCRIPTOR_PATH}/{revision}")
}
