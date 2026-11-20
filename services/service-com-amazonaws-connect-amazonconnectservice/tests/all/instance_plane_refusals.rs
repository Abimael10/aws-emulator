// SPDX-License-Identifier: CC0-1.0

//! How Connect's instance plane answers a read it can serve and a read it cannot, driven
//! over HTTP through the same `MultiServiceRouter` the emulator serves with.
//!
//! Two rules, each asserted over a table:
//!
//! RULE (pagination): `ListInstances` applies the two ends of its `MaxResults` member's
//! modelled `@range` (`MaxResult10`, 1..10) DIFFERENTLY. The minimum is a server-side
//! check: a value below it is refused with the operation's own modelled
//! `InvalidRequestException` (HTTP 400) whose `Message` names the member and that
//! minimum. The maximum bounds the PAGE the service builds rather than the request it
//! accepts: a larger value is served, with the page holding at most that maximum. Every
//! success serializes `InstanceSummaryList` — the empty list for an account holding no
//! instances — so a caller can always tell "no instances" from "bad request".
//!
//! RULE (the instance label): an operation whose addressed resource IS an instance
//! checks the `InstanceId` path label before resolving any state, against the two ends of
//! that identifier's own modelled `@length` bound (`InstanceId`, 1..100). A label holding
//! nothing but whitespace carries no identifier, so the request is refused as the
//! malformed request it is — `InvalidRequestException` (400), `Invalid request`. A label
//! longer than the maximum is not an identifier the service can read — the same error,
//! `Instance id is not valid`. Both those sentences are fixed and quote no label. Every
//! other label is RESOLVED, however unlike a minted identifier it looks, and one that
//! holds nothing is `ResourceNotFoundException` (404) naming the instance the request
//! addressed, `Instance <instanceId> not found` — the one sentence of this plane that has a
//! variable part, and that part is the label the request carried.

use serde_json::{json, Value};

use super::harness::{connect_managed, ids_of, Connect};

/// The sentence Connect answers an `InstanceId` that is present but blank with, written
/// out here rather than read off the implementation so the suite pins the text itself.
const INVALID_REQUEST: &str = "Invalid request";

/// The sentence it answers an identifier longer than the modelled maximum with.
const INSTANCE_ID_NOT_VALID: &str = "Instance id is not valid";

/// The sentence it answers an identifier that resolves to no instance with: the instance
/// plane's not-found sentence, written out here rather than read off the implementation so
/// the suite pins the text itself. Its one variable part is the label the request carried.
fn instance_not_found(instance_id: &str) -> String {
    format!("Instance {instance_id} not found")
}

/// The modelled `@length` maximum of `com.amazonaws.connect#InstanceId`
/// (`smithy_query(query="shape",shape="InstanceId")` -> length min 1 max 100), written
/// out here so the table states the bound rather than reading it off the implementation.
const INSTANCE_ID_MAX_LENGTH: usize = 100;

/// The refusal `ListInstances` answers a page size below its modelled minimum with. The
/// variable parts are the member's own name and the violated `@range` bound, both of
/// which this suite writes out as the model spells them.
const MAX_RESULTS_BELOW_MINIMUM: &str =
    "Invalid MaxResults. MaxResults should have minimum value of 1.";

/// The modelled `@range` of `MaxResult10`, the shape `ListInstances`'s `MaxResults`
/// targets (`smithy_query(query="operation",operation="ListInstances")`).
const MAX_RESULTS_MIN: i32 = 1;
/// See [`MAX_RESULTS_MIN`].
const MAX_RESULTS_MAX: i32 = 10;

/// The `Message` member of an answered error — the member name the model gives it.
fn message_of(body: &Value) -> String {
    body["Message"].as_str().unwrap_or_default().to_string()
}

impl Connect {
    /// `ListInstances`: `GET /instance[?maxResults=…][&nextToken=…]`, as the answered
    /// status, error code and body.
    async fn instances(
        &self,
        max_results: Option<i32>,
        token: Option<&str>,
    ) -> super::harness::Answer {
        let mut params: Vec<String> = Vec::new();
        if let Some(max) = max_results {
            params.push(format!("maxResults={max}"));
        }
        if let Some(token) = token {
            params.push(format!("nextToken={token}"));
        }
        let query = if params.is_empty() {
            String::new()
        } else {
            format!("?{}", params.join("&"))
        };
        self.send_answer("GET", &format!("/instance{query}"), None)
            .await
    }
}

/// RULE: a `MaxResults` at or above the modelled minimum is served (200) and always
/// carries `InstanceSummaryList`; one below it is refused with
/// `InvalidRequestException` naming the member and that minimum.
///
/// The rows differ along the VALUE: the member absent, both published bounds, one past
/// the maximum, far past it, the largest value the member's integer type can carry, and
/// three below the minimum including zero and the most negative value.
#[tokio::test]
async fn list_instances_applies_its_page_size_minimum_only() {
    let connect = Connect::new();
    connect.create_instance(connect_managed("paged")).await;

    let rows: Vec<(&str, Option<i32>, bool)> = vec![
        ("no MaxResults at all", None, true),
        ("the modelled minimum", Some(MAX_RESULTS_MIN), true),
        ("inside the range", Some(5), true),
        ("the modelled maximum", Some(MAX_RESULTS_MAX), true),
        ("one past the maximum", Some(MAX_RESULTS_MAX + 1), true),
        ("far past the maximum", Some(1_000), true),
        ("the largest value the type carries", Some(i32::MAX), true),
        ("one below the minimum", Some(MAX_RESULTS_MIN - 1), false),
        ("negative", Some(-7), false),
        ("the most negative value the type carries", Some(i32::MIN), false),
    ];

    for (case, max_results, served) in rows {
        let answer = connect.instances(max_results, None).await;
        if served {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected HTTP 200, got {} {}",
                answer.status, answer.body
            );
            assert!(
                answer.body["InstanceSummaryList"].is_array(),
                "case {case:?}: expected InstanceSummaryList to be serialized, got {}",
                answer.body
            );
            continue;
        }
        let actual = message_of(&answer.body);
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code, "InvalidRequestException",
            "case {case:?}: expected error code InvalidRequestException, got {:?}",
            answer.error_code
        );
        assert_eq!(
            actual, MAX_RESULTS_BELOW_MINIMUM,
            "case {case:?}: expected message {MAX_RESULTS_BELOW_MINIMUM:?}, got {actual:?}"
        );
        let rejected = max_results.expect("a refused row sends a value").to_string();
        assert!(
            !answer.body.to_string().contains(&rejected),
            "case {case:?}: the refusal must not quote the rejected value, got {}",
            answer.body
        );
    }
}

/// RULE (the other half of the success path): an account holding no instances is reported
/// as `InstanceSummaryList: []` with no `NextToken`, not as an absent member and not as a
/// refusal — which is what lets a caller tell an empty account from a bad request.
///
/// The rows are the page sizes an enumerating client sends: none at all, both published
/// bounds, and one past the maximum.
#[tokio::test]
async fn an_account_with_no_instances_lists_the_empty_list() {
    let connect = Connect::new();

    for max_results in [None, Some(MAX_RESULTS_MIN), Some(MAX_RESULTS_MAX), Some(99)] {
        let answer = connect.instances(max_results, None).await;
        assert_eq!(
            answer.status, 200,
            "case maxResults={max_results:?}: expected HTTP 200, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.body["InstanceSummaryList"],
            json!([]),
            "case maxResults={max_results:?}: expected the empty list, got {}",
            answer.body
        );
        assert!(
            answer.body["NextToken"].is_null(),
            "case maxResults={max_results:?}: the only page carries no NextToken, got {}",
            answer.body
        );
    }
}

/// RULE (the maximum end): a `MaxResults` above the modelled maximum is served with the
/// page bounded at that maximum, and the walk that follows still covers every instance
/// exactly once.
///
/// The rows differ along how far past the maximum the request asks, against a population
/// larger than the maximum so a page that was not bounded would be visible.
#[tokio::test]
async fn a_page_size_above_the_maximum_is_bounded_by_it() {
    let connect = Connect::new();
    let mut created: Vec<String> = Vec::new();
    for index in 0..(MAX_RESULTS_MAX as usize + 2) {
        let (id, _) = connect
            .create_instance(connect_managed(&format!("org{index}")))
            .await;
        created.push(id);
    }
    created.sort();

    for max_results in [MAX_RESULTS_MAX + 1, MAX_RESULTS_MAX + 2, 1_000, i32::MAX] {
        let case = format!("maxResults={max_results}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let answer = connect.instances(Some(max_results), token.as_deref()).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected HTTP 200, got {} {}",
                answer.status, answer.body
            );
            pages += 1;
            let page = ids_of(&answer.body["InstanceSummaryList"], "Id");
            assert!(
                page.len() <= MAX_RESULTS_MAX as usize,
                "case {case:?}: a page holds at most {MAX_RESULTS_MAX} items, got {} ({page:?})",
                page.len()
            );
            seen.extend(page);
            match answer.body["NextToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
            assert!(pages < 64, "case {case:?}: ListInstances did not terminate");
        }
        assert_eq!(
            pages, 2,
            "case {case:?}: a population of {} over pages of {MAX_RESULTS_MAX} is two pages, \
             got {pages}",
            created.len()
        );
        let mut sorted = seen.clone();
        sorted.sort();
        assert_eq!(
            sorted, created,
            "case {case:?}: the pages must cover exactly the created instances, got {seen:?}"
        );
    }
}

/// One operation whose addressed resource is an instance: its name and the HTTP method it
/// is sent as, over the one URI the model binds for both
/// (`smithy_query(query="operation",operation="DescribeInstance,DeleteInstance")` ->
/// `/instance/{InstanceId}`).
const INSTANCE_PLANE: [(&str, &str); 2] = [("DescribeInstance", "GET"), ("DeleteInstance", "DELETE")];

/// What the instance plane answers one label with.
struct Expected {
    /// The HTTP status.
    status: u16,
    /// The `x-amzn-errortype` the modelled error shape is named by.
    code: &'static str,
    /// The `Message` the error carries.
    message: String,
}

/// RULE: the instance plane places its `InstanceId` label in one of three classes before
/// resolving any state — carrying no identifier, longer than the identifier's modelled
/// `@length` maximum, or a label to resolve — and the first two are 400s with fixed
/// sentences, not 404s; the third, when it resolves nothing, is the 404 carrying the
/// not-found sentence.
///
/// The rows differ along the FORM and LENGTH of the label: three ways to carry no
/// identifier, both ends of the modelled bound (1..100) and two past the maximum, and
/// labels inside the bound that look nothing like a minted identifier yet are still
/// resolved. Each row runs against every operation of the plane.
#[tokio::test]
async fn the_instance_plane_classifies_its_label_before_resolving() {
    let connect = Connect::new();
    // A live instance stands beside the refused labels, so a row cannot pass merely
    // because the store is empty.
    let (live_id, _) = connect.create_instance(connect_managed("neighbour")).await;

    // Which class the row's label falls in. The 404 names the instance the request
    // addressed, so its sentence is built from the row's own label.
    #[derive(Clone, Copy)]
    enum Class {
        Blank,
        TooLong,
        Absent,
    }

    // (case, the label as the request carries it, the label after decoding, the class)
    let rows: Vec<(&str, String, String, Class)> = vec![
        ("one space", "%20".to_string(), " ".to_string(), Class::Blank),
        (
            "several spaces",
            "%20%20%20".to_string(),
            "   ".to_string(),
            Class::Blank,
        ),
        ("a tab", "%09".to_string(), "\t".to_string(), Class::Blank),
        (
            "one past the modelled maximum length",
            "i".repeat(INSTANCE_ID_MAX_LENGTH + 1),
            "i".repeat(INSTANCE_ID_MAX_LENGTH + 1),
            Class::TooLong,
        ),
        (
            "far past the modelled maximum length",
            "i".repeat(INSTANCE_ID_MAX_LENGTH * 3),
            "i".repeat(INSTANCE_ID_MAX_LENGTH * 3),
            Class::TooLong,
        ),
        (
            "a name, not an identifier",
            "support".to_string(),
            "support".to_string(),
            Class::Absent,
        ),
        (
            "a prefix of an identifier",
            "7f3c9b21".to_string(),
            "7f3c9b21".to_string(),
            Class::Absent,
        ),
        (
            "the right groups, a non-hex character",
            "1111111z-2222-3333-4444-555555555555".to_string(),
            "1111111z-2222-3333-4444-555555555555".to_string(),
            Class::Absent,
        ),
        (
            "one character, the modelled minimum length",
            "a".to_string(),
            "a".to_string(),
            Class::Absent,
        ),
        (
            "the modelled maximum length",
            "i".repeat(INSTANCE_ID_MAX_LENGTH),
            "i".repeat(INSTANCE_ID_MAX_LENGTH),
            Class::Absent,
        ),
        (
            "an identifier of the minted form that holds nothing",
            "7f3c9b21-0d4e-4a86-9c15-2b6f8e5d1a30".to_string(),
            "7f3c9b21-0d4e-4a86-9c15-2b6f8e5d1a30".to_string(),
            Class::Absent,
        ),
        (
            "the same identifier in upper case",
            "7F3C9B21-0D4E-4A86-9C15-2B6F8E5D1A30".to_string(),
            "7F3C9B21-0D4E-4A86-9C15-2B6F8E5D1A30".to_string(),
            Class::Absent,
        ),
    ];

    for (case, sent, label, class) in rows {
        let expected = match class {
            Class::Blank => Expected {
                status: 400,
                code: "InvalidRequestException",
                message: INVALID_REQUEST.to_string(),
            },
            Class::TooLong => Expected {
                status: 400,
                code: "InvalidRequestException",
                message: INSTANCE_ID_NOT_VALID.to_string(),
            },
            Class::Absent => Expected {
                status: 404,
                code: "ResourceNotFoundException",
                message: instance_not_found(&label),
            },
        };
        for (operation, method) in INSTANCE_PLANE {
            assert_ne!(
                label, live_id,
                "case {case:?}: the row has to name an instance the store does not hold"
            );
            let answer = connect
                .send_answer(method, &format!("/instance/{sent}"), None)
                .await;
            let actual = message_of(&answer.body);
            assert_eq!(
                answer.status, expected.status,
                "case {case:?}/{operation}: expected HTTP {}, got {} {}",
                expected.status, answer.status, answer.body
            );
            assert_eq!(
                answer.error_code, expected.code,
                "case {case:?}/{operation}: expected error code {}, got {:?}",
                expected.code, answer.error_code
            );
            assert_eq!(
                actual, expected.message,
                "case {case:?}/{operation}: expected message {:?}, got {actual:?}",
                expected.message
            );
            // The two 400 sentences are fixed, so the label cannot appear in those
            // replies. Only a label long enough not to occur in a sentence by accident is
            // checked: a one-character label is a letter of the sentence itself.
            if matches!(class, Class::Blank | Class::TooLong) && label.trim().len() >= 4 {
                assert!(
                    !answer.body.to_string().contains(label.trim()),
                    "case {case:?}/{operation}: the refusal must not quote the label, got {}",
                    answer.body
                );
            }
        }
    }

    // The live instance is untouched by every row above: the plane still serves it.
    let read = connect
        .send_answer("GET", &format!("/instance/{live_id}"), None)
        .await;
    assert_eq!(
        read.status, 200,
        "the live instance is still readable, got {} {}",
        read.status, read.body
    );
}

/// RULE (the round trip the two classes sit either side of): an identifier of the minted
/// form is RESOLVED — a created instance reads back through `DescribeInstance`, lists
/// through `ListInstances`, and once `DeleteInstance` retires it both the delete and the
/// read answer the not-found sentence.
#[tokio::test]
async fn a_minted_identifier_is_resolved_through_the_whole_plane() {
    let connect = Connect::new();
    let (instance_id, instance_arn) = connect.create_instance(connect_managed("roundtrip")).await;

    let read = connect
        .send_answer("GET", &format!("/instance/{instance_id}"), None)
        .await;
    assert_eq!(
        read.status, 200,
        "DescribeInstance of a created instance: got {} {}",
        read.status, read.body
    );
    assert_eq!(
        read.body["Instance"]["Id"].as_str(),
        Some(instance_id.as_str()),
        "DescribeInstance must report the created instance, got {}",
        read.body
    );
    assert_eq!(
        read.body["Instance"]["Arn"].as_str(),
        Some(instance_arn.as_str()),
        "DescribeInstance must report the created ARN, got {}",
        read.body
    );

    let listed = connect.instances(None, None).await;
    assert_eq!(
        ids_of(&listed.body["InstanceSummaryList"], "Id"),
        vec![instance_id.clone()],
        "ListInstances must report the created instance, got {}",
        listed.body
    );

    let deleted = connect
        .send_answer("DELETE", &format!("/instance/{instance_id}"), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "DeleteInstance of a live instance: got {} {}",
        deleted.status, deleted.body
    );

    for (operation, method) in INSTANCE_PLANE {
        let answer = connect
            .send_answer(method, &format!("/instance/{instance_id}"), None)
            .await;
        let actual = message_of(&answer.body);
        let expected = instance_not_found(&instance_id);
        assert_eq!(
            answer.status, 404,
            "{operation} of a retired identifier: expected HTTP 404, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            actual, expected,
            "{operation} of a retired identifier: expected {expected:?}, got {actual:?}"
        );
    }

    let empty = connect.instances(None, None).await;
    assert_eq!(
        empty.body["InstanceSummaryList"],
        json!([]),
        "the account is empty again, got {}",
        empty.body
    );
}
