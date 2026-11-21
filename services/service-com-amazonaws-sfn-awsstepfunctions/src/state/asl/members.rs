// SPDX-License-Identifier: CC0-1.0

//! WHICH members a state of a definition may carry: per state type, per query language,
//! and — for a `Task` — per kind of work its `Resource` names.
//!
//! One concern read in two places, so the sets live here rather than beside either
//! reader: the walk refuses to RUN a state carrying a member this emulator does not
//! interpret, and `TestState` refuses to inspect one for the same reason.

use super::*;

/// The members each executed state type may carry. A state that carries any other
/// member is not executed: the member changes what the state does, and running the
/// state while ignoring it would report an outcome the definition does not describe.
///
/// `Assign` is a member of BOTH query languages — the block that stores the execution's
/// variables, written as that language's own expressions: a payload template here
/// ([`Interpreter::jsonpath_assign`]) and a JSONata template in the other
/// ([`jsonata_members`]) — so every type that may carry it in one may carry it in the
/// other. The two types that END an execution carry none of it: a variable is assigned
/// for a LATER state to read, and nothing runs after a `Succeed` or a `Fail`. A `Fail`
/// state carries none of the two languages' data-flow members at all either.
///
/// A `Task`'s members depend on WHICH work its `Resource` names, so the type alone
/// does not decide them: this answers the ACTIVITY task's set, and
/// [`INTEGRATION_TASK_MEMBERS`] the service-integration call's.
/// [`unexecutable_member`] is what picks between the two.
pub(super) fn executable_members(state_type: &str) -> &'static [&'static str] {
    match state_type {
        "Pass" => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "Assign",
            "InputPath",
            "Parameters",
            "Result",
            "ResultPath",
            "OutputPath",
            "Next",
            "End",
        ],
        "Wait" => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "Assign",
            "InputPath",
            "OutputPath",
            "Seconds",
            "SecondsPath",
            "Timestamp",
            "TimestampPath",
            "Next",
            "End",
        ],
        "Choice" => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "Assign",
            "InputPath",
            "OutputPath",
            "Choices",
            "Default",
        ],
        "Succeed" => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "InputPath",
            "OutputPath",
        ],
        "Fail" => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "Error",
            "ErrorPath",
            "Cause",
            "CausePath",
        ],
        // An ACTIVITY task's own work is done by a worker, so the members below are
        // the ones that decide what is scheduled for it, what the state does with the
        // reply, and what happens when the task fails or runs out of time: a `Retry`
        // re-schedules the task for its next attempt, which is a task a worker polls
        // for exactly as the attempt before it was, and a `Catch` carries the execution
        // on at the catcher it names — applied by the CONTINUATION of the state the task
        // closed ([`super::super::callback_timeout`]), which walks the states after the
        // catcher in a chain of its own rather than inside whatever request closed the
        // task. `Credentials` is absent for the reason it is on every other type: it
        // changes who the invocation is made as.
        "Task" => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "Assign",
            "Resource",
            "InputPath",
            "Parameters",
            "TimeoutSeconds",
            "TimeoutSecondsPath",
            "HeartbeatSeconds",
            "HeartbeatSecondsPath",
            "ResultSelector",
            "ResultPath",
            "OutputPath",
            "Retry",
            "Catch",
            "Next",
            "End",
        ],
        // `ResultWriter` is interpreted too: it WRITES an S3 dataset, and the manifest
        // of result files it writes is written there for real
        // ([`super::result_writer`]), as `ItemReader` and `ItemBatcher` are — the
        // dataset is read from S3 ([`super::item_reader`]) and its items are grouped as
        // declared ([`super::item_batcher`]).
        "Map" => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "Assign",
            "InputPath",
            "Parameters",
            "ItemReader",
            "ItemBatcher",
            "ItemSelector",
            "ItemsPath",
            "Items",
            "ItemProcessor",
            "Iterator",
            "MaxConcurrency",
            "MaxConcurrencyPath",
            "ResultSelector",
            "ResultWriter",
            "ResultPath",
            "OutputPath",
            "Retry",
            "Catch",
            "ToleratedFailureCount",
            "ToleratedFailureCountPath",
            "ToleratedFailurePercentage",
            "ToleratedFailurePercentagePath",
            "Label",
            "Next",
            "End",
        ],
        "Parallel" => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "Assign",
            "InputPath",
            "Parameters",
            "Branches",
            "ResultSelector",
            "ResultPath",
            "OutputPath",
            "Retry",
            "Catch",
            "Next",
            "End",
        ],
        _ => &[],
    }
}

/// The members a `Task` state whose `Resource` names a SERVICE INTEGRATION may
/// carry: the ones that decide the request the call carries, what becomes of its
/// answer, and how a failed call is handled.
///
/// `HeartbeatSeconds`/`HeartbeatSecondsPath` is among them: the member is a `Task`
/// state's, whatever work its `Resource` names, and the window it declares closes at
/// its own length for a task nothing heartbeats — which is every task whose work this
/// emulator performs itself ([`Interpreter::task_state`] holds such a task to whichever
/// of the two spans the state declared runs out first). `Credentials` is deliberately
/// absent: it asks for the call to be made under another role, which this emulator does
/// not assume.
pub(super) const INTEGRATION_TASK_MEMBERS: &[&str] = &[
    "Type",
    "Comment",
    "QueryLanguage",
    "Assign",
    "Resource",
    "InputPath",
    "Parameters",
    "ResultSelector",
    "ResultPath",
    "OutputPath",
    "Retry",
    "Catch",
    "TimeoutSeconds",
    "TimeoutSecondsPath",
    "HeartbeatSeconds",
    "HeartbeatSecondsPath",
    "Next",
    "End",
];

/// The members a `Task` state asking for the callback (`.waitForTaskToken`) pattern
/// may carry: an integration task's, plus the heartbeat this one CAN owe.
///
/// "If more time than the specified seconds elapses between heartbeats from the task,
/// this state fails with a States.Timeout error name": the answer to a callback task
/// comes from outside the execution, so the span between the `SendTaskHeartbeat`
/// calls that report it still running is a span this pattern has — which the
/// request-response form, finished by its own call, does not.
pub(super) const CALLBACK_TASK_MEMBERS: &[&str] = &[
    "Type",
    "Comment",
    "QueryLanguage",
    "Assign",
    "Resource",
    "InputPath",
    "Parameters",
    "ResultSelector",
    "ResultPath",
    "OutputPath",
    "Retry",
    "Catch",
    "TimeoutSeconds",
    "TimeoutSecondsPath",
    "HeartbeatSeconds",
    "HeartbeatSecondsPath",
    "Next",
    "End",
];

/// The members a `Task` state written in JSONata that asks for the callback pattern
/// may carry: the JSONata integration task's, plus the same heartbeat bound.
pub(super) const JSONATA_CALLBACK_TASK_MEMBERS: &[&str] = &[
    "Type",
    "Comment",
    "QueryLanguage",
    "Assign",
    "Output",
    "Resource",
    "Arguments",
    "TimeoutSeconds",
    "HeartbeatSeconds",
    "Retry",
    "Catch",
    "Next",
    "End",
];

/// The first member of `state` this emulator does not execute, if any.
///
/// `language` is the query language the state is written in, which decides WHICH set
/// of data-flow members it may carry: a JSONata state reads and writes its data
/// through the members of that language ([`jsonata_members`]) and a JSONPath one
/// through the `*Path` family and the payload templates.
pub(super) fn unexecutable_member(
    state_type: &str,
    state: &Map<String, Value>,
    language: &str,
) -> Option<String> {
    let resource = task_resource(state);
    let activity = super::super::names_an_activity(resource);
    // A callback task's work is answered from OUTSIDE the execution, so it carries
    // the heartbeat bound neither of the other two kinds of integration task does.
    let callback = state_type == "Task"
        && !activity
        && super::super::integration::asks_for_callback(resource);
    let allowed = match (language, state_type) {
        (JSONATA, "Task") if callback => JSONATA_CALLBACK_TASK_MEMBERS,
        (JSONATA, _) => jsonata_members(state_type, activity),
        (_, "Task") if callback => CALLBACK_TASK_MEMBERS,
        // The two kinds of `Task` work carry different members, and the `Resource`
        // is what says which kind this state is.
        (_, "Task") if !activity => INTEGRATION_TASK_MEMBERS,
        _ => executable_members(state_type),
    };
    state
        .keys()
        .find(|member| !allowed.contains(&member.as_str()))
        .cloned()
}

/// The members a state written in JSONata may carry.
///
/// The language's own data-flow members replace JSONPath's: `Arguments` is the request
/// a `Task` builds (`Parameters`), `Items` the array a `Map` iterates (`ItemsPath`),
/// a `Choice` rule's `Condition` its test (`Variable` and the comparators) and
/// `Output` what a state puts out (`ResultSelector`, `ResultPath`, `OutputPath`) —
/// beside `Assign`, which every type may carry and which only this language has. The
/// `*Path` family and the payload templates belong to the other language
/// ([`JSONPATH_ONLY_MEMBERS`]), which is what a definition carrying one is refused
/// for at create time.
pub(super) fn jsonata_members(state_type: &str, activity: bool) -> &'static [&'static str] {
    match state_type {
        "Pass" => &["Type", "Comment", "QueryLanguage", "Assign", "Output", "Next", "End"],
        "Wait" => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "Assign",
            "Output",
            "Seconds",
            "Timestamp",
            "Next",
            "End",
        ],
        "Choice" => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "Assign",
            "Output",
            "Choices",
            "Default",
        ],
        "Succeed" => &["Type", "Comment", "QueryLanguage", "Output"],
        "Fail" => &["Type", "Comment", "QueryLanguage", "Error", "Cause"],
        // An ACTIVITY task's work is a worker's, which is what SENDS the heartbeats the
        // member's window is measured between; a task whose work this emulator performs
        // itself carries the member too, with a window nothing heartbeats
        // ([`INTEGRATION_TASK_MEMBERS`]).
        "Task" if activity => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "Assign",
            "Output",
            "Resource",
            "Arguments",
            "TimeoutSeconds",
            "HeartbeatSeconds",
            "Retry",
            "Catch",
            "Next",
            "End",
        ],
        "Task" => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "Assign",
            "Output",
            "Resource",
            "Arguments",
            "TimeoutSeconds",
            "HeartbeatSeconds",
            "Retry",
            "Catch",
            "Next",
            "End",
        ],
        "Map" => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "Assign",
            "Output",
            "Items",
            "ItemReader",
            "ItemBatcher",
            "ItemSelector",
            "ItemProcessor",
            "Iterator",
            "MaxConcurrency",
            "ResultWriter",
            "Retry",
            "Catch",
            "ToleratedFailureCount",
            "ToleratedFailurePercentage",
            "Label",
            "Next",
            "End",
        ],
        "Parallel" => &[
            "Type",
            "Comment",
            "QueryLanguage",
            "Assign",
            "Output",
            "Arguments",
            "Branches",
            "Retry",
            "Catch",
            "Next",
            "End",
        ],
        _ => &[],
    }
}
