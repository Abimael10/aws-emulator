// SPDX-License-Identifier: CC0-1.0

//! Connect's instance-attribute READ plane over HTTP, through the same
//! `MultiServiceRouter` the emulator serves with: `ListInstanceAttributes` beside the
//! `DescribeInstanceAttribute` read of one toggle, and how both refuse an `AttributeType`
//! that names no toggle at all.
//!
//! The rules under test, each asserted over a SET of inputs rather than one request:
//!
//! RULE (the listing): `ListInstanceAttributes` serves an existing instance HTTP 200 with
//! `Attributes` — every attribute type that instance holds a value for, in ascending
//! attribute-type order, each carrying the same value the single-attribute read reports —
//! paged by `MaxResults` with `NextToken` set exactly while toggles remain. Only the
//! MINIMUM end of that member's modelled `@range` (`MaxResult7`, 1..7) is a check on the
//! request: a page size above the maximum is SERVED, with the page holding as many
//! toggles as the caller asked for, and one below the minimum is the operation's own
//! modelled `InvalidRequestException` (HTTP 400). An instance this account does not hold
//! is `ResourceNotFoundException` (404), never a refusal of the request — which is what
//! lets a caller tell "this instance has no attributes" from "the request was bad".
//!
//! RULE (the attribute type): a value that names no toggle of an instance — neither a
//! member of the modelled `InstanceAttributeType` enum nor one of the types the service
//! reports beyond it — is a value this service cannot read, so every operation of this
//! plane refuses it with its own modelled `InvalidRequestException` (HTTP 400) whose
//! `Message` is `Invalid AttributeType` — fixed text, which quotes neither the rejected
//! value nor the set of values the member admits — rather than with the request boundary's
//! generic enum-validation prose under an error shape no operation of this service models.
//!
//! RULE (list then use): every attribute type the listing serves reads back through
//! `DescribeInstanceAttribute`, including the types AWS reports that the model's enum does
//! not declare, so a client that enumerates an instance's features can then read one.

use serde_json::Value;
use service_com_amazonaws_connect_amazonconnectservice::ops::types::INSTANCE_ATTRIBUTE_TYPE_VALUES;
use service_com_amazonaws_connect_amazonconnectservice::state::INSTANCE_ATTRIBUTE_TYPES_BEYOND_THE_MODEL;

use super::harness::{connect_managed, query, Answer, Connect};

/// The sentence Connect answers an `AttributeType` outside the modelled enum with: the
/// member is named, the rejected value and the permitted set are not. Written out here
/// rather than read off the implementation so the suite pins the text itself.
const INVALID_ATTRIBUTE_TYPE: &str = "Invalid AttributeType";

/// The refusal a page size below the modelled minimum takes. Its variable parts are the
/// member's own name and the violated `@range` bound, both written out as the model
/// spells them.
const MAX_RESULTS_BELOW_MINIMUM: &str =
    "Invalid MaxResults. MaxResults should have minimum value of 1.";

/// This service's one not-found sentence for a resource inside an instance.
const RESOURCE_NOT_FOUND: &str = "Resource not found";

/// What it answers a request whose `InstanceId` label carries no identifier at all with.
const INVALID_REQUEST: &str = "Invalid request";

/// What it answers an `InstanceId` longer than that identifier's modelled maximum with.
const INSTANCE_ID_NOT_VALID: &str = "Instance id is not valid";

/// The modelled `@range` of `com.amazonaws.connect#MaxResult7`, the shape
/// `ListInstanceAttributes`'s `MaxResults` targets
/// (`smithy_query(query="operation",operation="ListInstanceAttributes")` -> range min 1
/// max 7), written out here so the table states the bounds rather than reading them off
/// the implementation.
const MAX_RESULTS_MIN: i32 = 1;
/// See [`MAX_RESULTS_MIN`].
const MAX_RESULTS_MAX: i32 = 7;

/// The modelled `@length` maximum of `com.amazonaws.connect#InstanceId`
/// (`smithy_query(query="shape",shape="InstanceId")` -> length min 1 max 100).
const INSTANCE_ID_MAX_LENGTH: usize = 100;

/// The two values a feature toggle takes.
const ENABLED: &str = "true";
/// See [`ENABLED`].
const DISABLED: &str = "false";

impl Connect {
    /// `ListInstanceAttributes`: `GET /instance/{InstanceId}/attributes[?maxResults=…][&nextToken=…]`.
    async fn attribute_page(
        &self,
        instance_id: &str,
        max_results: Option<i32>,
        token: Option<&str>,
    ) -> Answer {
        let params = query(max_results, token, "", &[]);
        self.send_answer(
            "GET",
            &format!("/instance/{instance_id}/attributes{params}"),
            None,
        )
        .await
    }

    /// `DescribeInstanceAttribute`: `GET /instance/{InstanceId}/attribute/{AttributeType}`.
    async fn read_attribute(&self, instance_id: &str, attribute_type: &str) -> Answer {
        self.send_answer(
            "GET",
            &format!("/instance/{instance_id}/attribute/{attribute_type}"),
            None,
        )
        .await
    }

    /// `UpdateInstanceAttribute`: `POST /instance/{InstanceId}/attribute/{AttributeType}`.
    async fn write_attribute(
        &self,
        instance_id: &str,
        attribute_type: &str,
        value: &str,
    ) -> Answer {
        self.send_answer(
            "POST",
            &format!("/instance/{instance_id}/attribute/{attribute_type}"),
            Some(serde_json::json!({ "Value": value })),
        )
        .await
    }
}

/// The `Message` member of an answered error.
fn message_of(body: &Value) -> String {
    body["Message"].as_str().unwrap_or_default().to_string()
}

/// One page's `(AttributeType, Value)` pairs, asserting the page is a serialized list of
/// the modelled `Attribute` shape.
fn pairs_of(case: &str, body: &Value) -> Vec<(String, String)> {
    let listed = body["Attributes"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected Attributes to be serialized, got {body}"));
    listed
        .iter()
        .map(|attribute| {
            let attribute_type = attribute["AttributeType"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: expected an AttributeType, got {attribute}")
            });
            let value = attribute["Value"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a Value, got {attribute}"));
            (attribute_type.to_string(), value.to_string())
        })
        .collect()
}

/// Walk every page of one instance's attributes at `max_results`, asserting as it goes
/// that each page is served, is no larger than the caller asked for, and carries a
/// `NextToken` exactly while toggles remain.
async fn walk(connect: &Connect, case: &str, instance_id: &str, max_results: Option<i32>) -> Vec<(String, String)> {
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let answer = connect
            .attribute_page(instance_id, max_results, token.as_deref())
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from ListInstanceAttributes, got {} {}",
            answer.status, answer.body
        );
        pages += 1;
        let page = pairs_of(case, &answer.body);
        if let Some(asked) = max_results {
            assert!(
                page.len() <= asked as usize,
                "case {case:?}: a page holds at most the {asked} the caller asked for, got {} \
                 ({page:?})",
                page.len()
            );
        }
        seen.extend(page);
        match answer.body["NextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
        assert!(
            pages < 64,
            "case {case:?}: the walk did not terminate, got {pages} pages"
        );
    }
    seen
}

/// Every attribute type the service reports for an instance, in the ascending order a
/// page is cut on: the modelled enum's members together with the types AWS reports beyond
/// it (`INSTANCE_ATTRIBUTE_TYPES_BEYOND_THE_MODEL`).
fn reported_types_in_order() -> Vec<String> {
    let mut types: Vec<String> = INSTANCE_ATTRIBUTE_TYPE_VALUES
        .iter()
        .chain(INSTANCE_ATTRIBUTE_TYPES_BEYOND_THE_MODEL.iter())
        .map(|name| name.to_string())
        .collect();
    types.sort();
    types
}

/// RULE: the listing reports every attribute type the instance holds a value for, in
/// ascending order, with the same value the single-attribute read reports — whatever page
/// size the caller enumerates with.
///
/// The rows differ along the page size: none at all, both published bounds of the modelled
/// `@range`, inside it, one past the maximum, the whole population, far past it, and the
/// largest value the member's integer type carries.
#[tokio::test]
async fn the_listing_covers_every_toggle_at_every_page_size() {
    let connect = Connect::new();
    let (instance_id, _) = connect
        .create_instance(connect_managed("attributelisting"))
        .await;
    let expected_types = reported_types_in_order();

    let rows: Vec<Option<i32>> = vec![
        None,
        Some(MAX_RESULTS_MIN),
        Some(3),
        Some(MAX_RESULTS_MAX),
        Some(MAX_RESULTS_MAX + 1),
        Some(expected_types.len() as i32),
        Some(1_000),
        Some(i32::MAX),
    ];

    for max_results in rows {
        let case = format!("maxResults={max_results:?}");
        let seen = walk(&connect, &case, &instance_id, max_results).await;

        let listed_types: Vec<String> = seen.iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(
            listed_types, expected_types,
            "case {case:?}: expected every reported attribute type in ascending order, got \
             {listed_types:?}"
        );

        for (attribute_type, value) in &seen {
            let read = connect.read_attribute(&instance_id, attribute_type).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}/{attribute_type}: expected HTTP 200 from \
                 DescribeInstanceAttribute, got {} {}",
                read.status, read.body
            );
            let expected = read.body["Attribute"]["Value"].as_str().unwrap_or_default();
            assert_eq!(
                value, expected,
                "case {case:?}/{attribute_type}: expected the listing to report the value the \
                 single read reports ({expected:?}), got {value:?}"
            );
        }
    }
}

/// RULE (the maximum end): a page size above the member's modelled maximum is SERVED, and
/// the page holds as many toggles as the caller asked for — the maximum bounds neither the
/// request nor the page.
///
/// The rows differ along how far past the maximum the request asks, against a population
/// larger than that maximum so a page that had been bounded by it would be visible.
#[tokio::test]
async fn a_page_size_above_the_modelled_maximum_is_served_in_full() {
    let connect = Connect::new();
    let (instance_id, _) = connect
        .create_instance(connect_managed("attributebigpage"))
        .await;
    let total = reported_types_in_order().len();
    assert!(
        total > MAX_RESULTS_MAX as usize,
        "the population has to exceed the modelled maximum for this rule to be observable, \
         got {total}"
    );

    for asked in [
        MAX_RESULTS_MAX + 1,
        MAX_RESULTS_MAX + 2,
        total as i32,
        1_000,
        i32::MAX,
    ] {
        let case = format!("maxResults={asked}");
        let answer = connect
            .attribute_page(&instance_id, Some(asked), None)
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} {}",
            answer.status, answer.body
        );
        let page = pairs_of(&case, &answer.body);
        let expected = (asked as usize).min(total);
        assert_eq!(
            page.len(),
            expected,
            "case {case:?}: expected a first page of {expected} toggles, got {} ({page:?})",
            page.len()
        );
        let more_remain = expected < total;
        assert_eq!(
            answer.body["NextToken"].as_str().is_some(),
            more_remain,
            "case {case:?}: expected NextToken present={more_remain}, got {}",
            answer.body
        );
    }
}

/// RULE (the minimum end): a page size below the modelled minimum is refused with the
/// operation's own modelled `InvalidRequestException`, whose message names the member and
/// that minimum and quotes no value.
///
/// The rows are the values below the bound: zero, negative, and the most negative value
/// the member's integer type carries.
#[tokio::test]
async fn a_page_size_below_the_modelled_minimum_is_refused() {
    let connect = Connect::new();
    let (instance_id, _) = connect
        .create_instance(connect_managed("attributesmallpage"))
        .await;

    for asked in [MAX_RESULTS_MIN - 1, -7, i32::MIN] {
        let case = format!("maxResults={asked}");
        let answer = connect
            .attribute_page(&instance_id, Some(asked), None)
            .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code, "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {:?} {}",
            answer.error_code, answer.body
        );
        let actual = message_of(&answer.body);
        assert_eq!(
            actual, MAX_RESULTS_BELOW_MINIMUM,
            "case {case:?}: expected message {MAX_RESULTS_BELOW_MINIMUM:?}, got {actual:?}"
        );
        assert!(
            !answer.body.to_string().contains(&asked.to_string()),
            "case {case:?}: the refusal must not quote the rejected value, got {}",
            answer.body
        );
    }
}

/// RULE (round trip): an update is what the listing then reports, and listing stores
/// nothing — the same walk run twice reports the same page.
///
/// The rows differ along the attribute type written and the value written to it, including
/// the two call directions, which are the instance's own flags rather than a toggle beside
/// them.
#[tokio::test]
async fn the_listing_reports_what_an_update_stored() {
    let connect = Connect::new();
    let (instance_id, _) = connect
        .create_instance(connect_managed("attributelistingwrites"))
        .await;

    for (attribute_type, value) in [
        ("INBOUND_CALLS", DISABLED),
        ("OUTBOUND_CALLS", DISABLED),
        ("CONTACT_LENS", DISABLED),
        ("CONTACTFLOW_LOGS", ENABLED),
        // A type the service reports that the model's enum does not declare: the listing
        // serves it, so an update of it is stored and reported like any other toggle.
        ("BOT_MANAGEMENT", ENABLED),
        ("MESSAGE_STREAMING", ENABLED),
        ("MESSAGE_STREAMING", DISABLED),
    ] {
        let case = format!("{attribute_type}={value}");
        let written = connect
            .write_attribute(&instance_id, attribute_type, value)
            .await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: expected HTTP 200 from UpdateInstanceAttribute, got {} {}",
            written.status, written.body
        );

        let first = walk(&connect, &case, &instance_id, None).await;
        let listed = first
            .iter()
            .find(|(name, _)| name == attribute_type)
            .unwrap_or_else(|| panic!("case {case:?}: {attribute_type} is not listed: {first:?}"));
        assert_eq!(
            listed.1, value,
            "case {case:?}: expected the listing to report {value:?}, got {:?}",
            listed.1
        );

        // Reading the set changes nothing a later caller sees.
        let second = walk(&connect, &case, &instance_id, None).await;
        assert_eq!(
            second, first,
            "case {case:?}: listing the toggles must not change what a later caller sees"
        );
    }
}

/// RULE: there is nothing to enumerate on an instance this account does not hold, and a
/// request the service cannot read is refused as such rather than as a missing resource —
/// so a client can always tell the two apart.
///
/// The rows differ along WHY the request cannot be served: an identifier that holds
/// nothing, one a delete retired, a label carrying no identifier, a label longer than the
/// identifier's modelled maximum, and a cursor this service never issued.
#[tokio::test]
async fn a_listing_the_service_cannot_serve_is_refused_by_its_class() {
    let connect = Connect::new();
    let (live_id, _) = connect
        .create_instance(connect_managed("attributelistingneighbour"))
        .await;
    let (retired_id, _) = connect
        .create_instance(connect_managed("attributelistingretired"))
        .await;
    let deleted = connect
        .send_answer("DELETE", &format!("/instance/{retired_id}"), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "DeleteInstance of a live instance: got {} {}",
        deleted.status, deleted.body
    );

    // One row: the label the request carries, the cursor it sends, and the answer the
    // service's own modelled error channel gives it.
    struct Row {
        case: &'static str,
        label: String,
        token: Option<&'static str>,
        status: u16,
        code: &'static str,
        message: String,
    }

    let rows: Vec<Row> = vec![
        Row {
            case: "an identifier of the minted form that holds nothing",
            label: "7f3c9b21-0d4e-4a86-9c15-2b6f8e5d1a30".to_string(),
            token: None,
            status: 404,
            code: "ResourceNotFoundException",
            message: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            case: "a name, not an identifier",
            label: "support".to_string(),
            token: None,
            status: 404,
            code: "ResourceNotFoundException",
            message: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            case: "an identifier a delete retired",
            label: retired_id.clone(),
            token: None,
            status: 404,
            code: "ResourceNotFoundException",
            message: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            case: "a label carrying no identifier",
            label: "%20".to_string(),
            token: None,
            status: 400,
            code: "InvalidRequestException",
            message: INVALID_REQUEST.to_string(),
        },
        Row {
            case: "a label one past the identifier's modelled maximum",
            label: "i".repeat(INSTANCE_ID_MAX_LENGTH + 1),
            token: None,
            status: 400,
            code: "InvalidRequestException",
            message: INSTANCE_ID_NOT_VALID.to_string(),
        },
        Row {
            case: "a cursor this service never issued",
            label: live_id.clone(),
            token: Some("not-a-cursor"),
            status: 400,
            code: "InvalidRequestException",
            message: "NextToken is not a token this service issued".to_string(),
        },
    ];

    for Row {
        case,
        label,
        token,
        status,
        code,
        message,
    } in rows
    {
        let answer = connect.attribute_page(&label, None, token).await;
        assert_eq!(
            answer.status, status,
            "case {case:?}: expected HTTP {status}, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code, code,
            "case {case:?}: expected error code {code}, got {:?} {}",
            answer.error_code, answer.body
        );
        let actual = message_of(&answer.body);
        assert_eq!(
            actual, message,
            "case {case:?}: expected message {message:?}, got {actual:?}"
        );
        assert!(
            answer.body["Attributes"].is_null(),
            "case {case:?}: a refusal carries no page, got {}",
            answer.body
        );
    }

    // The live instance is untouched by every row above: its toggles still list.
    let served = walk(&connect, "the live instance", &live_id, None).await;
    assert_eq!(
        served.len(),
        reported_types_in_order().len(),
        "the live instance still lists every toggle, got {served:?}"
    );
}

/// RULE: a value that names no toggle of an instance is a value this service cannot read,
/// so every operation of this plane refuses it with `InvalidRequestException` (HTTP 400)
/// and the fixed `Invalid AttributeType` — and stores nothing.
///
/// The rows differ along the FORM of the value: a name nothing reports, a reported name in
/// the wrong case, one with something appended or prepended, one with whitespace in or
/// around it, one carrying the separator alone, and one far longer than any member. Every
/// type the instance reports stands beside them as the accepted case, so no row can pass
/// on a check that refuses everything.
#[tokio::test]
async fn an_attribute_type_that_names_no_toggle_is_refused_and_stores_nothing() {
    let connect = Connect::new();
    let (instance_id, _) = connect
        .create_instance(connect_managed("attributebadtype"))
        .await;
    let before = walk(&connect, "before the refusals", &instance_id, None).await;

    // (case, the label as the request carries it, the label after decoding)
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a name nothing reports",
            "NOT_AN_ATTRIBUTE".to_string(),
            "NOT_AN_ATTRIBUTE".to_string(),
        ),
        (
            "another name nothing reports",
            "SCREEN_SHARING".to_string(),
            "SCREEN_SHARING".to_string(),
        ),
        (
            "a reported name in lower case",
            "inbound_calls".to_string(),
            "inbound_calls".to_string(),
        ),
        (
            "a reported name in mixed case",
            "Contact_Lens".to_string(),
            "Contact_Lens".to_string(),
        ),
        (
            "a reported name with a character appended",
            "CONTACT_LENSX".to_string(),
            "CONTACT_LENSX".to_string(),
        ),
        (
            "a reported name with a character prepended",
            "XEARLY_MEDIA".to_string(),
            "XEARLY_MEDIA".to_string(),
        ),
        (
            "a reported name with a trailing space",
            "EARLY_MEDIA%20".to_string(),
            "EARLY_MEDIA ".to_string(),
        ),
        (
            "a reported name with its separator as a space",
            "EARLY%20MEDIA".to_string(),
            "EARLY MEDIA".to_string(),
        ),
        ("the separator alone", "_".to_string(), "_".to_string()),
        (
            "far longer than any member",
            "A".repeat(300),
            "A".repeat(300),
        ),
    ];

    for (case, sent, label) in rows {
        let answers = [
            (
                "DescribeInstanceAttribute",
                connect.read_attribute(&instance_id, &sent).await,
            ),
            (
                "UpdateInstanceAttribute",
                connect.write_attribute(&instance_id, &sent, ENABLED).await,
            ),
        ];
        for (operation, answer) in answers {
            assert_eq!(
                answer.status, 400,
                "case {case:?}/{operation}: expected HTTP 400, got {} {}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code, "InvalidRequestException",
                "case {case:?}/{operation}: expected InvalidRequestException, got {:?} {}",
                answer.error_code, answer.body
            );
            let actual = message_of(&answer.body);
            assert_eq!(
                actual, INVALID_ATTRIBUTE_TYPE,
                "case {case:?}/{operation}: expected message {INVALID_ATTRIBUTE_TYPE:?}, got \
                 {actual:?}"
            );
            // The sentence is fixed: neither the rejected value nor the set of values the
            // member admits appears in the answer. Only a value long enough not to occur
            // in the envelope by accident is checked — a one-character value is a
            // character of the envelope itself.
            let rendered = answer.body.to_string();
            if label.trim().len() >= 4 {
                assert!(
                    !rendered.contains(label.trim()),
                    "case {case:?}/{operation}: the refusal must not quote the rejected value, \
                     got {rendered}"
                );
            }
            for member in INSTANCE_ATTRIBUTE_TYPE_VALUES
                .iter()
                .chain(INSTANCE_ATTRIBUTE_TYPES_BEYOND_THE_MODEL.iter())
            {
                assert!(
                    !rendered.contains(member),
                    "case {case:?}/{operation}: the refusal must not quote the permitted set \
                     ({member}), got {rendered}"
                );
            }
        }
    }

    // RULE (list then use): every type the listing serves reads back, including the ones
    // the model's enum does not declare — and nothing above stored anything.
    for (attribute_type, _) in &before {
        let answer = connect.read_attribute(&instance_id, attribute_type).await;
        assert_eq!(
            answer.status, 200,
            "case {attribute_type}: expected HTTP 200 for a type the listing serves, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.body["Attribute"]["AttributeType"].as_str(),
            Some(attribute_type.as_str()),
            "case {attribute_type}: expected the answer to carry the type asked about, got {}",
            answer.body
        );
    }
    let after = walk(&connect, "after the refusals", &instance_id, None).await;
    assert_eq!(
        after, before,
        "the refusals must leave the instance's toggles exactly as they were"
    );
}
