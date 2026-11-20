// SPDX-License-Identifier: CC0-1.0

//! WHERE a simulation locates the statement of a document whose `Statement` element states ONE
//! statement as an OBJECT, end to end over the awsQuery wire.
//!
//! RULE: a `MatchedStatements` entry locates its statement in the document it was read from —
//! "the row and column of the beginning of the `Statement` in an IAM policy"
//! (`Statement$StartPosition`) and of its end (`$EndPosition`), each a `Position` of `Line`
//! and `Column` counted from one. The statement element OPENS with a delimiter that is not
//! part of the statement inside it: the `[` of a list, and the brace of the object a document
//! stating a single statement writes instead. So the statement begins at the character after
//! that delimiter — which for a list written `[{` is the statement's own `{` — and ends one
//! character past its last. A document that states its one statement as an object therefore
//! locates it at the same position as the equivalent list, and a client slicing or
//! highlighting the document at the reported position lands inside the statement either way.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: how the
//! object-form document is laid out (minified, spaced after every `:` and `,`, indented
//! across lines) and the FORM of the element (the single statement an object states, and the
//! list stating the same statement, which the rule puts at the same column). Each row is asked
//! of both operations that answer a simulation report, since the projection is theirs
//! together, and every position is stated by the row rather than read out of the response.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns; the principal fixture
//! is `tests/all/request_condition_context.rs`'s, the decision values
//! `tests/all/policy_simulation.rs`'s and the position reader
//! `tests/all/simulated_statement_positions.rs`'s, so every simulation suite reads a report
//! one way.

use smithy_server_runtime::MultiServiceRouter;

use crate::policy_simulation::ALLOWED;
use crate::request_condition_context::{create_user, user_arn};
use crate::simulated_statement_positions::positions;
use crate::user_lifecycle::{call, element, router};

/// The action every row simulates.
const LIST_USERS: &str = "iam:ListUsers";

/// The two operations that answer a simulation report.
const OPERATIONS: &[&str] = &["SimulateCustomPolicy", "SimulatePrincipalPolicy"];

/// One located row: the document, and where the statement it states begins and ends.
struct Located {
    case: &'static str,
    document: &'static str,
    /// The `(Line, Column)` the statement's span begins at: the character after the delimiter
    /// that opens the statement element.
    start: (i32, i32),
    /// The `(Line, Column)` one character past the statement's last.
    end: (i32, i32),
}

/// Rows spanning the layouts of an object-form document, with the list form for contrast.
const LOCATED: &[Located] = &[
    Located {
        case: "the single statement a minified object states",
        // `{` of the object is the 14th character and opens the element, so the statement
        // begins at the 15th; its `}` is the 71st, so the statement ends at the 72nd — the
        // `}` closing the document.
        document: r#"{"Statement":{"Effect":"Allow","Action":"iam:ListUsers","Resource":"*"}}"#,
        start: (1, 15),
        end: (1, 72),
    },
    Located {
        case: "the same statement in a list, which the rule puts at the same column",
        // The `[` is the 14th character and the statement's own `{` the 15th, one past it,
        // where its span begins; its `}` is the 72nd, so it ends at the 73rd — the `]`.
        document: r#"{"Statement":[{"Effect":"Allow","Action":"iam:ListUsers","Resource":"*"}]}"#,
        start: (1, 15),
        end: (1, 73),
    },
    Located {
        case: "an object-form document spaced after every separator",
        // A space after the `:` moves the object's `{` to the 15th character, so the
        // statement begins at the 16th; the spaces inside it move its `}` to the 77th, so it
        // ends at the 78th.
        document: r#"{"Statement": {"Effect": "Allow", "Action": "iam:ListUsers", "Resource": "*"}}"#,
        start: (1, 16),
        end: (1, 78),
    },
    Located {
        case: "an object-form document indented across lines",
        // The element opens with the `{` that ends line 2 at its 16th column, so the
        // statement begins at that line's 17th; the `}` closing it is the 3rd column of line
        // 6, so the statement ends at that line's 4th.
        document: "{\n  \"Statement\": {\n    \"Effect\": \"Allow\",\n    \"Action\": \"iam:ListUsers\",\n    \"Resource\": \"*\"\n  }\n}",
        start: (2, 17),
        end: (6, 4),
    },
];

/// One call of `operation` simulating [`LIST_USERS`] against `document`.
async fn simulate(
    router: &MultiServiceRouter,
    operation: &str,
    principal_arn: &str,
    document: &str,
) -> (u16, String) {
    let mut params: Vec<(&str, &str)> = vec![
        ("PolicyInputList.member.1", document),
        ("ActionNames.member.1", LIST_USERS),
    ];
    if operation == "SimulatePrincipalPolicy" {
        params.push(("PolicySourceArn", principal_arn));
    }
    call(router, operation, &params).await
}

/// RULE over the table, on both operations: the statement is located from the character after
/// the delimiter that opens the statement element to one past the statement's last.
#[tokio::test]
async fn a_statement_an_object_states_begins_after_the_brace_that_opens_the_element() {
    let router = router();
    let principal = "object-statements";
    create_user(&router, "the simulated principal", principal, &[]).await;
    let arn = user_arn(principal);

    for row in LOCATED {
        for operation in OPERATIONS {
            let (status, body) = simulate(&router, operation, &arn, row.document).await;
            assert_eq!(
                status, 200,
                "case {:?}/{operation}: expected 200, got {status}: {body}",
                row.case
            );
            let decision = element(&body, "EvalDecision");
            assert_eq!(
                decision, ALLOWED,
                "case {:?}/{operation}: expected {ALLOWED}, got {decision:?}: {body}",
                row.case
            );
            let starts = positions(&body, "StartPosition");
            let ends = positions(&body, "EndPosition");
            assert_eq!(
                (starts.as_slice(), ends.as_slice()),
                (&[row.start][..], &[row.end][..]),
                "case {:?}/{operation}: expected the statement located at {:?}..{:?}, got \
                 {starts:?}..{ends:?}: {body}",
                row.case,
                row.start,
                row.end
            );
        }
    }
}
