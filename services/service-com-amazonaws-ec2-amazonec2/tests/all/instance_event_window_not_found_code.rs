// SPDX-License-Identifier: CC0-1.0

//! The REFUSAL this service answers an `InstanceEventWindowId` it cannot resolve with, over
//! every operation that resolves one.
//!
//! The rule under test: an event window id that names no window of this account is refused by
//! the refusal of the RESOLUTION that could not resolve it, and that refusal does not depend
//! on the id — only on which resolution ran:
//!
//! * the family's OWN id resolution answers its own code,
//!   `InvalidInstanceEventWindowIDNotFound`, beside a sentence of the same family: the
//!   bracketed list of the ids a request named, for the describe over a LIST of them and for
//!   a tag request that names one, and the SINGULAR of that sentence for the delete, which
//!   names exactly one;
//! * an operation that names a window as an ordinary single-resource REFERENCE beside what it
//!   would change — the associate, the disassociate, the modify — answers this service's
//!   generic refusal for a resource it cannot resolve, the dotted
//!   `InvalidInstanceEventWindowId.NotFound` with its quoted `does not exist` sentence.
//!
//! A value that is no event window id at all is the `.Malformed` half of the pair on every one
//! of them, because that refusal is about the id's FORM rather than about a resource.
//!
//! The rows span what the rule generalises over: which operation resolved the id (the
//! describe's bulk resolution, each of the four operations that name one window, and the
//! tagging call that names a window) and the FORM of the id (both id lengths EC2 issues, in
//! several spellings of each), so no row is the specification and no refusal is pinned to one
//! id. The operations are the ones the model gives an `InstanceEventWindowId` member
//! (`smithy_query(query="operation",operation="AssociateInstanceEventWindow,
//! DisassociateInstanceEventWindow,ModifyInstanceEventWindow,DeleteInstanceEventWindow,
//! DescribeInstanceEventWindows")`).
//!
//! The wire is the one the service speaks, through the crate's shared [`super::ec2_query`]
//! harness.

use service_com_amazonaws_ec2_amazonec2::state::{
    INVALID_INSTANCE_EVENT_WINDOW_ID_DOT_NOT_FOUND, INVALID_INSTANCE_EVENT_WINDOW_ID_MALFORMED,
    INVALID_INSTANCE_EVENT_WINDOW_ID_NOT_FOUND,
};
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};

/// The Region every call here is signed for: an event window carries no ARN and no zone, so
/// nothing asserted here is derived from it.
const REGION: &str = "us-west-2";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One request's form parameters.
type Params = Vec<(String, String)>;

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// Well-formed event window ids this emulator never issued: both id lengths EC2 issues, in
/// several spellings of each.
fn unresolvable_ids() -> Vec<(&'static str, &'static str)> {
    vec![
        ("the long seventeen-digit form", "iew-0123456789abcdef0"),
        ("another long id", "iew-0aaaabbbbccccddde"),
        ("the short eight-digit form", "iew-0a1b2c3d"),
        ("another short id", "iew-feedface"),
    ]
}

/// Values that are no event window id at all.
fn malformed_ids() -> Vec<(&'static str, &'static str)> {
    vec![
        ("a word", "not-an-event-window"),
        ("the prefix alone", "iew-"),
        ("another resource's id", "i-0123456789abcdef0"),
    ]
}

/// One operation that resolves an event window id: its name, the parameters that name `id`
/// through it, and the code and sentence the resolution it uses words.
struct Resolver {
    operation: &'static str,
    params: fn(&str) -> Params,
    code: &'static str,
    sentence: fn(&str) -> String,
}

/// The sentence this service words for a resource it cannot resolve: the id in single quotes,
/// with no trailing period.
fn does_not_exist(id: &str) -> String {
    format!("The instance-event-window ID '{id}' does not exist")
}

/// The SINGULAR of the family's own sentence: the one id the request named.
fn one_id_not_found(id: &str) -> String {
    format!("Invalid Instance Event Window ID {id}.")
}

/// The family's own sentence for the ids a request named and it could not resolve.
fn ids_not_found(id: &str) -> String {
    format!("Invalid Instance Event Window IDs [{id}].")
}

fn resolvers() -> Vec<Resolver> {
    vec![
        Resolver {
            operation: "DescribeInstanceEventWindows",
            params: |id| vec![p("InstanceEventWindowId.1", id)],
            code: INVALID_INSTANCE_EVENT_WINDOW_ID_NOT_FOUND,
            sentence: ids_not_found,
        },
        Resolver {
            operation: "DeleteInstanceEventWindow",
            params: |id| vec![p("InstanceEventWindowId", id)],
            code: INVALID_INSTANCE_EVENT_WINDOW_ID_NOT_FOUND,
            sentence: one_id_not_found,
        },
        Resolver {
            operation: "AssociateInstanceEventWindow",
            params: |id| {
                vec![
                    p("InstanceEventWindowId", id),
                    p("AssociationTarget.InstanceTag.1.Key", "Owner"),
                    p("AssociationTarget.InstanceTag.1.Value", "platform"),
                ]
            },
            code: INVALID_INSTANCE_EVENT_WINDOW_ID_DOT_NOT_FOUND,
            sentence: does_not_exist,
        },
        Resolver {
            operation: "DisassociateInstanceEventWindow",
            params: |id| {
                vec![
                    p("InstanceEventWindowId", id),
                    p("AssociationTarget.InstanceTag.1.Key", "Owner"),
                    p("AssociationTarget.InstanceTag.1.Value", "platform"),
                ]
            },
            code: INVALID_INSTANCE_EVENT_WINDOW_ID_DOT_NOT_FOUND,
            sentence: does_not_exist,
        },
        Resolver {
            operation: "ModifyInstanceEventWindow",
            params: |id| vec![p("InstanceEventWindowId", id), p("Name", "renamed")],
            code: INVALID_INSTANCE_EVENT_WINDOW_ID_DOT_NOT_FOUND,
            sentence: does_not_exist,
        },
        // The tagging plane names a window by the same id and resolves it through the
        // family's own resolution, which reports the ids a request named as the list it
        // looked them up in.
        Resolver {
            operation: "CreateTags",
            params: |id| {
                vec![
                    p("ResourceId.1", id),
                    p("Tag.1.Key", "team"),
                    p("Tag.1.Value", "platform"),
                ]
            },
            code: INVALID_INSTANCE_EVENT_WINDOW_ID_NOT_FOUND,
            sentence: ids_not_found,
        },
    ]
}

/// One window this account DOES hold, so every refusal below is about the id the request
/// named and not about an empty store.
async fn seed_one_window(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "CreateInstanceEventWindow",
        &[p("Name", "held"), p("CronExpression", "* 0-4 * * 1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: expected the setup create to succeed, got {status} with body {body}"
    );
    element(&body, "instanceEventWindowId").unwrap_or_default()
}

/// RULE: each resolution of an event window id answers ONE refusal for every id it cannot
/// resolve — its own code and its own sentence, naming the id the request carried.
#[tokio::test]
async fn an_unresolvable_event_window_id_answers_its_resolutions_refusal() {
    let router = emulator();
    let held = seed_one_window(&router, "one-refusal-per-resolution").await;

    for resolver in resolvers() {
        for (case, id) in unresolvable_ids() {
            let (status, body) =
                call(&router, REGION, resolver.operation, &(resolver.params)(id)).await;
            assert_eq!(
                (
                    status,
                    error_code(&body).as_str(),
                    error_message(&body).as_str()
                ),
                (400, resolver.code, (resolver.sentence)(id).as_str()),
                "case {case:?} on {}: expected the not-found refusal naming {id}, got {status} \
                 with body {body}",
                resolver.operation
            );
        }
    }

    // The two codes are told apart, so a client branching on either reads one of them and not
    // the other.
    assert_ne!(
        INVALID_INSTANCE_EVENT_WINDOW_ID_NOT_FOUND, INVALID_INSTANCE_EVENT_WINDOW_ID_DOT_NOT_FOUND,
        "the family's own not-found code and this service's generic one must differ"
    );

    // The control: the window the account DOES hold is served by every one of them, so what
    // the rows above assert is the id and not the operation. Each control runs against its
    // own store, so none of them is answered by what the one before it left.
    for resolver in resolvers() {
        let router = emulator();
        let held = seed_one_window(&router, "control").await;
        let (status, body) =
            call(&router, REGION, resolver.operation, &(resolver.params)(&held)).await;
        assert_eq!(
            status, 200,
            "case the window this account holds on {}: expected it to be served, got {status} \
             with body {body}",
            resolver.operation
        );
    }
    assert!(
        !held.is_empty(),
        "the setup create must report the id of the window it minted"
    );
}

/// RULE, the other half: a value that is no event window id at all is MALFORMED rather than
/// absent — a third code, on every one of the same operations.
#[tokio::test]
async fn a_value_that_is_no_event_window_id_keeps_the_malformed_code() {
    for not_found in [
        INVALID_INSTANCE_EVENT_WINDOW_ID_NOT_FOUND,
        INVALID_INSTANCE_EVENT_WINDOW_ID_DOT_NOT_FOUND,
    ] {
        assert_ne!(
            INVALID_INSTANCE_EVENT_WINDOW_ID_MALFORMED, not_found,
            "a value that is no id and an id that resolves to nothing must be told apart"
        );
    }
    let router = emulator();
    let _held = seed_one_window(&router, "malformed").await;

    for resolver in resolvers() {
        for (case, id) in malformed_ids() {
            // The tagging plane reads an id of ANY resource kind, so a value that is no
            // event window id is not its refusal to word.
            if resolver.operation == "CreateTags" {
                continue;
            }
            let (status, body) =
                call(&router, REGION, resolver.operation, &(resolver.params)(id)).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, INVALID_INSTANCE_EVENT_WINDOW_ID_MALFORMED),
                "case {case:?} on {}: expected the malformed refusal, got {status} with body \
                 {body}",
                resolver.operation
            );
        }
    }
}
