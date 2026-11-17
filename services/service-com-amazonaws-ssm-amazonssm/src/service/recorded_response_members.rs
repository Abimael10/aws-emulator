// SPDX-License-Identifier: CC0-1.0

//! The response member the live Systems Manager wire renders on a STEP EXECUTION that this
//! checkout's Smithy model does not declare.
//!
//! # The gap this closes
//!
//! The model a service crate is generated from is a SNAPSHOT of the API, and the live wire
//! has moved past it here: every `StepExecution` the Automation reads render carries
//! `IsStepBlocked`, while the model snapshot declares no such member
//! (`smithy_query(query="shape",shape="StepExecution")` lists `StepName` … `ParentStepDetails`
//! and no `IsStepBlocked`). A client that reads it off a step execution of this emulator
//! finds nothing at all.
//!
//! A member the model does not declare cannot be a field of a generated shape, so it cannot
//! be written from a handler or from the shape's own projection
//! (`crate::state::automation::StepExecutionRecord::to_step_execution`). It is written HERE
//! instead, at the one seam this service renders an operation's answer through
//! ([`crate::service`]'s per-operation wrapper), over the body the framework's own schema
//! serializer just wrote.
//!
//! Nothing here invents a value: a step execution is BLOCKED when the automation cannot run
//! it — the step of a branch the automation did not take, which this emulator's walk never
//! enters at all ([`crate::state::automation`] walks the one path the runbook's `nextStep`
//! and `onFailure` members lay out) — so no step execution this service reports is blocked,
//! and the member says so for every one of them.
//!
//! The body is read and written back as JSON (a member the model does not declare cannot be
//! written through the schema serializer), value by value through `serde_json`'s RAW values,
//! so every value the renderer wrote is written back byte for byte — a timestamp's own
//! digits above all, which no re-rendering of a number may touch. What does change is the
//! order of the members of each object that is rewritten, which carries no meaning on this
//! protocol.
//!
//! What one call does is bounded by the body the renderer just wrote: the step executions it
//! holds are the steps of one runbook (`crate::state::automation::runbook::MAX_RUNBOOK_STEPS`)
//! or one page of them (`crate::state::automation::MAX_STEP_EXECUTION_PAGE_SIZE`), each read
//! once, and nothing here reads a size out of the request.
//!
//! REMOVE THIS MODULE when the model is refreshed to a snapshot that declares the member:
//! the projection then carries it as a field like every other, and `tests/all/automation_stop.rs`
//! pins the wire form either way.

use ::std::collections::BTreeMap;

use ::aws_smithy_runtime_api::http::Response as RtResponse;
use ::aws_smithy_types::body::SdkBody;
use ::serde_json::value::RawValue;

/// The member every step execution the live wire renders carries, and this checkout's model
/// does not declare.
const IS_STEP_BLOCKED: &str = "IsStepBlocked";

/// What the member says of every step execution this service reports: nothing here blocks a
/// step.
const NOT_BLOCKED: &str = "false";

/// One JSON object, each of its members still the bytes it was read as.
type RawObject<'b> = BTreeMap<String, &'b RawValue>;

/// Where each operation that reports step executions carries them in its success body: the
/// member path from the body's root down to the `StepExecutionList`.
///
/// One row per operation whose OUTPUT projects the `StepExecution` shape
/// (`smithy_query(query="operation",operation="GetAutomationExecution,DescribeAutomationStepExecutions")`),
/// so the member is on every step execution this service reports rather than on the ones one
/// read happens to go through.
const STEP_EXECUTION_LISTS: &[(&str, &[&str])] = &[
    (
        "GetAutomationExecution",
        &["AutomationExecution", "StepExecutions"],
    ),
    ("DescribeAutomationStepExecutions", &["StepExecutions"]),
];

/// `body` as this service sends it for `operation` — every step execution in it carrying the
/// member the live wire renders — or `None` when this rule owns nothing about it: another
/// operation's body, a body that is not a JSON object, or one that carries no step execution
/// to write it on.
#[must_use]
pub(super) fn served_success_body(operation: &str, body: &[u8]) -> Option<Vec<u8>> {
    let (_, path) = STEP_EXECUTION_LISTS
        .iter()
        .find(|(named, _)| *named == operation)?;
    serving_object(body, path)
}

/// The JSON object `body` with the step-execution list `path` names rewritten
/// ([`serving_steps`]), or `None` when it holds no such list to rewrite.
///
/// The path is walked one member at a time, each enclosing object read as raw members so
/// only the one the list sits behind is written anew.
fn serving_object(body: &[u8], path: &[&str]) -> Option<Vec<u8>> {
    let mut object: RawObject = ::serde_json::from_slice(body).ok()?;
    let (member, enclosed) = path.split_first()?;
    let held = object.get(*member)?.get().as_bytes();
    let served = if enclosed.is_empty() {
        serving_steps(held)?
    } else {
        serving_object(held, enclosed)?
    };
    let served = raw(&served)?;
    object.insert((*member).to_string(), served);
    ::serde_json::to_vec(&object).ok()
}

/// The JSON list `steps` with the member written onto every step execution in it, or `None`
/// when every one of them already carries it.
fn serving_steps(steps: &[u8]) -> Option<Vec<u8>> {
    let read: Vec<&RawValue> = ::serde_json::from_slice(steps).ok()?;
    let blocked = raw(NOT_BLOCKED.as_bytes())?;
    let mut written = false;
    // Each step's own rewritten bytes, kept alive while the list that borrows them is
    // written.
    let mut rewritten: Vec<Vec<u8>> = Vec::with_capacity(read.len());
    for step in read {
        let mut object: RawObject = ::serde_json::from_str(step.get()).ok()?;
        if !object.contains_key(IS_STEP_BLOCKED) {
            object.insert(IS_STEP_BLOCKED.to_string(), blocked);
            written = true;
        }
        rewritten.push(::serde_json::to_vec(&object).ok()?);
    }
    if !written {
        return None;
    }
    let listed: Vec<&RawValue> = rewritten
        .iter()
        .map(|step| raw(step))
        .collect::<Option<Vec<_>>>()?;
    ::serde_json::to_vec(&listed).ok()
}

/// `bytes` as one raw JSON value, or `None` when they are not one.
fn raw(bytes: &[u8]) -> Option<&RawValue> {
    let text = ::std::str::from_utf8(bytes).ok()?;
    ::serde_json::from_str(text).ok()
}

/// `resp` as this service answers `operation`: a success whose step executions carry the
/// member the live wire renders ([`served_success_body`]).
///
/// A response this rule owns nothing about is returned untouched — status, headers and every
/// byte of the body exactly as the renderer wrote them. A failure is one of those: its body
/// carries no step execution, and this service's refusal wording is [`super::refusal`]'s.
#[must_use]
pub(super) fn with_served_success_body(operation: &str, resp: RtResponse) -> RtResponse {
    if !resp.status().is_success() {
        return resp;
    }
    let rewritten = {
        // A success body of this protocol is built in memory by its renderer, so the bytes
        // are readable here; a streamed body carries no member list and is left alone.
        let Some(bytes) = resp.body().bytes() else {
            return resp;
        };
        match served_success_body(operation, bytes) {
            Some(rewritten) => rewritten,
            None => return resp,
        }
    };
    with_body(resp, rewritten)
}

/// `resp` carrying `body` instead of the bytes its renderer wrote, with a `content-length`
/// the renderer advertised corrected to describe it.
#[must_use]
fn with_body(mut resp: RtResponse, body: Vec<u8>) -> RtResponse {
    // Only a renderer that advertised a length needs it corrected; the framework's renderers
    // set `content-type` alone and leave framing to the transport.
    if resp.headers().get("content-length").is_some() {
        let length = body.len().to_string();
        // A decimal length is always a valid header value, so this cannot fail today; if it
        // ever did, the header is REMOVED rather than left describing the body before the
        // rewrite, so the two can never disagree.
        if resp
            .headers_mut()
            .try_insert("content-length", length)
            .is_err()
        {
            resp.headers_mut().remove("content-length");
        }
    }
    *resp.body_mut() = SdkBody::from(body);
    resp
}

#[cfg(test)]
mod tests {
    //! The rewriting rule, driven on bodies built by hand. The wire behaviour it adds up to
    //! — which operations carry the member, on which reads — is `tests/all/automation_stop.rs`.

    use super::*;
    use ::serde_json::Value;

    /// A response of `status` carrying `body`, with the length its renderer advertised.
    fn response(status: u16, body: &str) -> RtResponse {
        let mut resp = RtResponse::new(
            ::aws_smithy_runtime_api::http::StatusCode::try_from(status)
                .expect("a valid status"),
            SdkBody::from(body),
        );
        resp.headers_mut()
            .insert("content-length", body.len().to_string());
        resp
    }

    /// Every step execution of the body is answered with the member, whichever operation
    /// reported it and however many steps it holds. The rows differ along the operation, the
    /// depth the list sits at and the number of step executions in it.
    #[test]
    fn every_step_execution_a_read_reports_carries_the_served_member() {
        let rows: &[(&str, &str, &str, usize)] = &[
            (
                "one step of an execution read",
                "GetAutomationExecution",
                r#"{"AutomationExecution":{"StepExecutions":[{"StepName":"wait"}]}}"#,
                1,
            ),
            (
                "three steps of an execution read",
                "GetAutomationExecution",
                r#"{"AutomationExecution":{"StepExecutions":[{"StepName":"a"},{"StepName":"b"},
                   {"StepName":"c"}]}}"#,
                3,
            ),
            (
                "a page of the step report",
                "DescribeAutomationStepExecutions",
                r#"{"StepExecutions":[{"StepName":"a"},{"StepName":"b"}],"NextToken":"t"}"#,
                2,
            ),
        ];

        for (case, operation, body, expected) in rows {
            let rewritten = served_success_body(operation, body.as_bytes())
                .unwrap_or_else(|| panic!("case {case:?}: expected a rewritten body"));
            let document: Value =
                ::serde_json::from_slice(&rewritten).expect("the rewritten body is JSON");
            let steps = match *operation {
                "GetAutomationExecution" => &document["AutomationExecution"]["StepExecutions"],
                _ => &document["StepExecutions"],
            }
            .as_array()
            .cloned()
            .unwrap_or_default();
            assert_eq!(
                steps.len(),
                *expected,
                "case {case:?}: expected every step kept, got {document}"
            );
            for step in &steps {
                assert_eq!(
                    step[IS_STEP_BLOCKED],
                    Value::Bool(false),
                    "case {case:?}: expected the served member on every step, got {document}"
                );
            }
        }
    }

    /// Nothing to rewrite: another operation's body, a body holding no step executions, one
    /// that already carries the member, a failure, and a body that is not JSON at all.
    #[test]
    fn a_body_this_rule_does_not_own_is_left_alone() {
        for (case, status, operation, body) in [
            (
                "another operation",
                200,
                "StartAutomationExecution",
                r#"{"AutomationExecutionId":"x"}"#,
            ),
            (
                "an execution with no steps reported",
                200,
                "GetAutomationExecution",
                r#"{"AutomationExecution":{"AutomationExecutionStatus":"Pending"}}"#,
            ),
            (
                "already served",
                200,
                "DescribeAutomationStepExecutions",
                r#"{"StepExecutions":[{"IsStepBlocked":false}]}"#,
            ),
            (
                "a refusal",
                400,
                "GetAutomationExecution",
                r#"{"__type":"AutomationExecutionNotFoundException"}"#,
            ),
            ("not JSON", 200, "GetAutomationExecution", "<Error/>"),
        ] {
            let answered = with_served_success_body(operation, response(status, body));
            assert_eq!(
                answered.body().bytes().expect("bytes"),
                body.as_bytes(),
                "case {case:?}: the body must be byte-identical"
            );
            assert_eq!(
                answered.headers().get("content-length"),
                Some(body.len().to_string().as_str()),
                "case {case:?}: the advertised length must be untouched"
            );
        }
    }

    /// RULE: every value the body already carried is written back byte for byte — the
    /// digits of a timestamp above all, which no re-rendering of a number may touch. The
    /// rows differ along the kind of value sitting beside the step executions.
    #[test]
    fn every_value_the_body_already_carried_is_written_back_unchanged() {
        for (case, operation, body, kept) in [
            (
                "a timestamp of many digits",
                "GetAutomationExecution",
                r#"{"AutomationExecution":{"ExecutionStartTime":1794894495.9506013,"StepExecutions":[{"ExecutionStartTime":1794894495.9506013}]}}"#,
                "1794894495.9506013",
            ),
            (
                "a trailing zero a shorter rendering would drop",
                "DescribeAutomationStepExecutions",
                r#"{"StepExecutions":[{"ExecutionEndTime":1794894495.4231250}]}"#,
                "1794894495.4231250",
            ),
            (
                "a token of its own",
                "DescribeAutomationStepExecutions",
                r#"{"StepExecutions":[{"StepName":"wait"}],"NextToken":"647a"}"#,
                r#""647a""#,
            ),
        ] {
            let rewritten = served_success_body(operation, body.as_bytes())
                .unwrap_or_else(|| panic!("case {case:?}: expected a rewritten body"));
            let rendered = String::from_utf8(rewritten).expect("the rewritten body is text");
            assert!(
                rendered.contains(kept),
                "case {case:?}: expected {kept:?} written back unchanged, got {rendered}"
            );
        }
    }

    /// A rewritten body is described by the `content-length` the renderer advertised.
    #[test]
    fn a_rewritten_body_is_described_by_its_advertised_length() {
        let body = r#"{"StepExecutions":[{"StepName":"wait"}]}"#;
        let answered =
            with_served_success_body("DescribeAutomationStepExecutions", response(200, body));
        let bytes = answered.body().bytes().expect("bytes").to_vec();
        let advertised = answered.headers().get("content-length");
        assert_eq!(
            advertised,
            Some(bytes.len().to_string().as_str()),
            "the advertised length must describe the rewritten body, got {advertised:?}"
        );
    }
}
