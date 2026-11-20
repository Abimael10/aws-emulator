// SPDX-License-Identifier: CC0-1.0

//! WHERE a simulation says the statement that determined its decision is, and that it
//! always reports which context keys it lacked a value for.
//!
//! The two rules under test, both of them about the report a simulation answers with and
//! shared by the two operations that answer one (`SimulateCustomPolicy` and
//! `SimulatePrincipalPolicy`):
//!
//! 1. every `MatchedStatements` entry LOCATES its statement in the document it was read
//!    from: `StartPosition` is "the row and column of the beginning of the Statement in an
//!    IAM policy" and `EndPosition` "the row and column of the end of a Statement in an IAM
//!    policy" (`Statement$StartPosition` / `$EndPosition`, each a `Position` of `Line` and
//!    `Column`) — so the statement ends at the row and column one character past its last
//!    and begins where the statement before it ended, the first statement of a document at
//!    its own first character, all counted from one, and two statements of ONE document are
//!    told apart by them. The spans of one document therefore TILE it: the separator written
//!    between two statements belongs to the span of the one that follows it, so how a
//!    document spaces its separators does not move a statement's beginning relative to the
//!    statement before it.
//! 2. `MissingContextValues` is "a list of context keys that are required by the included
//!    input policies but that were not provided by one of the input parameters", which "in
//!    the top-level result … contains the deduplicated set of missing context values across
//!    all requested resources" (`EvaluationResult$MissingContextValues`) — so the member is
//!    REPORTED by every result, carrying the keys when the decision lacked values and
//!    carrying none when it lacked nothing. Absent is not one of its answers.
//!
//! The suites are TABLES whose rows differ along what rule 1 generalises over: how the
//! document is laid out (minified, spaced, indented across lines, offset by a leading
//! line), which statement of it determined the decision (the only one, the second of two),
//! the effect that statement states and the decision it reaches, whether `Statement` is a
//! list or the single statement an object states, and which part of the report carries the
//! entry (the action's own result and each `ResourceSpecificResults` entry). Each row is
//! asked of both operations, since the projection is theirs together. No single request is
//! the specification, and none of the positions below is read out of a response: each row
//! states the row and column its document puts the statement at.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns; the decision values
//! are `tests/all/policy_simulation.rs`'s and the principal fixture
//! `tests/all/request_condition_context.rs`'s, so every simulation suite reads a report one
//! way.

use smithy_server_runtime::MultiServiceRouter;

use crate::policy_simulation::{ALLOWED, EXPLICIT_DENY, IMPLICIT_DENY};
use crate::request_condition_context::{create_user, user_arn};
use crate::user_lifecycle::{call, element, elements, error_code, router};

/// The action every row below simulates, except where the row names another.
const LIST_USERS: &str = "iam:ListUsers";

/// The action the rows that name RESOURCES simulate: a read of one S3 object, which is an
/// action published WITH a resource type, so a statement can be scoped to one object and a
/// pair naming one is a request that action could have been made as
/// (`crate::state::action_resource_types`). `iam:ListUsers` takes no resource type, so a
/// pair naming a concrete resource for it is decided before any document is read.
const GET_OBJECT: &str = "s3:GetObject";

/// A minified document whose only statement allows that object read — the same layout as
/// [`MINIFIED`], so the statement opens at the 38th character of the document and ends at
/// the 95th (its action name is one character shorter).
const MINIFIED_OBJECT_READ: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#;

/// A minified document whose only statement allows that action.
///
/// The statement opens at the 38th character of the document and closes at its 95th, so it
/// ends at the 96th.
const MINIFIED: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iam:ListUsers","Resource":"*"}]}"#;

/// The same document written with a space after each `:` and `,`, which moves every
/// position in it: the statement opens at the 41st character and ends at the 104th.
const SPACED: &str = r#"{"Version": "2012-10-17", "Statement": [{"Effect": "Allow", "Action": "iam:ListUsers", "Resource": "*"}]}"#;

/// Two statements, of which the SECOND is about the simulated action: the first opens at the
/// 38th character and ends at the 96th, where the second's span begins; the second closes at
/// the 154th character, so it ends at the 155th.
const SECOND_STATEMENT: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iam:ListRoles","Resource":"*"},{"Effect":"Allow","Action":"iam:ListUsers","Resource":"*"}]}"#;

/// The same two statements written with a space after the separator between them, which
/// moves the second statement's own `{` by one character but not the position its span
/// begins at: the first still ends at the 96th character and the second's span still begins
/// there, two characters before its `{`, and it ends at the 156th.
const SECOND_STATEMENT_SPACED: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iam:ListRoles","Resource":"*"}, {"Effect":"Allow","Action":"iam:ListUsers","Resource":"*"}]}"#;

/// An `Allow` of the action followed by a `Deny` of it: the explicit deny is what
/// determines the decision, so the entry locates the SECOND statement, whose span begins at
/// the 96th character — where the first statement ended — and ends at the 154th.
const ALLOW_THEN_DENY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iam:ListUsers","Resource":"*"},{"Effect":"Deny","Action":"iam:ListUsers","Resource":"*"}]}"#;

/// `Statement` stated as the single statement an object carries rather than as a list: it
/// opens at the 37th character, where the list's `[` would have been, and ends at the 95th.
const SINGLE_OBJECT: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"iam:ListUsers","Resource":"*"}}"#;

/// The same statement indented across lines: it opens on line 4 at column 5 and ends on
/// line 8 at column 6, one past the `}` that closes it.
const INDENTED: &str = "{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": [\n    {\n      \"Effect\": \"Allow\",\n      \"Action\": \"iam:ListUsers\",\n      \"Resource\": \"*\"\n    }\n  ]\n}";

/// The minified document after a line break, so the statement is on the document's SECOND
/// line at the same column it was on the first.
const AFTER_A_LINE_BREAK: &str = "\n{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\"Action\":\"iam:ListUsers\",\"Resource\":\"*\"}]}";

/// One row of the location table: the document a request supplies, the decision it reaches
/// for the simulated action, and where in that document the statement that determined it
/// is.
struct Located {
    case: &'static str,
    document: &'static str,
    decision: &'static str,
    /// The `(Line, Column)` the statement's first character is at.
    start: (i32, i32),
    /// The `(Line, Column)` one character past its last.
    end: (i32, i32),
}

/// Rows spanning the document layouts, which statement of the document decided, and the
/// effect that statement states.
const LOCATED: &[Located] = &[
    Located {
        case: "the only statement of a minified document",
        document: MINIFIED,
        decision: ALLOWED,
        start: (1, 38),
        end: (1, 96),
    },
    Located {
        case: "the only statement of a spaced document",
        document: SPACED,
        decision: ALLOWED,
        start: (1, 41),
        end: (1, 104),
    },
    Located {
        case: "the second of two statements",
        document: SECOND_STATEMENT,
        decision: ALLOWED,
        start: (1, 96),
        end: (1, 155),
    },
    Located {
        case: "the second of two statements written with a space after the separator",
        document: SECOND_STATEMENT_SPACED,
        decision: ALLOWED,
        start: (1, 96),
        end: (1, 156),
    },
    Located {
        case: "the deny that overrides an allow before it",
        document: ALLOW_THEN_DENY,
        decision: EXPLICIT_DENY,
        start: (1, 96),
        end: (1, 154),
    },
    Located {
        case: "the single statement an object states",
        document: SINGLE_OBJECT,
        decision: ALLOWED,
        start: (1, 37),
        end: (1, 95),
    },
    Located {
        case: "a statement indented across lines",
        document: INDENTED,
        decision: ALLOWED,
        start: (4, 5),
        end: (8, 6),
    },
    Located {
        case: "a document whose statement is on its second line",
        document: AFTER_A_LINE_BREAK,
        decision: ALLOWED,
        start: (2, 38),
        end: (2, 96),
    },
];

/// RULE 1 and RULE 2 over every row, on both operations: the one entry the result names
/// locates its statement where the row says it is, and the result reports no missing
/// context key — the member present and empty, since no document here tests a condition.
#[tokio::test]
async fn a_matched_statement_is_located_where_the_document_puts_it() {
    let router = router();
    let principal = "located-statements";
    create_user(&router, "the simulated principal", principal, &[]).await;
    let arn = user_arn(principal);

    for case in LOCATED {
        for operation in OPERATIONS {
            let (status, body) =
                simulate(&router, operation, &arn, case.document, LIST_USERS, &[]).await;
            assert_eq!(
                status, 200,
                "case {:?}/{operation}: expected 200, got {status}: {body}",
                case.case
            );
            let decision = element(&body, "EvalDecision");
            assert_eq!(
                decision, case.decision,
                "case {:?}/{operation}: expected the decision {:?}, got {decision:?}: {body}",
                case.case, case.decision
            );
            let starts = positions(&body, "StartPosition");
            let ends = positions(&body, "EndPosition");
            assert_eq!(
                (starts.as_slice(), ends.as_slice()),
                (&[case.start][..], &[case.end][..]),
                "case {:?}/{operation}: expected the statement located at {:?}..{:?}, got \
                 {starts:?}..{ends:?}: {body}",
                case.case,
                case.start,
                case.end
            );
            assert!(
                reports_no_missing_key(&body),
                "case {:?}/{operation}: expected MissingContextValues present and empty, got \
                 {body}",
                case.case
            );
        }
    }
}

/// RULE 1 where the request names RESOURCES, so the report carries a per-resource result
/// beside the action's own: both halves locate the statement, and they locate the same one.
#[tokio::test]
async fn each_part_of_the_report_locates_the_statement_it_names() {
    let router = router();
    let principal = "located-per-resource";
    create_user(&router, "the simulated principal", principal, &[]).await;
    let arn = user_arn(principal);
    // Two resources, so the response carries two `ResourceSpecificResults` entries beside
    // the action's own result; the document allows the action on every resource, so each of
    // the three parts names the one statement.
    let resources = [
        (
            "ResourceArns.member.1".to_string(),
            "arn:aws:s3:::quarterly-reports/q1.csv".to_string(),
        ),
        (
            "ResourceArns.member.2".to_string(),
            "arn:aws:s3:::quarterly-reports/q2.csv".to_string(),
        ),
    ];

    for operation in OPERATIONS {
        let (status, body) = simulate(
            &router,
            operation,
            &arn,
            MINIFIED_OBJECT_READ,
            GET_OBJECT,
            &resources,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {operation:?}: expected 200, got {status}: {body}"
        );
        let starts = positions(&body, "StartPosition");
        let ends = positions(&body, "EndPosition");
        // The action's own result and one per resource: three entries, each locating the
        // same statement of the same document.
        let expected_starts = vec![(1, 38); 3];
        let expected_ends = vec![(1, 95); 3];
        assert_eq!(
            (starts, ends),
            (expected_starts.clone(), expected_ends.clone()),
            "case {operation:?}: expected every part of the report to locate the statement at \
             {expected_starts:?}..{expected_ends:?}, got {body}"
        );
    }
}

/// A statement whose `Allow` is guarded by a condition over a key no request below supplies
/// a value for, so the decision lacked that value: the key the report must name.
const GUARDED: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iam:ListUsers","Resource":"*","Condition":{"StringEquals":{"aws:PrincipalTag/team":"core"}}}]}"#;

/// The key `GUARDED` tests.
const GUARDED_KEY: &str = "aws:PrincipalTag/team";

/// RULE 2 the other way round: the member that is present-and-empty above carries the key
/// when the decision did lack a value for one — so the empty report is a measured answer
/// and not a constant.
#[tokio::test]
async fn a_missing_context_key_is_reported_by_the_result_that_needed_it() {
    let router = router();
    let principal = "located-missing-keys";
    create_user(&router, "the simulated principal", principal, &[]).await;
    let arn = user_arn(principal);

    for operation in OPERATIONS {
        // No `ContextEntries`: the simulation carries no value for the key the condition
        // tests, so the `Allow` cannot be read as applying and nothing else states anything.
        let (status, body) =
            simulate(&router, operation, &arn, GUARDED, LIST_USERS, &[]).await;
        assert_eq!(
            status, 200,
            "case {operation:?}: expected 200, got {status}: {body}"
        );
        let decision = element(&body, "EvalDecision");
        assert_eq!(
            decision, IMPLICIT_DENY,
            "case {operation:?}: expected {IMPLICIT_DENY} for an unsatisfiable condition, got \
             {decision:?}: {body}"
        );
        let reported = missing_keys(&body);
        assert_eq!(
            reported,
            vec![GUARDED_KEY.to_string()],
            "case {operation:?}: expected the result to report {GUARDED_KEY:?} as missing, got \
             {reported:?}: {body}"
        );
        // And the value supplied, the same request lacks nothing: the member is then the
        // empty report, and the decision the condition's own.
        let supplied = [
            (
                "ContextEntries.member.1.ContextKeyName".to_string(),
                GUARDED_KEY.to_string(),
            ),
            (
                "ContextEntries.member.1.ContextKeyValues.member.1".to_string(),
                "core".to_string(),
            ),
            (
                "ContextEntries.member.1.ContextKeyType".to_string(),
                "string".to_string(),
            ),
        ];
        let (status, body) =
            simulate(&router, operation, &arn, GUARDED, LIST_USERS, &supplied).await;
        assert_eq!(
            status, 200,
            "case {operation:?} with the value supplied: expected 200, got {status}: {body}"
        );
        let decision = element(&body, "EvalDecision");
        assert_eq!(
            decision, ALLOWED,
            "case {operation:?} with the value supplied: expected {ALLOWED}, got {decision:?}: \
             {body}"
        );
        assert!(
            reports_no_missing_key(&body),
            "case {operation:?} with the value supplied: expected MissingContextValues present \
             and empty, got {body}"
        );
    }
}

/// The two operations that answer a simulation report, so every row above is asked of both:
/// the projection under test is theirs together.
const OPERATIONS: &[&str] = &["SimulateCustomPolicy", "SimulatePrincipalPolicy"];

/// How many statements one PAGE may report as having determined its results
/// (`crate::state::simulated_decisions::MAX_REPORTED_STATEMENTS_PER_PAGE`, the bound beside
/// the one on the context keys a page may report): the report is carried by every result of
/// the page and by each of its per-resource results, so what is bounded is the PRODUCT of
/// the statements the documents state and the pairs the request asks to have decided — a
/// page past it is refused rather than answered with a truncated report.
const MAX_REPORTED_STATEMENTS_PER_PAGE: usize = 100_000;

/// A document whose `count` statements all allow `action` on every resource, so every one of
/// them contributes to the decision and is reported.
fn allowing_statements(count: usize, action: &str) -> String {
    let statement = format!(r#"{{"Effect":"Allow","Action":"{action}","Resource":"*"}}"#);
    let statements = vec![statement; count];
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{}]}}"#,
        statements.join(",")
    )
}

/// RULE 1 where SEVERAL statements of one document determine the result: each is its own
/// entry, located at its own span — the entry count follows the statements the document
/// states, and no entry stands for two of them.
#[tokio::test]
async fn every_contributing_statement_is_its_own_entry_at_its_own_span() {
    let router = router();
    let principal = "located-many";
    create_user(&router, "the simulated principal", principal, &[]).await;
    let arn = user_arn(principal);

    for operation in OPERATIONS {
        for count in [1usize, 2, 10, 100] {
            let document = allowing_statements(count, LIST_USERS);
            let (status, body) =
                simulate(&router, operation, &arn, &document, LIST_USERS, &[]).await;
            assert_eq!(
                status, 200,
                "case {count} statements/{operation}: expected 200, got {status}: {body}"
            );
            let starts = positions(&body, "StartPosition");
            let distinct: std::collections::BTreeSet<(i32, i32)> = starts.iter().copied().collect();
            assert_eq!(
                (starts.len(), distinct.len()),
                (count, count),
                "case {count} statements/{operation}: expected {count} entries at distinct \
                 positions, got {starts:?}"
            );
        }
    }
}

/// RULE 1 at the bound on how many statements one PAGE may report, and past it: a page whose
/// product of statements and pairs is within the bound is answered, and one past it is
/// refused `InvalidInput` naming the bound rather than answered with a truncated report.
#[tokio::test]
async fn a_page_past_the_report_bound_is_refused_rather_than_truncated() {
    let router = router();
    let principal = "located-page-bound";
    create_user(&router, "the simulated principal", principal, &[]).await;
    let arn = user_arn(principal);
    // One document stating 100 statements that all match: each of the page's pairs reports
    // all 100, and the action's own result reports their union — so a page deciding
    // `resources` pairs reports `100 * resources + 100` statements, which is exactly the
    // bound at 999 resources and past it at 1,000 (the most resources one request may name).
    // The action is the object read, which IS published with a resource type, so every pair
    // the row names is one that action could have been made as.
    let document = allowing_statements(100, GET_OBJECT);

    for operation in OPERATIONS {
        // (case, how many resources the request names, whether the page is answered)
        for (case, resources, answered) in [
            ("exactly the bound", 999usize, true),
            ("past the bound", 1_000usize, false),
        ] {
            let named: Vec<(String, String)> = (1..=resources)
                .map(|at| {
                    (
                        format!("ResourceArns.member.{at}"),
                        format!("arn:aws:s3:::audit-{at}/report.csv"),
                    )
                })
                .collect();
            let (status, body) =
                simulate(&router, operation, &arn, &document, GET_OBJECT, &named).await;
            if answered {
                assert_eq!(
                    status, 200,
                    "case {case:?}/{operation}: expected 200, got {status}"
                );
            } else {
                let code = error_code(&body);
                assert_eq!(
                    (status, code.as_str()),
                    (400, "InvalidInput"),
                    "case {case:?}/{operation}: expected HTTP 400 InvalidInput, got {status} \
                     {code}"
                );
                let message = element(&body, "Message");
                assert!(
                    message.contains(&MAX_REPORTED_STATEMENTS_PER_PAGE.to_string()),
                    "case {case:?}/{operation}: expected the refusal to name the bound \
                     {MAX_REPORTED_STATEMENTS_PER_PAGE}, got {message:?}"
                );
            }
        }
    }
}

/// One call of `operation` simulating `action` against `document`, which the request
/// supplies in `PolicyInputList`.
///
/// `SimulatePrincipalPolicy` additionally names the principal whose own policies join it —
/// the fixture user, which carries none of its own, so both operations decide from the one
/// supplied document.
async fn simulate(
    router: &MultiServiceRouter,
    operation: &str,
    principal_arn: &str,
    document: &str,
    action: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut owned: Vec<(String, String)> = vec![
        (
            "PolicyInputList.member.1".to_string(),
            document.to_string(),
        ),
        ("ActionNames.member.1".to_string(), action.to_string()),
    ];
    if operation == "SimulatePrincipalPolicy" {
        owned.push(("PolicySourceArn".to_string(), principal_arn.to_string()));
    }
    owned.extend(extra.iter().cloned());
    let params: Vec<(&str, &str)> = owned
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    call(router, operation, &params).await
}

/// The `(Line, Column)` of every `<tag>` position element in the body, in document order.
///
/// A `Position` is not a leaf, so the shared leaf reader is taken over each position's own
/// text rather than over the element that carries it. Shared with the sibling suite about the
/// form every modelled member is reported in
/// (`tests/all/query_response_member_form.rs`), so a position is read one way.
pub(crate) fn positions(body: &str, tag: &str) -> Vec<(i32, i32)> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut found = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find(&open) {
        let text = &rest[start + open.len()..];
        let end = text
            .find(&close)
            .unwrap_or_else(|| panic!("{open} is never closed in {body}"));
        let within = &text[..end];
        found.push((number(within, "Line"), number(within, "Column")));
        rest = &text[end + close.len()..];
    }
    found
}

/// The number one `<tag>` leaf carries.
fn number(part: &str, tag: &str) -> i32 {
    let text = element(part, tag);
    text.parse()
        .unwrap_or_else(|_| panic!("<{tag}> must carry a number, got {text:?} in {part}"))
}

/// The keys every result of the body reports as missing a value, in report order.
///
/// `MissingContextValues` carries a member LIST, so the span of each list is found first
/// and the shared leaf reader taken over it.
fn missing_keys(body: &str) -> Vec<String> {
    const OPEN: &str = "<MissingContextValues>";
    const CLOSE: &str = "</MissingContextValues>";
    let mut found = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find(OPEN) {
        let text = &rest[start + OPEN.len()..];
        let end = text
            .find(CLOSE)
            .unwrap_or_else(|| panic!("{OPEN} is never closed in {body}"));
        found.extend(elements(&text[..end], "member"));
        rest = &text[end + CLOSE.len()..];
    }
    found
}

/// Whether the body reports `MissingContextValues` AND it names no key: the member present
/// with no entry in it, which is how the query protocol writes an empty list — either as an
/// element with nothing between its tags or as a self-closed one.
///
/// Shared with the sibling suite named on [`positions`].
pub(crate) fn reports_no_missing_key(body: &str) -> bool {
    let present = body.contains("<MissingContextValues></MissingContextValues>")
        || body.contains("<MissingContextValues/>");
    present && missing_keys(body).is_empty()
}

/// What a document states before its first statement, and the statement the rows below
/// state several of, which allows the action they simulate.
const STATEMENT_PREFIX: &str = r#"{"Version":"2012-10-17","Statement":["#;
const ONE_STATEMENT: &str = r#"{"Effect":"Allow","Action":"iam:ListUsers","Resource":"*"}"#;

/// A document stating `count` identical allowing statements separated by `separator`, with
/// the column its first statement's own `{` is at.
fn separated_statements(count: usize, separator: &str) -> (String, i32) {
    let statements = vec![ONE_STATEMENT; count];
    let document = format!("{STATEMENT_PREFIX}{}]}}", statements.join(separator));
    let opens_at = i32::try_from(STATEMENT_PREFIX.len() + 1).expect("the prefix is short");
    (document, opens_at)
}

/// RULE 1 as the INVARIANT it is: the spans of one document TILE it — each statement ends
/// one character past its own last and begins where the statement before it ended — so how
/// widely the document spaces the separator between two statements moves a statement's own
/// `{` but not the position its span begins at.
#[tokio::test]
async fn each_statement_begins_where_the_statement_before_it_ended() {
    let statement_characters =
        i32::try_from(ONE_STATEMENT.len()).expect("one statement is short");
    let router = router();
    let principal = "located-tiled";
    create_user(&router, "the simulated principal", principal, &[]).await;
    let arn = user_arn(principal);

    // The rows differ along what the rule generalises over: how widely the document spaces
    // the separator between two statements, and how many statements it states.
    for separator in [",", ", ", ",   "] {
        for count in [1usize, 2, 4] {
            let case = format!("{count} statements separated by {separator:?}");
            let (document, opens_at) = separated_statements(count, separator);
            for operation in OPERATIONS {
                let (status, body) =
                    simulate(&router, operation, &arn, &document, LIST_USERS, &[]).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}/{operation}: expected 200, got {status}: {body}"
                );
                // Derived from the rule rather than listed per row: each statement ends
                // `ONE_STATEMENT`'s length past its own `{`, the next one opens a separator
                // later, and a statement begins where the one before it ended.
                let mut expected: Vec<((i32, i32), (i32, i32))> = Vec::new();
                let mut opens = opens_at;
                let mut ended: Option<i32> = None;
                for _ in 0..count {
                    let ends = opens + statement_characters;
                    expected.push(((1, ended.unwrap_or(opens)), (1, ends)));
                    ended = Some(ends);
                    opens = ends + i32::try_from(separator.len()).expect("a short separator");
                }
                let starts = positions(&body, "StartPosition");
                let ends = positions(&body, "EndPosition");
                let located: Vec<((i32, i32), (i32, i32))> =
                    starts.into_iter().zip(ends).collect();
                // Every statement allows the simulated action, so the action's own result
                // names all of them — and the request names no resource, so there is no
                // per-resource result repeating them.
                assert_eq!(
                    located, expected,
                    "case {case:?}/{operation}: expected the statements located at \
                     {expected:?}, got {located:?}: {body}"
                );
            }
        }
    }
}
