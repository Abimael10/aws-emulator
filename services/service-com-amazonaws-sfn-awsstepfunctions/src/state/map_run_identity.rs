// SPDX-License-Identifier: CC0-1.0

//! WHAT a Distributed Map state's Map Run and its CHILD state machine are named by.
//!
//! One concern, held in one file: a Distributed Map state runs its items on a state
//! machine of its own — the `ItemProcessor` — and that child state machine has an
//! identity the API reports.
//!
//! * its LABEL is "a user-defined or an auto-generated string that identifies a `Map`
//!   state" (`MapRunLabel`): the `Label` the state declares, and an identifier this
//!   service generates for a state that declares none;
//! * its ARN is the parent state machine's ARN with that label after a `/` — "the
//!   following qualified state machine ARN refers to a Distributed Map state with a
//!   label `mapStateLabel` in a state machine named `myStateMachine`:
//!   `arn:partition:states:region:account-id:stateMachine:myStateMachine/mapStateLabel`"
//!   (`StartExecution`) — which is the ARN of the state machine the Map Run EXECUTED,
//!   so every member that reports that state machine carries it;
//! * its DEFINITION is the `ItemProcessor`'s own state list, and its TYPE is the
//!   `ExecutionType` that processor's `ProcessorConfig` runs its child workflow
//!   executions as, which is what `DescribeStateMachine` reports for the labeled ARN.
//!
//! The label is derived ONCE here ([`map_state_label`]) and the ARNs are rendered from
//! it ([`child_state_machine_arn`], [`super::map_run_arn`]), so the label a Map Run's
//! own ARN carries and the label of the child state machine it ran can never disagree.
//!
//! What a search of a caller's definition costs is bounded here as well
//! ([`MAX_SCAN_BYTES`], [`MAX_NESTING`]).

use std::collections::BTreeMap;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;

/// How deep into a definition the child state machine of a label is looked for: a
/// `Map`'s processor and a `Parallel`'s branches each nest a state list of their own,
/// and the definition is a caller's. The same ceiling the walk reads a definition under.
const MAX_NESTING: usize = super::asl::MAX_DEFINITION_DEPTH;

/// How many bytes of definition text ONE search reads.
///
/// An emulator bound on the WORK rather than on what is kept, for the reason
/// [`super::asl::echo`] bounds its own scan of a definition: a state list nested inside
/// another is parsed again with it, so the cost is the product of the definition's size
/// and how deeply it nests rather than its size alone. This keeps that product inside a
/// small multiple of one definition's own modelled ceiling (`Definition` is 1 MiB), and a
/// definition nested past the budget resolves no label beyond it.
const MAX_SCAN_BYTES: usize = 4_194_304;

/// The label that identifies the `Map` state `state_name` of the state machine
/// `machine_name`: the `Label` the state declares, and an identifier generated for a
/// state that declares none.
///
/// "A user-defined or an auto-generated string that identifies a `Map` state"
/// (`MapRunLabel`): the user-defined one is the state's own `Label` member, and a state
/// that declares none is identified by a generated one. The generated label is a
/// version-3 UUID — the 36-character form the service generates — over the state machine's
/// name and the state's own name, so it is the SAME label every time that state is
/// entered: a redriven attempt enters the state again and must find the Map Run the
/// earlier attempt started, which is matched by this label.
///
/// A state name is unique only inside its own state list, so two `Map` states of one
/// definition that declare no `Label` and share a name — one inside a `Parallel` branch or
/// a processor, one outside it — are identified by the one generated label. Each still
/// starts a Map Run of its own, told apart by the id its `mapRun` ARN carries and matched
/// by WHICH entry into a state of that label it is, and the labeled state machine ARN they
/// share resolves to the first such state the definition writes ([`child_state_machine`]).
/// A definition that needs them told apart declares a `Label`, which is what the member is
/// for.
pub fn map_state_label(machine_name: &str, state_name: &str, declared: Option<&str>) -> String {
    match declared {
        Some(declared) => declared.to_string(),
        // No namespace: the name below is this emulator's own identity for the state,
        // not a name in one of the namespaces RFC 4122 defines.
        None => ::uuid::Uuid::new_v3(
            &::uuid::Uuid::nil(),
            format!("{machine_name}/{state_name}").as_bytes(),
        )
        .to_string(),
    }
}

/// The ARN of the CHILD state machine a Distributed Map state labelled `label` of the
/// state machine `parent_arn` runs: that ARN with the label after a `/`.
///
/// This is the state machine the Map Run EXECUTED, which is what `ListMapRuns` reports
/// for a Map Run ("the Amazon Resource Name (ARN) of the executed state machine") and
/// what the child workflow executions it started ran. The parent's own unqualified ARN
/// stays the one the store keys the state machine by, so this is DERIVED wherever the
/// child is reported rather than stored in its place.
pub fn child_state_machine_arn(parent_arn: &str, label: &str) -> String {
    format!("{parent_arn}/{label}")
}

/// The ARN of the state machine that RAN the execution whose state machine is
/// `parent_arn` and whose Map Run (if any) is `map_run_arn`.
///
/// A child workflow execution of a Distributed Map state ran on the Map state's own
/// child state machine — its own ARN says so, carrying `<parent name>/<label>` where
/// every other execution carries the state machine's name — so "the state machine that
/// ran the execution" is that labeled ARN, while an execution no Map Run dispatched ran
/// on the state machine itself. The label comes out of the Map Run's own ARN
/// ([`label_of`]), which the execution record carries.
pub fn executed_state_machine_arn(parent_arn: &str, map_run_arn: Option<&str>) -> String {
    match map_run_arn.and_then(label_of) {
        Some(label) => child_state_machine_arn(parent_arn, label),
        None => parent_arn.to_string(),
    }
}

/// The label a Map Run's ARN carries: the `<label>` of
/// `mapRun:<state machine name>/<label>:<id>` ([`super::map_run_arn`]).
pub fn label_of(map_run_arn: &str) -> Option<&str> {
    let (resource, _id) = map_run_arn.rsplit_once(':')?;
    let (_machine, label) = resource.rsplit_once('/')?;
    Some(label).filter(|label| !label.is_empty())
}

/// The CHILD state machine a labeled state machine ARN names: what
/// `DescribeStateMachine` reports for the `Map` state that label identifies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildStateMachine {
    /// The `ItemProcessor`'s OWN definition — its state list, with the
    /// `ProcessorConfig` that configures how the parent runs it dropped — reported with
    /// the insignificant whitespace between its tokens removed and its member order and
    /// number literals as the definition wrote them, exactly as every other document
    /// this service was given as text is.
    pub definition: String,
    /// `ProcessorConfig.ExecutionType`, "the execution type [...] its child workflow
    /// executions run as", where the processor names one; `None` leaves the type to the
    /// parent state machine's own.
    pub execution_type: Option<String>,
}

/// The child state machine the label `label` names in the definition of the state
/// machine `machine_name`, or `None` when no Distributed Map state of it carries that
/// label.
///
/// The search covers the WHOLE definition — a `Map`'s processor and a `Parallel`'s
/// branches nest state lists of their own, and a label identifies a `Map` state wherever
/// it is written — and only a DISTRIBUTED processor answers: an inline `Map` runs its
/// items inside the execution rather than on a state machine of its own, so there is no
/// child state machine for a label of one to name.
///
/// `machine_name` is the state machine's own name, because a state that declares no
/// `Label` is identified by the label generated from where it sits
/// ([`map_state_label`]) — the same derivation the walk that runs the state makes.
///
/// The search reads at most [`MAX_SCAN_BYTES`] of definition text and descends at most
/// [`MAX_NESTING`] levels, so no definition makes one report cost more than that; the
/// FIRST state carrying the label answers.
pub fn child_state_machine(
    definition: &str,
    machine_name: &str,
    label: &str,
) -> Option<ChildStateMachine> {
    let mut budget = MAX_SCAN_BYTES;
    child_in(definition, machine_name, label, 0, &mut budget)
}

/// The same, for the state list `states_text` at nesting `depth`, charging what it reads
/// against `budget`.
fn child_in(
    states_text: &str,
    machine_name: &str,
    label: &str,
    depth: usize,
    budget: &mut usize,
) -> Option<ChildStateMachine> {
    if depth > MAX_NESTING || *budget == 0 {
        return None;
    }
    *budget = budget.saturating_sub(states_text.len());
    let document: BTreeMap<String, Box<RawValue>> = serde_json::from_str(states_text).ok()?;
    let states: BTreeMap<String, Box<RawValue>> =
        serde_json::from_str(document.get("States")?.get()).ok()?;
    for (state_name, state) in &states {
        let Ok(members) = serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(state.get())
        else {
            continue;
        };
        match member_text(&members, "Type").as_deref() {
            Some("Map") => {
                let Some(processor) = members
                    .get("ItemProcessor")
                    .or_else(|| members.get("Iterator"))
                else {
                    continue;
                };
                let processor_members =
                    serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(processor.get())
                        .unwrap_or_default();
                let config = processor_members
                    .get("ProcessorConfig")
                    .and_then(|config| {
                        serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(config.get()).ok()
                    })
                    .unwrap_or_default();
                // "ProcessorConfig [...] Mode": only a DISTRIBUTED processor runs its
                // items on a child state machine of its own.
                let distributed = member_text(&config, "Mode")
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("DISTRIBUTED"));
                let declared = member_text(&members, "Label");
                let state_label = map_state_label(machine_name, state_name, declared.as_deref());
                if distributed && state_label == label {
                    return Some(ChildStateMachine {
                        definition: child_definition(processor.get()),
                        execution_type: member_text(&config, "ExecutionType"),
                    });
                }
                // A Distributed Map nested inside this one is a `Map` state of the
                // processor's own state list.
                if let Some(found) =
                    child_in(processor.get(), machine_name, label, depth + 1, budget)
                {
                    return Some(found);
                }
            }
            Some("Parallel") => {
                let branches = members
                    .get("Branches")
                    .and_then(|branches| {
                        serde_json::from_str::<Vec<Box<RawValue>>>(branches.get()).ok()
                    })
                    .unwrap_or_default();
                for branch in &branches {
                    if let Some(found) =
                        child_in(branch.get(), machine_name, label, depth + 1, budget)
                    {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// The DEFINITION of the child state machine an `ItemProcessor` is: the processor's own
/// text with the `ProcessorConfig` member — which configures how the PARENT runs the
/// processor rather than being part of the state machine it runs — dropped.
///
/// The members that stay keep the order the definition wrote them in and the text they
/// were written as, with only the insignificant whitespace between their tokens removed
/// ([`::service_common::json_document::minified`]).
fn child_definition(processor_text: &str) -> String {
    let Some(members) = ordered_members(processor_text) else {
        return ::service_common::json_document::minified(processor_text);
    };
    let kept: Vec<String> = members
        .iter()
        .filter(|(name, _)| name != "ProcessorConfig")
        .map(|(name, value)| {
            format!(
                "{}:{}",
                ::service_common::json_document::minified(&quoted(name)),
                ::service_common::json_document::minified(value.get())
            )
        })
        .collect();
    format!("{{{}}}", kept.join(","))
}

/// `name` as the JSON string literal a member name is written as.
fn quoted(name: &str) -> String {
    serde_json::Value::String(name.to_string()).to_string()
}

/// The string value of the member `member`, for a member whose value is a JSON string.
fn member_text(members: &BTreeMap<String, Box<RawValue>>, member: &str) -> Option<String> {
    serde_json::from_str::<String>(members.get(member)?.get()).ok()
}

/// The members of the JSON object `text`, in the ORDER the document writes them, each
/// with its own text.
///
/// `serde_json`'s object is a `BTreeMap` unless its `preserve_order` feature is on, so a
/// parse sorts the members by name; the order is read off the same maintained parser
/// through a visitor that keeps each member's own text ([`RawValue`]) instead.
fn ordered_members(text: &str) -> Option<Vec<(String, Box<RawValue>)>> {
    serde_json::from_str::<OrderedMembers>(text)
        .ok()
        .map(|OrderedMembers(members)| members)
}

/// The members of one JSON object, in the order they are written.
struct OrderedMembers(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for OrderedMembers {
    fn deserialize<D: Deserializer<'de>>(reader: D) -> Result<Self, D::Error> {
        struct Members;

        impl<'de> Visitor<'de> for Members {
            type Value = Vec<(String, Box<RawValue>)>;

            fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                out.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut members: A) -> Result<Self::Value, A::Error> {
                let mut written = Vec::new();
                while let Some(member) = members.next_entry::<String, Box<RawValue>>()? {
                    written.push(member);
                }
                Ok(written)
            }
        }

        reader.deserialize_map(Members).map(OrderedMembers)
    }
}
