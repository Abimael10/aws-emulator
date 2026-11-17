// SPDX-License-Identifier: CC0-1.0

//! Taking the next step of an act: what the service asks its caller to execute, which
//! results it accepts back, and what it keeps of each.
//!
//! An act is driven by its CLIENT. The service hands back the tool calls it wants executed
//! and the caller returns their results on the next step, matching each result to the call
//! it answers by `callId` — "A unique identifier for this tool call, used to match results
//! back to requests"
//! (`smithy_query(query="doc",shape="Call",member="callId")`), and on the result side "The
//! identifier of the tool call that this result corresponds to". Both sides of that match
//! live here: the record of every call the service has handed out ([`ActExecution`]), and
//! the rule that a result naming a call it never handed out is refused
//! ([`unmatched_result`]).
//!
//! The calls themselves come from what the act DECLARED. `CreateAct` stores the tool
//! specifications the act may invoke ("A list of tool specifications that the act can
//! invoke to complete its task"), so a step asks for the first of those tools no earlier
//! step has asked for ([`next_tool`]), carrying an input built from that tool's own
//! declared input schema ([`schema_instance`]). An act whose declared tools have all been
//! asked for has nothing further for its caller to execute, so its next step hands back no
//! calls at all — which is what tells the caller the act is done.

use std::time::SystemTime;

use ::aws_smithy_types::{Document, Number};

use crate::ops::types::{
    Call, CallResult, ToolInputSchema, ToolSpec, ValidationException, ValidationExceptionField,
};

/// The value `ValidationExceptionReason` declares for a member that failed validation
/// (`smithy_query(query="shape",shape="ValidationExceptionReason")`).
const FIELD_VALIDATION_FAILED: &str = "FieldValidationFailed";

/// How far into a tool's declared input schema an instance is built.
///
/// The schema is a caller-supplied document of arbitrary nesting, so the walk that
/// instantiates it is bounded here rather than by how deeply a caller nested it: below this
/// depth a sub-schema instantiates to `null`.
const MAX_SCHEMA_DEPTH: usize = 16;

/// One step taken on an act.
#[derive(Debug, Clone)]
pub struct ActStep {
    /// The identifier this step was minted with, which the step's answer reports and a
    /// later step may name as its `previousStepId`.
    pub step_id: String,
    /// The step the request said this one follows, when it named one.
    pub previous_step_id: Option<String>,
    pub taken_at: SystemTime,
    /// The calls this step handed the caller to execute, in the order it handed them over.
    pub calls: Vec<Call>,
    /// The calls whose results the request that took this step carried, in the order it
    /// carried them — `None` for a result naming no call, which the model allows
    /// (`CallResult$callId` is optional) for a result that answers no outstanding call.
    ///
    /// The result CONTENT is not kept: the step that processes it consumes it, no operation
    /// this model declares reports it back, and the only thing a later step resolves against
    /// is the identifier. That keeps what one act holds bounded by how many calls it
    /// answered rather than by how much text a caller submitted with them.
    pub answered_calls: Vec<Option<String>>,
}

/// Every step taken on one act, oldest first: the act's execution record.
///
/// BOUNDED by the act itself, without a cap of its own: a step asks for one of the tools the
/// act declared ([`next_tool`]) and the step after the last of them asks for nothing, which
/// finishes the act — and a finished act takes no further step. So one act holds at most one
/// step per declared tool plus that closing one, and `ToolSpecs` is modelled `len ..100`
/// (`smithy_query(query="shape",shape="ToolSpecs")`).
#[derive(Debug, Default)]
pub struct ActExecution {
    steps: Vec<ActStep>,
}

impl ActExecution {
    /// The steps taken on the act, oldest first.
    pub fn steps(&self) -> &[ActStep] {
        &self.steps
    }

    /// How many calls the act has handed its caller across every step, which is also how
    /// many of its declared tools have been asked for.
    pub fn calls_requested(&self) -> usize {
        self.steps.iter().map(|step| step.calls.len()).sum()
    }

    /// Whether `step_id` names a step taken on this act.
    pub fn holds_step(&self, step_id: &str) -> bool {
        self.steps.iter().any(|step| step.step_id == step_id)
    }

    /// Whether `call_id` names a call this act asked its caller to execute.
    pub fn requested_call(&self, call_id: &str) -> bool {
        self.steps
            .iter()
            .flat_map(|step| step.calls.iter())
            .any(|call| call.call_id == call_id)
    }

    /// Append a step to the act's record. The ONE place a step is recorded, so what the
    /// step answered and what the act holds cannot differ.
    pub fn record(&mut self, step: ActStep) {
        self.steps.push(step);
    }
}

/// The tool the act's next step asks its caller to invoke: the first tool the act declared
/// that no earlier step has asked for, and `None` once every declared tool has been asked
/// for.
///
/// One call per step, so each result the next step carries answers a call that step
/// requested, and an act that declared no tools asks for nothing at all.
pub fn next_tool<'a>(tool_specs: &'a [ToolSpec], execution: &ActExecution) -> Option<&'a ToolSpec> {
    tool_specs.get(execution.calls_requested())
}

/// The call a step hands back for `spec`, under the identifier the service minted for it.
pub fn call_for(spec: &ToolSpec, call_id: String) -> Call {
    Call {
        call_id,
        name: spec.name.clone(),
        input: schema_instance(&spec.input_schema),
    }
}

/// The input a call carries: an instance of the input schema its tool declared.
///
/// `ToolSpec$inputSchema` is "The JSON schema that defines the expected input format for the
/// tool" (a `ToolInputSchema` union whose `json` member holds that schema), so a call's
/// input is built FROM the schema rather than invented: one member per property the schema
/// declares, each carrying the value the schema itself fixes for it — `const`, else
/// `default`, else the first value of an `enum` — and otherwise the empty value of the type
/// the schema declares for it. A schema that declares no properties instantiates to an empty
/// object, which is what a tool taking no input expects.
pub fn schema_instance(schema: &ToolInputSchema) -> Document {
    let ToolInputSchema::Json(document) = schema;
    instance_of(document, 0)
}

/// An instance of one (sub-)schema, `depth` levels below the tool's own schema.
fn instance_of(schema: &Document, depth: usize) -> Document {
    if depth >= MAX_SCHEMA_DEPTH {
        return Document::Null;
    }
    // Anything but a schema object says nothing about what an instance of it looks like.
    let Document::Object(keywords) = schema else {
        return Document::Null;
    };
    // A value the schema fixes is the instance, whatever type it declares.
    if let Some(fixed) = keywords.get("const") {
        return fixed.clone();
    }
    if let Some(default) = keywords.get("default") {
        return default.clone();
    }
    if let Some(Document::Array(values)) = keywords.get("enum") {
        if let Some(first) = values.first() {
            return first.clone();
        }
    }
    match keywords.get("type") {
        Some(Document::String(declared)) => match declared.as_str() {
            "object" => object_instance(keywords, depth),
            "array" => Document::Array(Vec::new()),
            "string" => Document::String(String::new()),
            "number" | "integer" => Document::Number(Number::PosInt(0)),
            "boolean" => Document::Bool(false),
            // "null", and any word a JSON schema's `type` does not name.
            _ => Document::Null,
        },
        // A schema may describe an object by its properties alone; anything else — a type
        // given as a list of types, or no type at all — fixes no instance.
        _ if keywords.contains_key("properties") => object_instance(keywords, depth),
        _ => Document::Null,
    }
}

/// An instance of an object schema: one member per property it declares.
fn object_instance(
    keywords: &::std::collections::HashMap<String, Document>,
    depth: usize,
) -> Document {
    let Some(Document::Object(properties)) = keywords.get("properties") else {
        return Document::Object(::std::collections::HashMap::new());
    };
    Document::Object(
        properties
            .iter()
            .map(|(name, property)| (name.clone(), instance_of(property, depth + 1)))
            .collect(),
    )
}

/// The first entry of `results` naming a call this act never asked for, as the member path
/// that names that entry.
///
/// A result carrying no `callId` answers no call by construction — the model leaves that
/// member optional — so it matches nothing and is not refused.
pub fn unmatched_result(results: &[CallResult], execution: &ActExecution) -> Option<String> {
    results.iter().enumerate().find_map(|(index, result)| {
        let call_id = result.call_id.as_deref()?;
        (!execution.requested_call(call_id)).then(|| format!("callResults[{index}].callId"))
    })
}

/// AWS's sentence for a request whose input this service cannot use.
///
/// It states the CLASS of failure and nothing of the request: which member to fix, and what
/// is wrong with it, is what the `fieldList` entry carries. That is the split this service's
/// own recordings show for an input-validation refusal, and it is why this refusal does not
/// go through [`crate::state::paging::field_validation_failed`], which states one sentence in
/// both places for the refusals whose message AWS words per member.
pub const INVALID_INPUT: &str = "The provided input is invalid or malformed.";

/// The modelled refusal for an identifier the act cannot resolve: HTTP 400
/// `ValidationException`, `reason` `FieldValidationFailed` — the value
/// `ValidationExceptionReason` declares for a member that failed validation — with the class
/// sentence as its message and one `fieldList` entry naming the member.
///
/// `member` is the member path, and `names` what the identifier was supposed to name, so one
/// finding states the rule for every identifier of this kind.
pub fn unresolved_identifier(member: &str, names: &str) -> ValidationException {
    ValidationException {
        message: INVALID_INPUT.to_string(),
        reason: FIELD_VALIDATION_FAILED.to_string(),
        field_list: Some(vec![ValidationExceptionField {
            name: member.to_string(),
            message: format!("{member} does not identify {names}"),
        }]),
    }
}
