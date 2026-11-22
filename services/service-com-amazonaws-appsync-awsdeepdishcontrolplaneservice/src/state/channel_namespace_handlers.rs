// SPDX-License-Identifier: CC0-1.0

//! The APPSYNC_JS `codeHandlers` a channel namespace is defined by: the compile every call
//! that stores them runs, and how handlers the runtime cannot read are refused.
//!
//! `codeHandlers` is "the event handler functions that run custom business logic to process
//! published events and subscribe requests" (`ChannelNamespace$codeHandlers`, targeting
//! `Code`, `@length` 1..32768 — `smithy_query(query="shape", shape="ChannelNamespace")`),
//! an APPSYNC_JS module run by the same runtime that runs a resolver's `code`
//! ([`crate::service::event_api::handlers`]). Handlers that runtime cannot read define
//! nothing that could ever process an event, so AppSync compiles the module as the
//! namespace is written and refuses the call instead of storing them: `BadRequestException`
//! (HTTP 400) carrying [`CODE_HANDLERS_MESSAGE`]. Nothing is stored for a refused call, so
//! a later read and a later listing report only the namespaces that were accepted — and a
//! CloudFormation stack that declared one is failed by that refusal rather than reporting a
//! namespace whose handlers cannot run.
//!
//! The compile is a REAL compile, by the very engine that RUNS the handlers in this process
//! ([`compile_faults`]): nothing is read out of the module's TEXT, so what is refused is
//! what the engine could not read, under that engine's own memory and stack ceilings and
//! the wall clock and step budget its interrupt handler spends. Nothing of the module is
//! evaluated: the compile stops at the compiled module.
//!
//! The rule is over the MEMBER rather than over one operation, so `CreateChannelNamespace`
//! and `UpdateChannelNamespace` refuse the same modules; a request that carries no
//! `codeHandlers` defines nothing to compile.
//!
//! What this compile does NOT judge is which functions the module EXPORTS. A module the
//! engine reads to the end is stored, and a namespace whose module declares neither
//! handler serves a publish and a subscribe exactly as a namespace with no handlers at all
//! does ([`crate::service::event_api::handlers::HandlerRun::NotDeclared`]) — the same split
//! the written `code` of a resolver is judged by ([`super::code_definition`]).

use crate::ops::evaluate_code::appsync_js::{compile_faults, Compiled};
use crate::ops::types::BadRequestException;

/// The message a call refused for handlers the runtime could not read carries: AppSync's
/// own fixed sentence for this member, which names nothing of the request.
pub const CODE_HANDLERS_MESSAGE: &str = "Error processing APPSYNC_JS code.";

/// What the `codeHandlers` a call would store came to, or [`super::CodeDefinition::Compiled`]
/// for a request that carries none, which defines nothing to compile.
///
/// The refusal is the modelled `BadRequestException` so each operation raises it through its
/// own error channel; a compile that did not happen at all is
/// [`super::CodeDefinition::Unknown`], which the caller answers as the server fault its
/// model declares rather than storing handlers nothing could read.
pub async fn channel_namespace_handlers(code_handlers: Option<&str>) -> super::CodeDefinition {
    let Some(code) = code_handlers else {
        return super::CodeDefinition::Compiled;
    };
    let faults = match compile_faults(code.to_string()).await {
        Compiled::Read(faults) => faults,
        Compiled::Unknown(reason) => return super::CodeDefinition::Unknown(reason),
    };
    if faults.is_empty() {
        return super::CodeDefinition::Compiled;
    }
    // The faults themselves are not reported on this member: the surface states the one
    // sentence above, where the written `code` of a resolver carries a `CodeError` per
    // fault in its `detail`.
    super::CodeDefinition::Refused(BadRequestException {
        message: Some(CODE_HANDLERS_MESSAGE.to_string()),
        ..Default::default()
    })
}
