// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the IAM decision a request that asks for MORE than the operation it
//! names is subject to, driven over this service's own wire protocol through the production
//! `MultiServiceRouter` with Step Functions, IAM and STS mounted on ONE engine — so the
//! documents the decision reads are the ones IAM's own handlers wrote and the key a call
//! signs with is one IAM or STS minted.
//!
//! The rule under test: a member that switches on a capability AWS gates on an action of its
//! own makes the call ask for THAT action besides the operation's own, so such a call is
//! served only to a caller whose identity-based documents allow BOTH, and otherwise refused
//! with `AccessDeniedException` naming the first action they do not allow — while the same
//! caller's call with the member at its modelled default asks for the operation's action
//! alone and is served on that grant by itself. The resource either refusal names is the one
//! the request addresses, which for a call naming no resource of this service is every
//! resource of it in the account and Region the request was made for, written in this
//! service's own ARN form (`arn:aws:states:<region>:<account>:*:*`).
//!
//! The one member of this model that is such a capability is `TestState$revealSecrets`,
//! gated on `states:RevealSecrets` ("if you set revealSecrets to true, you must make sure
//! that the IAM user that calls the TestState API has permission for the
//! states:RevealSecrets action ... without this permission, Step Functions throws an access
//! denied error"), and the catalog the gate reads it through is re-derived from this crate's
//! Smithy model below, so a model that gates another member on an action of its own fails
//! here rather than leaving that capability ungated.
//!
//! The rows differ along what the rule generalises over: whether the request switches the
//! capability on (`true`, `false`, left out altogether) and what else it carries beside it
//! (an inspection level, a `stateName` picking one state out of a whole definition), which
//! of the two actions the caller's documents grant (the operation only, the capability only,
//! both, every action of this service), how they say it (an allow, an explicit deny beside a
//! wildcard allow, nothing at all), and the KIND of principal (an IAM user, an assumed-role
//! session, and the account's own published credential, which no identity policy governs).
//!
//! The harness (`Emulator`, `Principal`, `allowing`, `not_authorized`, `no_grant`,
//! `message`) is the one `identity_policy_authorization` already builds; `tests/all.rs` is
//! this binary's crate root, so it is reached through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::service::identity_authorization;

use crate::identity_policy_authorization::{
    every_resource, message, no_grant, not_authorized, Emulator, Principal, ACCESS_DENIED,
    ACCESS_DENIED_STATUS, ACCOUNT_KEY, EVERY_TYPE, MODEL_PATH,
};
use crate::state_machine_lifecycle::{error_code, ACCOUNT};
use crate::sync_execution::document as json_member;

/// The operation the capability below belongs to, and the action AWS names it by.
const OPERATION: &str = "TestState";
const OPERATION_ACTION: &str = "states:TestState";

/// The action the capability member is gated on.
const CAPABILITY_ACTION: &str = "states:RevealSecrets";

/// The `Resource` element every policy below is written at: AWS's own example of the
/// capability's permission grants it on every resource.
const EVERY_RESOURCE_SCOPE: &str = "*";

/// The definition of the ONE state every call below tests: a `Pass` state whose result is
/// fixed, so what a served call answers is decided by the state and not by the caller.
const PASS_STATE: &str = r#"{"Type": "Pass", "Result": {"ok": true}, "End": true}"#;

/// The output that state produces, whichever caller is served.
const PASS_OUTPUT: &str = r#"{"ok":true}"#;

/// A whole state machine definition, so one row can pick a state out of it by name.
const WHOLE_DEFINITION: &str = r#"{
    "StartAt": "Only",
    "States": { "Only": { "Type": "Pass", "Result": { "ok": true }, "End": true } }
}"#;

/// One `TestState` call the rule covers: what it carries, and whether it switches the
/// capability on.
struct Call {
    case: &'static str,
    body: Value,
    /// Whether the request asks for the capability, which is what makes the call ask for
    /// the capability's action too.
    reveals: bool,
}

/// Every call the rows below are taken over: the capability switched on, switched off and
/// left out, each shape of request carrying something different beside it.
fn calls() -> Vec<Call> {
    vec![
        Call {
            case: "the capability switched on",
            body: json!({ "definition": PASS_STATE, "input": "{}", "revealSecrets": true }),
            reveals: true,
        },
        Call {
            case: "the capability switched off",
            body: json!({ "definition": PASS_STATE, "input": "{}", "revealSecrets": false }),
            reveals: false,
        },
        Call {
            case: "the capability left out altogether",
            body: json!({ "definition": PASS_STATE, "input": "{}" }),
            reveals: false,
        },
        Call {
            case: "the capability switched on beside an inspection level",
            body: json!({
                "definition": PASS_STATE,
                "input": "{}",
                "inspectionLevel": "DEBUG",
                "revealSecrets": true,
            }),
            reveals: true,
        },
        Call {
            case: "the capability switched on for a state picked out of a whole definition",
            body: json!({
                "definition": WHOLE_DEFINITION,
                "stateName": "Only",
                "input": "{}",
                "revealSecrets": true,
            }),
            reveals: true,
        },
        Call {
            case: "the capability left out for a state picked out of a whole definition",
            body: json!({
                "definition": WHOLE_DEFINITION,
                "stateName": "Only",
                "input": "{}",
            }),
            reveals: false,
        },
    ]
}

/// One caller of the calls above: what its identity-based documents say, and how it signs.
struct CallerCase {
    case: &'static str,
    /// The actions its inline policy ALLOWS, as a policy's `Action` element writes them.
    grants: &'static [&'static str],
    /// The actions its inline policy DENIES explicitly, beside those grants.
    denies: &'static [&'static str],
    /// Whether it is an assumed-role session rather than an IAM user.
    session: bool,
    /// Whether it is the account's own published credential, which no identity policy
    /// governs and no inline document is written for.
    account: bool,
}

/// The inline policy document carrying `grants` and `denies`, on every resource — the scope
/// AWS's own example of the capability's permission is written at.
fn document(grants: &[&str], denies: &[&str]) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": grants,
        "Resource": [EVERY_RESOURCE_SCOPE],
    })];
    if !denies.is_empty() {
        statements.push(json!({
            "Effect": "Deny",
            "Action": denies,
            "Resource": [EVERY_RESOURCE_SCOPE],
        }));
    }
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// Whether the action pattern `granted` covers `action`: IAM's trailing `*`, which is how
/// one statement naming `states:*` covers every action of this service.
fn covers(granted: &str, action: &str) -> bool {
    match granted.strip_suffix('*') {
        Some(prefix) => action.starts_with(prefix),
        None => granted == action,
    }
}

/// The FIRST action of `asked` the documents in force on `caller` do not allow, and the
/// clause a refusal on it names — or `None` when they allow every one of them.
fn refusal(caller: &CallerCase, asked: &[&'static str]) -> Option<(&'static str, String)> {
    if caller.account {
        return None;
    }
    for action in asked {
        if caller.denies.iter().any(|denied| covers(denied, action)) {
            return Some((
                action,
                "with an explicit deny in an identity-based policy".to_string(),
            ));
        }
        if !caller.grants.iter().any(|granted| covers(granted, action)) {
            return Some((action, no_grant(action)));
        }
    }
    None
}

/// The actions a call asks for: the operation's own, and the capability's when the request
/// switches the capability on.
fn asked_for(reveals: bool) -> Vec<&'static str> {
    if reveals {
        vec![OPERATION_ACTION, CAPABILITY_ACTION]
    } else {
        vec![OPERATION_ACTION]
    }
}

/// A call asking for a capability is served only where the documents in force on the
/// signing principal allow the capability's action as well as the operation's; every other
/// caller is refused with `AccessDeniedException` naming the first action they withhold, and
/// the same caller's call with the capability at its default is decided on the operation's
/// action alone.
#[tokio::test]
async fn a_capability_is_served_only_where_its_own_action_is_allowed_too() {
    let callers = [
        CallerCase {
            case: "an IAM user allowed the operation but not the capability",
            grants: &[OPERATION_ACTION],
            denies: &[],
            session: false,
            account: false,
        },
        CallerCase {
            case: "an IAM user allowed the operation and the capability",
            grants: &[OPERATION_ACTION, CAPABILITY_ACTION],
            denies: &[],
            session: false,
            account: false,
        },
        CallerCase {
            case: "an IAM user allowed the capability but not the operation",
            grants: &[CAPABILITY_ACTION],
            denies: &[],
            session: false,
            account: false,
        },
        CallerCase {
            case: "an IAM user allowed every action of this service",
            grants: &["states:*"],
            denies: &[],
            session: false,
            account: false,
        },
        CallerCase {
            case: "an IAM user allowed every action with the capability denied explicitly",
            grants: &["states:*"],
            denies: &[CAPABILITY_ACTION],
            session: false,
            account: false,
        },
        CallerCase {
            case: "an IAM user whose documents say nothing of this service",
            grants: &["sts:GetCallerIdentity"],
            denies: &[],
            session: false,
            account: false,
        },
        CallerCase {
            case: "an assumed-role session allowed the operation but not the capability",
            grants: &[OPERATION_ACTION],
            denies: &[],
            session: true,
            account: false,
        },
        CallerCase {
            case: "an assumed-role session allowed the operation and the capability",
            grants: &[OPERATION_ACTION, CAPABILITY_ACTION],
            denies: &[],
            session: true,
            account: false,
        },
        CallerCase {
            case: "the account's own published credential",
            grants: &[],
            denies: &[],
            session: false,
            account: true,
        },
    ];

    for (index, caller) in callers.iter().enumerate() {
        let emulator = Emulator::new();
        let policy = document(caller.grants, caller.denies);
        let principal = if caller.account {
            Principal {
                access_key_id: ACCOUNT_KEY.to_string(),
                arn: format!("arn:aws:iam::{ACCOUNT}:root"),
            }
        } else if caller.session {
            emulator
                .assumed_role(&format!("reveal-role-{index}"), &policy)
                .await
        } else {
            emulator
                .user(&format!("reveal-user-{index}"), &policy)
                .await
        };

        for call in calls() {
            let case = caller.case;
            let row = call.case;
            let asked = asked_for(call.reveals);
            let (status, answer) = emulator
                .call_as(&principal.access_key_id, OPERATION, &call.body)
                .await;
            match refusal(caller, &asked) {
                None => {
                    assert_eq!(
                        status, 200,
                        "case {case:?} / {row:?}: a call every asked-for action is allowed for \
                         is served, got {status} with {answer:?}"
                    );
                    assert_eq!(
                        answer.get("status").and_then(Value::as_str),
                        Some("SUCCEEDED"),
                        "case {case:?} / {row:?}: the served call reports what the state did, \
                         got {answer:?}"
                    );
                    let expected: Value =
                        serde_json::from_str(PASS_OUTPUT).expect("the expected output is JSON");
                    let produced = json_member(&answer, "output");
                    assert_eq!(
                        produced,
                        Some(expected.clone()),
                        "case {case:?} / {row:?}: expected the state's output {expected:?}, got \
                         {answer:?}"
                    );
                }
                Some((refused_action, reason)) => {
                    assert_eq!(
                        (status, error_code(&answer).as_str()),
                        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
                        "case {case:?} / {row:?}: a call asking for {refused_action} is refused, \
                         got {status} with {answer:?}"
                    );
                    let expected = not_authorized(
                        &principal.arn,
                        refused_action,
                        &every_resource(EVERY_TYPE),
                        &reason,
                    );
                    let stated = message(&answer);
                    assert_eq!(
                        stated, expected,
                        "case {case:?} / {row:?}: expected {expected:?}, got {stated:?}"
                    );
                    // A refused call ran no state, so it reported nothing of one.
                    for withheld in ["output", "status", "inspectionData"] {
                        assert!(
                            answer.get(withheld).is_none(),
                            "case {case:?} / {row:?}: a refused call must report no {withheld}, \
                             got {answer:?}"
                        );
                    }
                }
            }
        }
    }
}

/// The decision is taken against the documents as they read NOW: granting the capability's
/// action serves the next call that asks for it, and taking it away refuses the next one —
/// while the calls that do not ask for it are served throughout.
#[tokio::test]
async fn granting_and_revoking_the_capability_decides_the_next_call() {
    let emulator = Emulator::new();
    let user = "reveal-revocation";
    let principal = emulator
        .user(user, &document(&[OPERATION_ACTION], &[]))
        .await;
    let revealing = json!({ "definition": PASS_STATE, "input": "{}", "revealSecrets": true });
    let plain = json!({ "definition": PASS_STATE, "input": "{}" });

    // (case, the actions the document in force allows, whether the revealing call is served)
    let phases: Vec<(&str, &[&str], bool)> = vec![
        ("the capability withheld", &[OPERATION_ACTION], false),
        (
            "the capability granted",
            &[OPERATION_ACTION, CAPABILITY_ACTION],
            true,
        ),
        ("the capability taken away again", &[OPERATION_ACTION], false),
    ];

    for (phase, grants, served) in phases {
        emulator
            .put_user_policy(user, &document(grants, &[]))
            .await;
        // Each phase is asked TWICE: the decision is taken per request, so nothing latches.
        for attempt in 1..=2 {
            let (status, answer) = emulator
                .call_as(&principal.access_key_id, OPERATION, &revealing)
                .await;
            if served {
                assert_eq!(
                    status, 200,
                    "case {phase:?} (attempt {attempt}): the revealing call is served, got \
                     {status} with {answer:?}"
                );
            } else {
                assert_eq!(
                    (status, error_code(&answer).as_str()),
                    (ACCESS_DENIED_STATUS, ACCESS_DENIED),
                    "case {phase:?} (attempt {attempt}): the revealing call is refused, got \
                     {status} with {answer:?}"
                );
                let expected = not_authorized(
                    &principal.arn,
                    CAPABILITY_ACTION,
                    &every_resource(EVERY_TYPE),
                    &no_grant(CAPABILITY_ACTION),
                );
                assert_eq!(
                    message(&answer),
                    expected,
                    "case {phase:?} (attempt {attempt}): expected {expected:?}, got {answer:?}"
                );
            }
            // The call that does not ask for the capability is served in every phase: the
            // grant the capability needs gates the reveal, not the operation.
            let (status, answer) = emulator
                .call_as(&principal.access_key_id, OPERATION, &plain)
                .await;
            assert_eq!(
                status, 200,
                "case {phase:?} (attempt {attempt}): the call that asks for no capability is \
                 served, got {status} with {answer:?}"
            );
            assert_eq!(
                answer.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {phase:?} (attempt {attempt}): the served call reports what the state \
                 did, got {answer:?}"
            );
        }
    }
}

/// The catalog the gate reads a request's capabilities through
/// (`service::identity_authorization::CAPABILITY_ACTIONS`) is a transcription of this
/// crate's Smithy model: every member whose own documentation says a call setting it needs
/// an action of this service carries a row naming that action, each row's member is a
/// boolean member of THAT operation's input defaulting to `false`, and the catalog carries
/// no row the model does not.
///
/// So a model that gates another member on an action of its own fails here rather than
/// leaving that capability served to a caller that holds only the operation.
#[test]
fn the_catalog_of_capability_actions_is_the_model_s_own() {
    let source = std::fs::read_to_string(MODEL_PATH).unwrap_or_else(|error| {
        panic!("the Smithy model at {MODEL_PATH} must be readable: {error}")
    });
    let model: Value = serde_json::from_str(&source).expect("the Smithy model parses as JSON");
    let shapes = model
        .get("shapes")
        .and_then(Value::as_object)
        .expect("the Smithy model carries a shapes map");

    /// How AWS words a member's own permission requirement in this model's documentation:
    /// "has permission for the <code>states:RevealSecrets</code> action".
    const REQUIRES: &str = "permission for the <code>states:";

    // The capability members the MODEL declares, as (operation, member, action name).
    let mut declared: Vec<(String, String, String)> = Vec::new();
    for (shape_id, shape) in shapes {
        if shape.get("type").and_then(Value::as_str) != Some("operation") {
            continue;
        }
        let operation = shape_id.rsplit('#').next().unwrap_or(shape_id).to_string();
        let input = shape
            .get("input")
            .and_then(|input| input.get("target"))
            .and_then(Value::as_str)
            .expect("every operation of this model declares an input shape");
        let Some(members) = shapes
            .get(input)
            .and_then(|shape| shape.get("members"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (member, declaration) in members {
            let documentation = declaration
                .get("traits")
                .and_then(|traits| traits.get("smithy.api#documentation"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some(required) = documentation.split(REQUIRES).nth(1) else {
                continue;
            };
            let action = required
                .split("</code>")
                .next()
                .unwrap_or_default()
                .to_string();
            declared.push((operation.clone(), member.clone(), action));
        }
    }

    for (operation, member, action) in &declared {
        let carried = identity_authorization::capabilities(operation);
        assert!(
            carried
                .iter()
                .any(|(named, gated)| *named == member.as_str() && *gated == action.as_str()),
            "case {operation:?}/{member:?}: the model says a call setting it needs \
             states:{action}, which the catalog does not carry; it carries {carried:?}"
        );
    }
    assert_eq!(
        declared.len(),
        identity_authorization::CAPABILITY_ACTIONS.len(),
        "expected one catalog row per capability member the model declares, got {} row(s) for \
         {declared:?}",
        identity_authorization::CAPABILITY_ACTIONS.len()
    );

    // Each row's member is a boolean member of its own operation's input, defaulting to the
    // value a request that does not switch the capability on carries.
    for (operation, member, action) in identity_authorization::CAPABILITY_ACTIONS {
        let input = shapes
            .get(&format!("com.amazonaws.sfn#{operation}"))
            .and_then(|shape| shape.get("input"))
            .and_then(|input| input.get("target"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {operation:?}: the model binds no such operation"))
            .to_string();
        let declaration = shapes
            .get(&input)
            .and_then(|shape| shape.get("members"))
            .and_then(Value::as_object)
            .and_then(|members| members.get(*member))
            .unwrap_or_else(|| {
                panic!("case {operation:?}: {input} declares no member {member:?}")
            });
        let target = declaration
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let kind = shapes
            .get(target)
            .and_then(|shape| shape.get("type"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            kind, "boolean",
            "case {operation:?}/{member:?}: a capability member is the boolean that switches \
             it on, got a {kind:?}"
        );
        // A capability is off by default, so a request that does not switch it on asks for
        // nothing more than the operation — declared on the member or on the shape it
        // targets, whichever this model writes it on.
        let defaults_to_false = |declared: Option<&Value>| {
            declared
                .and_then(|shape| shape.get("traits"))
                .and_then(|traits| traits.get("smithy.api#default"))
                == Some(&Value::Bool(false))
        };
        assert!(
            defaults_to_false(Some(declaration)) || defaults_to_false(shapes.get(target)),
            "case {operation:?}/{member:?}: a capability is off by default, so {target} must \
             default to false"
        );
        assert!(
            !action.is_empty(),
            "case {operation:?}/{member:?}: the row names no action"
        );
    }
}
