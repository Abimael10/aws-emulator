// SPDX-License-Identifier: CC0-1.0

//! WHO MAY READ a parameter: every Parameter Store read is decided against the calling
//! principal's identity-based policies before this service resolves what the request names.
//!
//! RULE under test: **AWS evaluates a signed call against the identity-based policies in force
//! on the principal it was signed as before the service reads anything, so a read whose caller
//! holds no grant for `ssm:<OperationName>` on the parameter the request addresses is refused
//! `AccessDeniedException` (HTTP 400) naming the principal, the action, the resource and the
//! rule that withheld the grant — while a caller a readable document allows is served, and one
//! an explicit `Deny` covers is refused however much else it is allowed.** The decision comes
//! BEFORE the store, so it says nothing about whether the parameter exists.
//!
//! Why it matters beyond the refusal itself: a function, a task or an operator calling this
//! service runs as a ROLE, and the whole point of scoping that role's policy to the parameters
//! it needs is that a call outside the scope fails. An emulator that serves such a read reports
//! success for work AWS refuses — and a caller that handles the refusal (a custom resource
//! provider that answers CloudFormation `FAILED`, a retry, a fallback) is never exercised at
//! all.
//!
//! Every case drives the emulator through its own wire: the principals are made through IAM's
//! and STS's own operations, the parameters through this service's `PutParameter`, and each
//! read is signed with the credential of the principal under test.
//!
//! The rows differ along what the rule generalises over: WHICH read is called (the single read,
//! the batch read, the hierarchy read and the history read — the four operations a parameter is
//! read through), the KIND of principal (the account's own published credential and assumed-role
//! sessions), and what the principal's policy says (nothing, every action of this service, the
//! actions on the parameters the read addresses, the same actions on ANOTHER parameter, and an
//! explicit deny on top of a grant of everything).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::just_in_time_node_access::{assumed_role, call_as, emulator, Principal};
use crate::parameter_store::{call, error_code, error_message, ACCOUNT, REGION};

/// The parameter every read addresses, the one beside it that no row's grant covers, and the
/// hierarchy both live in.
const PARAMETER: &str = "/probe/secret";
const OTHER_PARAMETER: &str = "/probe/other";
const PATH: &str = "/probe";

/// The access key id the account's own published credential presents, which is what the
/// sibling suites sign their setup calls with.
const ACCOUNT_KEY: &str = "AKIDTEST";

/// The ARN of the parameter (or hierarchy) `name`, as the resource an IAM statement names it
/// by: `arn:aws:ssm:<region>:<account>:parameter/Dev/DBServer` — the hierarchical name keeps
/// its whole path after the `parameter/` prefix (`PutParameter`, `Parameter$ARN`).
fn parameter_arn(name: &str) -> String {
    format!("arn:aws:ssm:{REGION}:{ACCOUNT}:parameter{name}")
}

/// One read under test: the operation, the body that calls it, and the resource the decision
/// about it is taken on — the parameter a named read addresses, and the PATH a hierarchy read
/// does.
struct Read {
    case: &'static str,
    operation: &'static str,
    body: Value,
    resource: String,
}

fn reads() -> Vec<Read> {
    vec![
        Read {
            case: "the single read",
            operation: "GetParameter",
            body: json!({ "Name": PARAMETER }),
            resource: parameter_arn(PARAMETER),
        },
        Read {
            case: "the batch read",
            operation: "GetParameters",
            body: json!({ "Names": [PARAMETER] }),
            resource: parameter_arn(PARAMETER),
        },
        Read {
            case: "the hierarchy read",
            operation: "GetParametersByPath",
            body: json!({ "Path": PATH, "Recursive": true }),
            resource: parameter_arn(PATH),
        },
        Read {
            case: "the history read",
            operation: "GetParameterHistory",
            body: json!({ "Name": PARAMETER }),
            resource: parameter_arn(PARAMETER),
        },
    ]
}

/// What the rule says about one principal's read: it is served, or refused with the rule that
/// withheld the grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decided {
    /// A readable document allows the action on the resource (or there is no document the
    /// decision can bound the caller by at all).
    Served,
    /// No readable document states the action: the sentence names the action it was asked
    /// about.
    NoGrant,
    /// A readable document DENIES it explicitly, which refuses the call whatever else the
    /// principal carries.
    ExplicitDeny,
}

/// The sentence a refusal carries for one principal, action and resource — IAM's own wording,
/// with the rule that withheld the grant at the end.
fn not_authorized(principal: &str, operation: &str, resource: &str, decided: Decided) -> String {
    let action = format!("ssm:{operation}");
    let reason = match decided {
        Decided::NoGrant => format!("because no identity-based policy allows the {action} action"),
        Decided::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        Decided::Served => String::new(),
    };
    format!("User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}")
}

/// Seed the two parameters every read is answered from, through this service's own write and
/// with the account's own credential — which is the caller the decision cannot bound, so the
/// seeding says nothing about the rows below.
async fn seed(router: &MultiServiceRouter) {
    for name in [PARAMETER, OTHER_PARAMETER] {
        let (status, body) = call(
            router,
            "PutParameter",
            &json!({ "Name": name, "Value": "held", "Type": "String" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "seeding {name:?}: expected PutParameter to succeed, got {status} {body}"
        );
    }
}

#[tokio::test]
async fn a_read_is_decided_against_the_callers_identity_policies() {
    let router = emulator();
    seed(&router).await;

    // Every action of this service, on every resource: the grant an administrator carries.
    let allow_every_action = r#"{"Version":"2012-10-17","Statement":[
        {"Effect":"Allow","Action":"ssm:*","Resource":"*"}]}"#;
    // The same actions, scoped to the hierarchy the reads address — the parameter and the
    // path, which is what a role scoped to the parameters it needs carries.
    let allow_this_hierarchy = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
        "Action":"ssm:*","Resource":"{}*"}}]}}"#,
        parameter_arn(PATH)
    );
    // The same actions on ANOTHER parameter, which states nothing about the ones under test.
    let allow_another_parameter = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
        "Action":"ssm:*","Resource":"{}"}}]}}"#,
        parameter_arn(OTHER_PARAMETER)
    );
    // A grant of everything with an explicit deny on the reads on top of it.
    let deny_the_reads = r#"{"Version":"2012-10-17","Statement":[
        {"Effect":"Allow","Action":"ssm:*","Resource":"*"},
        {"Effect":"Deny","Action":"ssm:Get*","Resource":"*"}]}"#;

    let principals: Vec<(&str, Principal, Decided)> = vec![
        (
            "the account's own published credential, which holds no identity policy",
            Principal {
                access_key_id: ACCOUNT_KEY.to_string(),
                arn: format!("arn:aws:iam::{ACCOUNT}:root"),
            },
            Decided::Served,
        ),
        (
            "a session carrying no policy at all",
            assumed_role(&router, "parameter-reader-nopolicy", None).await,
            Decided::NoGrant,
        ),
        (
            "a session allowed every action of this service",
            assumed_role(&router, "parameter-reader-admin", Some(allow_every_action)).await,
            Decided::Served,
        ),
        (
            "a session allowed this service's actions on the hierarchy it reads",
            assumed_role(
                &router,
                "parameter-reader-scoped",
                Some(&allow_this_hierarchy),
            )
            .await,
            Decided::Served,
        ),
        (
            "a session allowed the same actions on ANOTHER parameter",
            assumed_role(
                &router,
                "parameter-reader-elsewhere",
                Some(&allow_another_parameter),
            )
            .await,
            Decided::NoGrant,
        ),
        (
            "a session allowed everything with an explicit deny on the reads",
            assumed_role(&router, "parameter-reader-denied", Some(deny_the_reads)).await,
            Decided::ExplicitDeny,
        ),
    ];

    for (who, principal, decided) in &principals {
        for read in reads() {
            let case = format!("{who}, {}", read.case);
            let (status, body) =
                call_as(&router, &principal.access_key_id, REGION, read.operation, &read.body)
                    .await;
            if *decided == Decided::Served {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the allowed caller's read to be served, got \
                     {status} {body}"
                );
                continue;
            }
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "AccessDeniedException"),
                "case {case:?}: expected the read to be refused, got {status} {body}"
            );
            let expected =
                not_authorized(&principal.arn, read.operation, &read.resource, *decided);
            assert_eq!(
                error_message(&body),
                expected,
                "case {case:?}: expected the refusal to carry IAM's own sentence, got {body}"
            );
        }
    }
}

#[tokio::test]
async fn a_batch_read_is_refused_for_a_name_the_caller_may_not_read() {
    // A batch read asks for every name it carries, so a caller granted one of them and not the
    // other is refused the whole call — and the sentence names the parameter the grant does
    // not cover, not the one it does.
    let router = emulator();
    seed(&router).await;
    let allow_one_parameter = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
        "Action":"ssm:GetParameters","Resource":"{}"}}]}}"#,
        parameter_arn(PARAMETER)
    );
    let principal =
        assumed_role(&router, "parameter-reader-partial", Some(&allow_one_parameter)).await;

    // The granted name alone is served.
    let case = "the granted name alone";
    let (status, body) = call_as(
        &router,
        &principal.access_key_id,
        REGION,
        "GetParameters",
        &json!({ "Names": [PARAMETER] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the granted name to be served, got {status} {body}"
    );

    // ...and the pair is refused, naming the one the grant does not cover.
    for (case, names) in [
        ("the ungranted name beside it", json!([PARAMETER, OTHER_PARAMETER])),
        ("the ungranted name first", json!([OTHER_PARAMETER, PARAMETER])),
    ] {
        let (status, body) = call_as(
            &router,
            &principal.access_key_id,
            REGION,
            "GetParameters",
            &json!({ "Names": names }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "AccessDeniedException"),
            "case {case:?}: expected the batch to be refused, got {status} {body}"
        );
        let expected = not_authorized(
            &principal.arn,
            "GetParameters",
            &parameter_arn(OTHER_PARAMETER),
            Decided::NoGrant,
        );
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected the refusal to name the parameter the grant does not \
             cover, got {body}"
        );
    }
}
