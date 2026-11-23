// SPDX-License-Identifier: CC0-1.0

//! The APPSYNC_JS `code` a resolver or a function is DEFINED by: the read every call that
//! stores one runs over it, and how code that does not compile — or that breaks a rule of the
//! runtime's own rule set — is refused.
//!
//! `code` is "The `resolver` code that contains the request and response functions"
//! (`Resolver$code`, and `FunctionConfiguration$code` for a function), run by the
//! APPSYNC_JS runtime. A definition the runtime could not run defines nothing, so the call
//! that would store it is refused instead: AppSync reads the code as the resolver or function
//! is written and answers its `BadRequestException` (HTTP 400) with the `CODE_ERROR` reason,
//! for which "the detail will contain a list of code errors" (`BadRequestDetail`) — one
//! `CodeError` per diagnostic, carrying the diagnostic as the compiler or the broken rule
//! worded it and where in the code it sits. Nothing is stored for a refused call, so a later
//! read and a later listing report only the definitions that were accepted.
//!
//! How the code is read — the compile and the rule set, what each diagnostic is worded like,
//! and the bounds the read happens under — is [`super::code_diagnostics`], shared by every
//! operation that stores `code` (`CreateFunction`, `UpdateFunction`, `CreateResolver`,
//! `UpdateResolver`), so one definition is judged one way whichever call wrote it; a request
//! that carries no `code` defines nothing to read.
//!
//! What this read does NOT judge is which functions the module EXPORTS. A written definition
//! is refused only for code that does not compile or that breaks a rule, because that is what
//! the corpus records this plane refusing: `EvaluateCode` is the call recorded reporting a
//! module that exports no `request`/`response` pair as faulty
//! (`crate::ops::evaluate_code`), and it reports that as the evaluation's own `error` rather
//! than as a refusal of the request. A create or an update of a definition is left exactly as
//! recorded until a recording says otherwise.

use crate::ops::evaluate_code::appsync_js::{CodeFault, CODE_UNIT};
use crate::ops::types::{BadRequestDetail, BadRequestException, CodeError, CodeErrorLocation};

use super::code_diagnostics::{code_diagnostics, Diagnosed};

/// The `errorType` a fault of a WRITTEN definition the COMPILE found is reported under,
/// re-exported here beside the refusal that carries it; a fault the RULE SET found carries
/// [`super::LINT_ERROR`] instead.
pub use crate::ops::evaluate_code::appsync_js::PARSE_ERROR;

/// The `reason` a code fault is reported under: "Provides context for the cause of the
/// bad request. The only supported value is `CODE_ERROR`"
/// (`BadRequestException$reason`).
pub const CODE_ERROR_REASON: &str = "CODE_ERROR";

/// The message a call refused for code it could not read carries: AppSync's own fixed
/// sentence, which names nothing of the request — the faults themselves are reported in
/// the `detail`, one per fault.
pub const CODE_ERRORS_MESSAGE: &str = "The code contains one or more errors.";

/// The faults `code` carries as the modelled `BadRequestDetail`: "Contains the list of
/// errors in the request" (`BadRequestDetail$codeErrors`), one entry per fault.
///
/// The single projection every refusal that reports code faults answers with, so the
/// pre-flight evaluation and the create or update of a definition cannot disagree about
/// how one fault is reported.
pub fn code_errors_detail(faults: &[CodeFault]) -> BadRequestDetail {
    BadRequestDetail {
        code_errors: Some(code_errors(faults)),
    }
}

/// The faults `code` carries as the modelled `CodeError` list, which both the member that
/// reports them in a REFUSAL ([`code_errors_detail`]) and the one that reports them in an
/// evaluation's own `error` (`EvaluateCodeErrorDetail$codeErrors`) carry.
pub fn code_errors(faults: &[CodeFault]) -> Vec<CodeError> {
    faults.iter().map(code_error).collect()
}

/// One fault as the modelled `CodeError` reports it: its type, the user-presentable error
/// the compiler worded, and where in the code it sits.
fn code_error(fault: &CodeFault) -> CodeError {
    CodeError {
        error_type: Some(fault.error_type.to_string()),
        value: Some(fault.value.clone()),
        location: Some(CodeErrorLocation {
            line: fault.line,
            column: fault.column,
            span: fault.span,
        }),
    }
}

/// What a definition's `code` came to, as the operation that would store it has to answer
/// it.
#[derive(Debug)]
pub enum CodeDefinition {
    /// The code compiled and broke no rule: the definition may be stored.
    Compiled,
    /// It did not: the refusal to answer the call with.
    Refused(BadRequestException),
    /// The read did not happen or did not come back, so nothing is known about the code: the
    /// text says why. A definition nothing could read is not stored — the call
    /// answers the server fault its model declares instead, because storing it would be
    /// indistinguishable from storing one that compiled.
    Unknown(String),
}

/// What the `code` a call would store came to, or [`CodeDefinition::Compiled`] for a
/// request that carries none, which defines nothing to read.
///
/// The refusal is the modelled `BadRequestException` so each operation raises it through
/// its own error channel, with the reason and the per-fault detail a client reads to place
/// each fault in the code it sent.
pub async fn code_definition(code: Option<&str>) -> CodeDefinition {
    let Some(code) = code else {
        return CodeDefinition::Compiled;
    };
    match code_diagnostics(code.to_string()).await {
        Diagnosed::Read => CodeDefinition::Compiled,
        Diagnosed::Unknown(reason) => CodeDefinition::Unknown(reason),
        Diagnosed::Faults(faults) => CodeDefinition::Refused(BadRequestException {
            message: Some(CODE_ERRORS_MESSAGE.to_string()),
            reason: Some(CODE_ERROR_REASON.to_string()),
            detail: Some(code_errors_detail(&faults)),
        }),
    }
}

/// `fault` as the ENGINE that ran the code states one: its position, and the engine's own
/// account of what it could not read.
///
/// A diagnostic is stated as `<unit>(<line>,<column>): error <code>: <sentence>`, on a line
/// of its own, the unit being the one the code is compiled under ([`CODE_UNIT`]) — the form
/// a client reads it in. The engine names no diagnostic CODE of its own, so the code is left
/// out of the scaffolding and the sentence follows `error:` directly. The position is the one
/// the engine stated, or, for a fault the engine states none for, where the code ITSELF ends
/// ([`end_of_input`]), which is as far as a run that could not read the code got.
///
/// This is the wording of a fault the EVALUATION sandbox found while reading code it was
/// about to run (`crate::ops::evaluate_code`); a WRITTEN definition is read by the compiler
/// and the rule set instead, which state their own diagnostics
/// (`super::code_diagnostics`).
pub fn stated_code_fault(fault: &CodeFault, code: &str) -> CodeFault {
    let (line, column) = if fault.line > 0 {
        (fault.line, fault.column)
    } else {
        end_of_input(code)
    };
    CodeFault {
        error_type: fault.error_type,
        value: format!("{CODE_UNIT}({line},{column}): error: {}\n", fault.value),
        line,
        column,
        span: POSITION_SPAN,
    }
}

/// The `span` a fault of a written definition carries: `CodeErrorLocation$span` is "The
/// span/length of the error" and declares `0` as its default, which is what a diagnostic
/// about a POSITION rather than about a stretch of code carries.
const POSITION_SPAN: i32 = 0;

/// Where `code` ends, as a line and a column counted from one: the position just past its
/// last character.
///
/// The position a compile that ran out of input stopped at, which the text itself states —
/// as many lines as it holds, and as many characters as its last line carries, plus one for
/// the position after them. A text ending in a line terminator ends at the start of the
/// line after its last, which is where the next character would go.
fn end_of_input(code: &str) -> (i32, i32) {
    if code.is_empty() {
        return (1, 1);
    }
    let lines = i32::try_from(code.lines().count()).unwrap_or(i32::MAX);
    if code.ends_with('\n') {
        return (lines.saturating_add(1), 1);
    }
    let last = code.rsplit_once('\n').map_or(code, |(_, last)| last);
    let column = i32::try_from(last.chars().count().saturating_add(1)).unwrap_or(i32::MAX);
    (lines, column)
}
