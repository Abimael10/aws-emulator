// SPDX-License-Identifier: CC0-1.0

//! The ONE pagination-token rule of this service, driven end-to-end over the
//! wire (`awsJson1_0`) through the same multi-service router the emulator boots,
//! and through EVERY paginated read it serves.
//!
//! The rule: a paginated read starts at the row its `nextToken` names — no token
//! at all asking for the first row — and a token that read's own list did not
//! issue names no row to resume at, so it is refused with HTTP 400
//! `ValidationException` attributed to `nextToken`, rather than silently served
//! the first page again.
//!
//! Every paginated operation of this service judges a token the same way, so the
//! table below spans the operations (each of which used to carry its own copy of
//! the branch and the refusal) against the forms a token can take: absent,
//! issued by this list, issued by ANOTHER list of the same service, and four
//! kinds of value this service never issued. The two answers a case asserts are
//! the whole refusal body (status, code, `reason`, `message` and the single
//! `fields` entry, which differ between operations only in the operation named)
//! and, for an accepted token, the rows the page resumes at.
//!
//! A token's ROW is part of what the list issued: a list hands a token out only
//! for a page that has rows before it and rows after it, so the rows it ever
//! resumes at are the ones inside the collection being read and never its first.
//! The suite therefore spans, for every paginated read, the rows a token can
//! name — the first row, one past the last, further past it, and a row inside
//! the collection — including a token this service really issued for a WIDER
//! scope of the same list and then presented to a narrower one.
//!
//! WHICH list a token belongs to is the other half of that: most of these reads
//! answer a different list for every value of the members that select one — the
//! grouping, the narrowing, the ordering, the period — so a token names a row of
//! the list those members selected and of no other, however many rows another
//! list of the same operation happens to have. The last two tables below span
//! that: the members each read selects its list by, against the one member a
//! case changes before presenting the token, and the complement, the member that
//! selects no list at all (`maxResults`, which bounds the page rather than the
//! list, so one list can be walked with a different page size per call).


use serde_json::{json, Value};
use service_com_amazonaws_costoptimizationhub_costoptimizationhubservice as cost_optimization_hub;
use smithy_server_runtime::MultiServiceRouter;

use crate::cost_optimization_hub::{
    call, daily_period, enrolled_router, fields, management_account_router_with_store,
    monthly_period, ok, record_enrollment, FIELD_VALIDATION_FAILED, FIELD_VALIDATION_SUMMARY,
};

/// Member accounts of the organization the store records: twelve digits, which
/// is what the `AccountId` shape's `^[0-9]{12}$` pattern admits, and neither of
/// them the caller — recorded so the organization-wide enrollment read has more
/// than one row and so pages at all.
const MEMBER_ACTIVE: &str = "111122223333";
const MEMBER_INACTIVE: &str = "444455556666";

/// Every paginated read of this service whose rows are a list of records, with
/// the request that makes it answer more than one row (so a page size of one
/// issues a token) — the operations that each enforce the token rule.
///
/// `ListEfficiencyMetrics` is this service's fourth paginated read; it pages
/// over GROUPS of a time series rather than records, so the same rule is driven
/// through it by the `efficiency_metrics` suite instead of by this table.
fn paginated_reads() -> Vec<(&'static str, Value)> {
    vec![
        (
            "ListRecommendations",
            json!({"includeAllRecommendations": true}),
        ),
        (
            "ListRecommendationSummaries",
            json!({"groupBy": "ResourceType"}),
        ),
        (
            "ListEnrollmentStatuses",
            json!({"includeOrganizationInfo": true}),
        ),
    ]
}

/// A router whose every paginated read answers more than one row: the caller is
/// enrolled with its analysis already due (so the catalog is readable), and the
/// organization it manages has two member accounts beside it.
///
/// The records are written through the shared harness because this emulator has
/// no Organizations control plane, so no operation creates another account's
/// enrollment.
fn paged_router() -> MultiServiceRouter {
    let (router, store) = management_account_router_with_store();
    record_enrollment(&store, engine::emulator_account_id(), "Active", true);
    record_enrollment(&store, MEMBER_ACTIVE, "Active", true);
    record_enrollment(&store, MEMBER_INACTIVE, "Inactive", false);
    router
}

/// The request one case sends: the operation's base request with the page size
/// and the token, if any, the case is about.
fn request(base: &Value, max_results: Option<u32>, token: Option<&str>) -> Value {
    let mut request = base.clone();
    if let Some(max_results) = max_results {
        request["maxResults"] = json!(max_results);
    }
    if let Some(token) = token {
        request["nextToken"] = json!(token);
    }
    request
}

/// One served page: the rows it carries and the token that resumes after them.
async fn page(
    router: &MultiServiceRouter,
    operation: &str,
    base: &Value,
    max_results: Option<u32>,
    token: Option<&str>,
) -> (Vec<Value>, Option<String>) {
    let payload = ok(router, operation, &request(base, max_results, token)).await;
    let items = payload["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {operation}: expected a list of items in {payload}"))
        .clone();
    let next = payload["nextToken"].as_str().map(str::to_string);
    (items, next)
}

/// The forms of `nextToken` a read never issued, so each is refused: a value
/// that is not the encoding this service's tokens are carried in, a well-formed
/// encoding of a body this service never writes, a token this service issues for
/// a list of another kind, the empty value, and one of the read's OWN tokens
/// with a byte appended.
fn foreign_tokens(own: &str) -> Vec<(String, String)> {
    vec![
        (
            "not the token encoding".to_string(),
            "not-a-token".to_string(),
        ),
        (
            "a well-formed encoding of a body this service never issues".to_string(),
            hex::encode("page=1"),
        ),
        (
            "a token for a list of another kind".to_string(),
            cost_optimization_hub::state::encode_page_token("no-such-list", 1),
        ),
        ("the empty value".to_string(), String::new()),
        (
            "this list's own token with a byte appended".to_string(),
            format!("{own}00"),
        ),
    ]
}

/// Assert `payload` is the refusal every paginated read of this service answers
/// a token it did not issue with: HTTP 400 `ValidationException` whose `message`
/// states the rejected member and which list would have issued an acceptable
/// token, `reason` = `FieldValidationFailed`, and one `fields` entry attributing
/// the same diagnosis to `nextToken`.
fn assert_refused(case: &str, operation: &str, status: u16, payload: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {payload}"
    );
    let code = payload["__type"].as_str().unwrap_or_default();
    assert!(
        code.ends_with("ValidationException"),
        "case {case:?}: expected a ValidationException, got {code:?} in {payload}"
    );
    let message = payload["message"].as_str().unwrap_or_default();
    let expected_message =
        format!("Invalid nextToken specified. The token was not issued by {operation}.");
    assert_eq!(
        message, expected_message,
        "case {case:?}: expected {expected_message:?}, got {message:?} in {payload}"
    );
    assert_ne!(
        message, FIELD_VALIDATION_SUMMARY,
        "case {case:?}: expected the refusal to state the rule rather than the \
         field-validation summary, got {payload}"
    );
    let reason = payload["reason"].as_str().unwrap_or_default();
    assert_eq!(
        reason, FIELD_VALIDATION_FAILED,
        "case {case:?}: expected reason {FIELD_VALIDATION_FAILED:?}, got {reason:?} in {payload}"
    );
    let expected_fields = vec![(
        "nextToken".to_string(),
        format!("the value is not a pagination token issued by {operation}"),
    )];
    let answered = fields(payload);
    assert_eq!(
        answered, expected_fields,
        "case {case:?}: expected fields {expected_fields:?}, got {answered:?} in {payload}"
    );
}

/// Every paginated read refuses a `nextToken` it did not issue — whichever form
/// that value takes, including a token another list of the same service issued —
/// with the one refusal body, naming the list that would have issued an
/// acceptable token.
#[tokio::test]
async fn a_token_a_read_did_not_issue_is_refused_by_every_paginated_read() {
    let router = paged_router();
    let reads = paginated_reads();

    // One token genuinely issued by each list, so a case can hand a read a token
    // that is valid for a DIFFERENT list of the same service.
    let mut issued: Vec<(&str, String)> = Vec::new();
    for (operation, base) in &reads {
        let (items, token) = page(&router, operation, base, Some(1), None).await;
        assert_eq!(
            items.len(),
            1,
            "case {operation}: expected a page of one row, got {items:?}"
        );
        let token = token.unwrap_or_else(|| {
            panic!("case {operation}: expected a token after a page of one row, got none")
        });
        issued.push((operation, token));
    }

    for (operation, base) in &reads {
        let own = &issued
            .iter()
            .find(|(named, _)| named == operation)
            .expect("every read issued a token above")
            .1;

        let mut refused: Vec<(String, String)> = foreign_tokens(own);
        for (other, token) in &issued {
            if other != operation {
                refused.push((format!("a token issued by {other}"), token.clone()));
            }
        }

        for (form, token) in &refused {
            let case = format!("{operation} given {form}");
            let (status, payload) =
                call(&router, operation, &request(base, Some(1), Some(token))).await;
            assert_refused(&case, operation, status, &payload);
        }
    }
}

/// The other half of the same rule: a read with no `nextToken` starts at the
/// first row, and a token the read itself issued resumes at the row the previous
/// page stopped at — so walking a list one row at a time yields exactly the rows
/// the unpaged read answers, in the same order, and the last page carries no
/// token.
#[tokio::test]
async fn an_absent_token_starts_at_the_first_row_and_an_issued_one_resumes() {
    let router = paged_router();

    for (operation, base) in paginated_reads() {
        let (whole, token) = page(&router, operation, &base, None, None).await;
        assert!(
            whole.len() > 1,
            "case {operation}: expected more than one row to page over, got {whole:?}"
        );
        assert!(
            token.is_none(),
            "case {operation}: a read of every row carries no resume token, got {token:?}"
        );

        // Walk the list a row at a time, bounded by the rows the unpaged read
        // answered plus one, so a token that never stops fails rather than hangs.
        let mut walked: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..=whole.len() {
            let (items, next) =
                page(&router, operation, &base, Some(1), cursor.as_deref()).await;
            walked.extend(items);
            match next {
                None => break,
                Some(next) => cursor = Some(next),
            }
        }
        assert_eq!(
            walked, whole,
            "case {operation}: expected paging to yield the unpaged rows {whole:?}, \
             got {walked:?}"
        );
    }
}

/// A `nextToken` in the encoding `issued` is carried in — the one the list that
/// issued `issued` writes its tokens in — naming `row` instead.
///
/// Built from a token the list really handed out, so a case states only the row
/// it is about and nothing about how this service encodes a token or which list
/// it belongs to.
fn token_at_row(issued: &str, row: usize) -> String {
    let decoded = hex::decode(issued).expect("an issued token is hex");
    let text = String::from_utf8(decoded).expect("an issued token decodes to text");
    let (head, _) = text
        .rsplit_once(':')
        .expect("an issued token carries the row it resumes at last");
    hex::encode(format!("{head}:{row}"))
}

/// A row of a paginated read is a row a token can name only when the list could
/// have issued a token for it: a list issues one only for a page with rows
/// before it and rows after it, so the first row and every row from the end of
/// the collection onwards were never issued, whatever else the token looks like.
/// Each is refused with the one refusal body, not answered with the empty final
/// page a completed listing answers.
#[tokio::test]
async fn a_token_naming_a_row_this_list_never_issued_is_refused_by_every_paginated_read() {
    let router = paged_router();

    for (operation, base) in paginated_reads() {
        let (whole, _) = page(&router, operation, &base, None, None).await;
        let rows = whole.len();
        assert!(
            rows > 1,
            "case {operation}: expected more than one row to page over, got {whole:?}"
        );

        // A token this list genuinely issued, so the cases differ from it only in
        // the row they name.
        let (_, issued) = page(&router, operation, &base, Some(1), None).await;
        let issued = issued.unwrap_or_else(|| {
            panic!("case {operation}: expected a token after a page of one row, got none")
        });

        let never_issued: Vec<(String, usize)> = vec![
            ("the first row of the collection".to_string(), 0),
            ("the row one past the last".to_string(), rows),
            ("two rows past the last".to_string(), rows + 1),
            ("a row far past the last".to_string(), rows + 4_000),
        ];
        for (form, row) in &never_issued {
            let case = format!("{operation} given a token naming {form}");
            let token = token_at_row(&issued, *row);
            let (status, payload) =
                call(&router, operation, &request(&base, None, Some(&token))).await;
            assert_refused(&case, operation, status, &payload);
            assert!(
                payload["items"].is_null(),
                "case {case:?}: expected a refusal rather than a page of rows, got {payload}"
            );
        }

        // The complement, from the same table: the last row a token of this list
        // can name is served, and it resumes exactly there — so the boundary is
        // the end of the collection and not the shape of any one request.
        let last = rows - 1;
        let case = format!("{operation} given a token naming its last resumable row");
        let (items, next) = page(
            &router,
            operation,
            &base,
            None,
            Some(&token_at_row(&issued, last)),
        )
        .await;
        assert_eq!(
            items,
            whole[last..].to_vec(),
            "case {case:?}: expected the rows from {last} on ({:?}), got {items:?}",
            &whole[last..]
        );
        assert!(
            next.is_none(),
            "case {case:?}: a page ending at the last row carries no token, got {next:?}"
        );
    }
}

/// The same rule holds for a token this service really issued: the rows it can
/// resume at are the rows of the collection the read it is presented to answers,
/// so a token the organization-wide enrollment read issued names no row of the
/// caller's own one-record read and is refused there, rather than answered with
/// the empty final page.
#[tokio::test]
async fn an_issued_token_naming_a_row_outside_the_read_it_is_given_to_is_refused() {
    let router = paged_router();
    let operation = "ListEnrollmentStatuses";
    let organization = json!({"includeOrganizationInfo": true});
    let caller_only = json!({"includeOrganizationInfo": false});

    // The organization's view pages over the caller and its two members, so its
    // second page resumes at a row the caller's own one-record view does not have.
    let (_, issued) = page(&router, operation, &organization, Some(2), None).await;
    let issued = issued.unwrap_or_else(|| {
        panic!("case {operation}: expected a token after two of three rows, got none")
    });

    let (own, _) = page(&router, operation, &caller_only, None, None).await;
    assert_eq!(
        own.len(),
        1,
        "case setup: the caller's own read answers its one record, got {own:?}"
    );

    let (status, payload) = call(
        &router,
        operation,
        &request(&caller_only, None, Some(&issued)),
    )
    .await;
    assert_refused(
        "ListEnrollmentStatuses given the organization view's token",
        operation,
        status,
        &payload,
    );
}

// ---------------------------------------------------------------------------
// WHICH list a token belongs to
// ---------------------------------------------------------------------------

/// The member a paginated read of this service carries its rows in, as the model
/// paginates it (`smithy_query(query="operation", operation="…")`:
/// `paginated.items`): `items` for the reads of records and groups of records,
/// and `efficiencyMetricsByGroup` for the metrics read.
const ITEMS: &str = "items";
const EFFICIENCY_ITEMS: &str = "efficiencyMetricsByGroup";

/// One case of WHICH list a token belongs to: the request that mints the token,
/// and the request of the SAME operation it is then presented to, differing from
/// it in one member that selects which of that operation's lists is answered.
struct OtherList {
    case: &'static str,
    operation: &'static str,
    items: &'static str,
    minting: Value,
    presenting: Value,
}

/// The rows one answer carries, out of the member its operation paginates.
fn rows(payload: &Value, items: &str) -> Vec<Value> {
    payload[items]
        .as_array()
        .unwrap_or_else(|| panic!("case {items}: expected a list of rows in {payload}"))
        .clone()
}

/// Every selecting member of every paginated read whose lists can each carry
/// more than one row, against the value of it a case changes before presenting
/// the token: the grouping, the narrowing, the ordering, the de-duplication, the
/// granularity and the period.
///
/// `ListEnrollmentStatuses` selects its list by the accounts the request
/// addresses, and every scope of it narrower than the organization's view
/// addresses ONE account, so its cases are the ones pinned by
/// `an_issued_token_naming_a_row_outside_the_read_it_is_given_to_is_refused`
/// above.
fn other_lists() -> Vec<OtherList> {
    let (start, end) = monthly_period(2, 2);
    let (recent_start, recent_end) = monthly_period(1, 1);
    let (day_start, day_end) = daily_period(5, 3);
    vec![
        OtherList {
            case: "ListRecommendationSummaries grouped by another dimension",
            operation: "ListRecommendationSummaries",
            items: ITEMS,
            minting: json!({"groupBy": "ActionType"}),
            presenting: json!({"groupBy": "Region"}),
        },
        OtherList {
            case: "ListRecommendationSummaries narrowed by a filter the minting read did not carry",
            operation: "ListRecommendationSummaries",
            items: ITEMS,
            minting: json!({"groupBy": "ResourceType"}),
            presenting: json!({
                "groupBy": "ResourceType",
                "filter": {"actionTypes": ["Upgrade"]},
            }),
        },
        OtherList {
            case: "ListRecommendations narrowed by a filter the minting read did not carry",
            operation: "ListRecommendations",
            items: ITEMS,
            minting: json!({"includeAllRecommendations": true}),
            presenting: json!({
                "includeAllRecommendations": true,
                "filter": {"actionTypes": ["Upgrade"]},
            }),
        },
        OtherList {
            case: "ListRecommendations ordered by a dimension the minting read did not ask for",
            operation: "ListRecommendations",
            items: ITEMS,
            minting: json!({"includeAllRecommendations": true}),
            presenting: json!({
                "includeAllRecommendations": true,
                "orderBy": {"dimension": "EstimatedMonthlySavings", "order": "Desc"},
            }),
        },
        OtherList {
            case: "ListRecommendations de-duplicated per resource where the minting read was not",
            operation: "ListRecommendations",
            items: ITEMS,
            minting: json!({"includeAllRecommendations": true}),
            presenting: json!({"includeAllRecommendations": false}),
        },
        OtherList {
            case: "ListEfficiencyMetrics grouped by another dimension",
            operation: "ListEfficiencyMetrics",
            items: EFFICIENCY_ITEMS,
            minting: json!({
                "granularity": "Monthly",
                "timePeriod": {"start": start.clone(), "end": end.clone()},
                "groupBy": "ResourceType",
            }),
            presenting: json!({
                "granularity": "Monthly",
                "timePeriod": {"start": start.clone(), "end": end.clone()},
                "groupBy": "Region",
            }),
        },
        OtherList {
            case: "ListEfficiencyMetrics over another period",
            operation: "ListEfficiencyMetrics",
            items: EFFICIENCY_ITEMS,
            minting: json!({
                "granularity": "Monthly",
                "timePeriod": {"start": start.clone(), "end": end.clone()},
                "groupBy": "ResourceType",
            }),
            presenting: json!({
                "granularity": "Monthly",
                "timePeriod": {"start": recent_start, "end": recent_end},
                "groupBy": "ResourceType",
            }),
        },
        OtherList {
            case: "ListEfficiencyMetrics at another granularity",
            operation: "ListEfficiencyMetrics",
            items: EFFICIENCY_ITEMS,
            minting: json!({
                "granularity": "Monthly",
                "timePeriod": {"start": start, "end": end},
                "groupBy": "ResourceType",
            }),
            presenting: json!({
                "granularity": "Daily",
                "timePeriod": {"start": day_start, "end": day_end},
                "groupBy": "ResourceType",
            }),
        },
    ]
}

/// A token names a row of the list the request that minted it selected, so
/// presenting it to another list of the SAME operation is presenting a token
/// that list did not issue: it is refused with the one refusal body, and not
/// served as a page starting in the middle of a list no page of which ever ended
/// there.
///
/// Each case asserts that the list being refused has rows past the one the token
/// names — so what refuses the token is which list it belongs to and not the row
/// it carries — and, as the complement, that the list that DID issue it resumes
/// exactly where its page stopped.
#[tokio::test]
async fn a_token_names_a_row_of_the_list_that_issued_it_and_of_no_other() {
    let router = enrolled_router().await;

    for other in other_lists() {
        let case = other.case;
        let minted = ok(
            &router,
            other.operation,
            &request(&other.minting, Some(1), None),
        )
        .await;
        let first = rows(&minted, other.items);
        assert_eq!(
            first.len(),
            1,
            "case {case:?}: expected a page of one row to mint a token from, got {first:?}"
        );
        let token = minted["nextToken"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a token after a page of one row, got {minted}")
            })
            .to_string();

        // The list the token is presented to has a row past the one the token
        // names, so served as a page this request would answer that list from its
        // second row on and silently drop its first.
        let other_whole = rows(
            &ok(&router, other.operation, &other.presenting).await,
            other.items,
        );
        assert!(
            other_whole.len() > 1,
            "case {case:?}: expected the list presented to to have a row past the one the \
             token names, got {other_whole:?}"
        );

        let (status, payload) = call(
            &router,
            other.operation,
            &request(&other.presenting, None, Some(&token)),
        )
        .await;
        assert_refused(case, other.operation, status, &payload);
        assert!(
            payload[other.items].is_null(),
            "case {case:?}: expected a refusal rather than a page of rows, got {payload}"
        );

        // The complement: the list that issued the token resumes at the row it
        // names, so the refusal above is about which list and not about the
        // token.
        let own_whole = rows(
            &ok(&router, other.operation, &other.minting).await,
            other.items,
        );
        let resumed = rows(
            &ok(
                &router,
                other.operation,
                &request(&other.minting, None, Some(&token)),
            )
            .await,
            other.items,
        );
        assert_eq!(
            resumed,
            own_whole[1..].to_vec(),
            "case {case:?}: expected the issuing list's rows from the second on ({:?}), \
             got {resumed:?}",
            &own_whole[1..]
        );
    }
}

/// The member that selects no list: `maxResults` bounds the page, not the list,
/// so a token is accepted whatever page size the call presenting it asks for and
/// a walk may change its page size mid-list.
///
/// Spans every paginated read: the three whose list the request selects, each
/// through the request that mints its token above, and the enrollment read
/// through the organization's view.
#[tokio::test]
async fn a_token_resumes_its_list_whatever_page_size_the_next_call_asks_for() {
    let catalog = enrolled_router().await;
    let organization = paged_router();
    let others = other_lists();
    let mut reads: Vec<(&str, &str, &str, Value, &MultiServiceRouter)> = others
        .iter()
        .map(|other| {
            (
                other.case,
                other.operation,
                other.items,
                other.minting.clone(),
                &catalog,
            )
        })
        .collect();
    reads.push((
        "ListEnrollmentStatuses over the organization's view",
        "ListEnrollmentStatuses",
        ITEMS,
        json!({"includeOrganizationInfo": true}),
        &organization,
    ));

    for (case, operation, items, base, router) in reads {
        let whole = rows(&ok(router, operation, &base).await, items);
        assert!(
            whole.len() > 1,
            "case {case:?}: expected more than one row to page over, got {whole:?}"
        );
        let minted = ok(router, operation, &request(&base, Some(1), None)).await;
        let token = minted["nextToken"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a token after a page of one row, got {minted}")
            })
            .to_string();

        // Every page size the next call can ask for, including the one the page
        // that issued the token used and none at all.
        for size in [None, Some(1), Some(2), Some(whole.len() as u32 + 1)] {
            let resumed = rows(
                &ok(router, operation, &request(&base, size, Some(&token))).await,
                items,
            );
            let want = match size {
                None => whole[1..].to_vec(),
                Some(size) => {
                    let end = (1 + size as usize).min(whole.len());
                    whole[1..end].to_vec()
                }
            };
            assert_eq!(
                resumed, want,
                "case {case:?} with maxResults {size:?}: expected {want:?}, got {resumed:?}"
            );
        }
    }
}

