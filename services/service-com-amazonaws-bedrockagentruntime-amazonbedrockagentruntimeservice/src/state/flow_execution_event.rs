// SPDX-License-Identifier: CC0-1.0

//! The EVENTS of one flow execution: what happened during the run a start really made, which
//! is what `ListFlowExecutionEvents` reports.
//!
//! An event is never composed for this listing: it is read back from the trace the run left
//! on the execution ([`super::flow_invocation::FlowRunTrace`]), so every node name, field
//! name, declared type and expression an event carries is one the stored flow declares, and
//! every document one carries is the one the start was given.
//!
//! ## Which events a listing selects
//!
//! `eventType` picks the PLANE the events belong to ([`EventPlane`]): the flow's own input
//! and output, or the inputs and outputs of the nodes it ran. The two planes report the same
//! run from the two sides the model declares for it, so neither ever carries the other's
//! events.
//!
//! ## When an event occurred
//!
//! An event is reported only once it HAS occurred:
//!
//! * what the start did — the flow receiving its input, and the input node handing that
//!   document on — carries the instant the execution started, so it is reported from the
//!   moment it exists;
//! * what the END of the run did — an output of the flow, a node receiving the value, the
//!   failure of a node or of the flow — carries the instant the execution ended, and is
//!   reported only once the execution really has ended (`super::flow_execution`: the status
//!   the clock owes is materialised before any read answers). Nothing is reported for work
//!   the run has not finished doing.
//!
//! So the events of one execution are only ever APPENDED to as its run progresses, which is
//! what lets a cursor into them stay valid across pages.
//!
//! An execution that was stopped reports nothing past its start: its run was aborted before
//! it delivered anything, so there is no output and no failure of it to report.

use ::aws_smithy_types::DateTime;

use crate::ops::types::{
    FlowExecutionContent, FlowExecutionEvent, FlowExecutionInputEvent, FlowExecutionOutputEvent,
    FlowFailureEvent, FlowInputField, FlowOutputField, NodeExecutionContent, NodeFailureEvent,
    NodeInputEvent, NodeInputField, NodeInputSource, NodeOutputEvent, NodeOutputField,
    NodeOutputNext,
};

use super::flow_execution::{FlowExecution, FlowExecutionStatus};
use super::flow_invocation::{FlowRunHop, FlowRunTrace};

/// The most events one page of a listing carries.
///
/// The modelled `FlowExecutionEvents` list is `@length(max: 10)`
/// (`smithy_query(query="operation",operation="ListFlowExecutionEvents")`), so a page cannot
/// hand back more than ten however large a `maxResults` asks for; a listing with more to give
/// hands out a cursor instead. It is also the page size a request that names none takes.
pub const MAX_EVENTS_PER_PAGE: usize = 10;

/// Which side of the run a listing asks for, as the modelled `FlowExecutionEventType`
/// declares it (`smithy_query(query="shape",shape="FlowExecutionEventType")` -> Node, Flow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventPlane {
    /// The flow's own input and output, and a failure of the flow.
    Flow,
    /// The inputs and outputs of the nodes the run went through, and a failure of one.
    Node,
}

impl EventPlane {
    /// The plane an `eventType` names, or `None` for a value the enum does not declare.
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "Flow" => Some(Self::Flow),
            "Node" => Some(Self::Node),
            _ => None,
        }
    }

    /// The value this plane is named by, which is what a pagination cursor is scoped to: a
    /// cursor into one plane's events means nothing in the other's.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Flow => "Flow",
            Self::Node => "Node",
        }
    }
}

/// The `NodeErrorCode` a node whose work nothing in this emulator performs earns.
///
/// The failure is the DEPENDENCY's: the model, function or other worker the node's kind
/// delegates to is what did not answer, not the request (`smithy_query(query="shape",
/// shape="NodeErrorCode")` -> VALIDATION, DEPENDENCY_FAILED, BAD_GATEWAY, INTERNAL_SERVER).
/// It is the class `StartFlowExecution` refuses an unreachable input node with, too.
const NODE_DEPENDENCY_FAILED: &str = "DEPENDENCY_FAILED";

/// The `FlowErrorCode` a flow whose run failed AT A NODE earns
/// (`smithy_query(query="shape",shape="FlowErrorCode")` -> VALIDATION, INTERNAL_SERVER,
/// NODE_EXECUTION_FAILED).
const FLOW_NODE_EXECUTION_FAILED: &str = "NODE_EXECUTION_FAILED";

/// See [`FLOW_NODE_EXECUTION_FAILED`]: the code a run that failed on the flow's own wiring
/// earns, where no node of it ran at all — the definition does not carry the input to an
/// output, so what is invalid is the flow rather than any node of it.
const FLOW_VALIDATION: &str = "VALIDATION";

/// The events of `execution` on `plane`, in the order they occurred.
///
/// Derived, so two readers of the same execution read the same events in the same order, and
/// an execution whose run has since finished reports the events of that ending as well.
pub fn events_of(execution: &FlowExecution, plane: EventPlane) -> Vec<FlowExecutionEvent> {
    match plane {
        EventPlane::Flow => flow_events(execution),
        EventPlane::Node => node_events(execution),
    }
}

/// The flow-level events: the input the flow was entered with, and what the flow itself came
/// to.
fn flow_events(execution: &FlowExecution) -> Vec<FlowExecutionEvent> {
    let trace = &execution.trace;
    let mut events = Vec::new();
    // The field the flow's input arrived as is the one the flow's own input schema names; a
    // definition that names none names no field for this event, and `fields` is modelled
    // `@length(min: 1)`, so there is no event to report rather than one with a field the
    // flow does not declare.
    if let Some(name) = trace.input_field.clone() {
        events.push(FlowExecutionEvent::flowInputEvent(FlowExecutionInputEvent {
            node_name: trace.input_node.clone(),
            timestamp: execution.started_at,
            fields: vec![FlowInputField {
                name,
                content: FlowExecutionContent::Document(trace.document.clone()),
            }],
        }));
    }
    let Some(ended) = ended_at(execution) else {
        return events;
    };
    match execution.status {
        // Every value the run delivered left the flow at an output node, in the field that
        // node's definition declares it arrived in.
        FlowExecutionStatus::Succeeded => {
            for hop in &trace.hops {
                let Some(name) = hop.target_input.clone() else {
                    continue;
                };
                events.push(FlowExecutionEvent::flowOutputEvent(
                    FlowExecutionOutputEvent {
                        node_name: hop.target_node.clone(),
                        timestamp: ended,
                        fields: vec![FlowOutputField {
                            name,
                            content: FlowExecutionContent::Document(trace.document.clone()),
                        }],
                    },
                ));
            }
        }
        FlowExecutionStatus::Failed => {
            if let Some(message) = execution.failure.clone() {
                events.push(FlowExecutionEvent::flowFailureEvent(FlowFailureEvent {
                    timestamp: ended,
                    error_code: flow_error_code(trace).to_string(),
                    error_message: message,
                }));
            }
        }
        // A run that was stopped, and one this emulator mints no status for, produced
        // nothing past its start: see the module docs.
        _ => {}
    }
    events
}

/// The node-level events: what the input node handed on, what each node it reached received,
/// and the failure of a node that could not be run.
fn node_events(execution: &FlowExecution) -> Vec<FlowExecutionEvent> {
    let trace = &execution.trace;
    let mut events = Vec::new();
    if let Some(name) = trace.input_field.clone() {
        events.push(FlowExecutionEvent::nodeOutputEvent(NodeOutputEvent {
            node_name: trace.input_node.clone(),
            timestamp: execution.started_at,
            fields: vec![NodeOutputField {
                name,
                content: NodeExecutionContent::Document(trace.document.clone()),
                // Where that output goes next: the nodes the flow's own connections carry it
                // to, and the input of each that it arrives at.
                next: next_of(&trace.hops),
                r#type: trace.input_field_type.clone(),
            }],
        }));
    }
    let Some(ended) = ended_at(execution) else {
        return events;
    };
    match execution.status {
        FlowExecutionStatus::Succeeded => {
            for hop in &trace.hops {
                let Some(name) = hop.target_input.clone() else {
                    continue;
                };
                events.push(FlowExecutionEvent::nodeInputEvent(NodeInputEvent {
                    node_name: hop.target_node.clone(),
                    timestamp: ended,
                    fields: vec![NodeInputField {
                        name,
                        content: NodeExecutionContent::Document(trace.document.clone()),
                        source: source_of(trace, hop),
                        r#type: hop.target_input_type.clone(),
                        category: hop.target_input_category.clone(),
                        // Nothing this emulator runs nests a node inside an iterator or a
                        // loop (`super::flow_invocation`: a run leaving the input node for
                        // any node kind but an output is what fails), so no run has an
                        // execution path through nested nodes to report.
                        execution_chain: None,
                    }],
                }));
            }
        }
        FlowExecutionStatus::Failed => {
            // Reported for the node the failure IS about: a run that failed on the flow's
            // own wiring ran no node, so there is no node of it that failed.
            if let (Some(node_name), Some(message)) =
                (trace.failed_node.clone(), execution.failure.clone())
            {
                events.push(FlowExecutionEvent::nodeFailureEvent(NodeFailureEvent {
                    node_name,
                    timestamp: ended,
                    error_code: NODE_DEPENDENCY_FAILED.to_string(),
                    error_message: message,
                }));
            }
        }
        _ => {}
    }
    events
}

/// The instant the run ENDED, for an execution that has ended.
///
/// `None` while it is still running, which is what keeps the events of an ending out of a
/// listing until that ending has happened.
fn ended_at(execution: &FlowExecution) -> Option<DateTime> {
    execution
        .ended_at
        .filter(|_| execution.status.is_terminal())
}

/// Where the input node's output goes next: one entry per hop the flow's connections carry
/// it along, for those the connection names an input of its target for.
fn next_of(hops: &[FlowRunHop]) -> Option<Vec<NodeOutputNext>> {
    let next: Vec<NodeOutputNext> = hops
        .iter()
        .filter_map(|hop| {
            hop.target_input.clone().map(|input_field_name| NodeOutputNext {
                node_name: hop.target_node.clone(),
                input_field_name,
            })
        })
        .collect();
    (!next.is_empty()).then_some(next)
}

/// Where one node's input came from: the node that produced the value, the output of it the
/// value left by, and the expression that input extracts its data with.
///
/// All three members are modelled required, so the source is reported only when the stored
/// flow names all three; a connection that names no fields reports no source rather than one
/// with a name nothing declared.
fn source_of(trace: &FlowRunTrace, hop: &FlowRunHop) -> Option<NodeInputSource> {
    Some(NodeInputSource {
        node_name: trace.input_node.clone(),
        output_field_name: hop.source_output.clone()?,
        expression: hop.target_input_expression.clone()?,
    })
}

/// The `FlowErrorCode` a failed run earns: the node one failed at, else the flow's own
/// wiring.
fn flow_error_code(trace: &FlowRunTrace) -> &'static str {
    if trace.failed_node.is_some() {
        FLOW_NODE_EXECUTION_FAILED
    } else {
        FLOW_VALIDATION
    }
}
