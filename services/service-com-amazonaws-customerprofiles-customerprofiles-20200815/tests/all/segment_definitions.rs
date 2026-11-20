// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `CreateSegmentDefinition` and the reader that serves what it
//! stored (`GetSegmentDefinition`).
//!
//! The rule under test: a segment definition is a resource of its domain, identified by
//! the (domain, `SegmentDefinitionName`) pair, and it is created from EXACTLY ONE kind of
//! criteria — the segment-group structure (a classic segment) or a SQL query (an enhanced
//! one). A create carrying both, or neither, is refused `BadRequestException` (400);
//! otherwise it persists the criteria and labels the request carried, mints the
//! definition's ARN once, and answers from the record it stored, so the reader reports the
//! same definition with the `SegmentType` its criteria imply. A name the domain already
//! holds and a tag key outside the bounds `TagKey` declares are each refused
//! `BadRequestException` (400) with nothing persisted; a domain, or a definition in it,
//! that is not stored is `ResourceNotFoundException` (404).
//!
//! Every case goes through the real router, so what is asserted is the wire behaviour a
//! client sees. The HTTP plumbing and the create helpers are this crate's one test
//! harness, in `tests/all/domain_profile_lifecycle.rs`.

use serde_json::{json, Value};

use service_com_amazonaws_customerprofiles_customerprofiles_20200815::state::{
    ARN_SERVICE, SEGMENT_TYPE_CLASSIC, SEGMENT_TYPE_ENHANCED,
};

use crate::domain_profile_lifecycle::{error_code, Emulator};
use crate::profile_object_types::error_message;

/// The region the test harness signs for, which is the region a minted ARN carries.
const REGION: &str = "us-east-1";

/// `CreateSegmentDefinition` over the wire:
/// `POST /domains/{DomainName}/segment-definitions/{SegmentDefinitionName}`.
async fn create(emulator: &Emulator, domain: &str, name: &str, body: Value) -> (u16, Value) {
    emulator
        .post(&format!("/domains/{domain}/segment-definitions/{name}"), body)
        .await
}

/// `GetSegmentDefinition` over the wire:
/// `GET /domains/{DomainName}/segment-definitions/{SegmentDefinitionName}`.
async fn get(emulator: &Emulator, domain: &str, name: &str) -> (u16, Value) {
    emulator
        .send(
            "GET",
            &format!("/domains/{domain}/segment-definitions/{name}"),
            None,
        )
        .await
}

/// The ARN a definition of `domain` named `name` is minted with: this service's ARN
/// namespace and the resource path the definition's own operations are bound to.
fn expected_arn(domain: &str, name: &str) -> String {
    engine::emulator_arn(
        ARN_SERVICE,
        REGION,
        &format!("domains/{domain}/segment-definitions/{name}"),
    )
}

/// One `ProfileDimension` on a profile attribute: a `StringDimensionType` out of the
/// modelled enum (INCLUSIVE | EXCLUSIVE | CONTAINS | BEGINS_WITH | ENDS_WITH) and the
/// values it applies to.
fn dimension(dimension_type: &str, values: &[&str]) -> Value {
    json!({ "DimensionType": dimension_type, "Values": values })
}

/// One `Group` of a segment group, with every member the model defaults stated, so what a
/// read reports back is exactly what the request carried.
fn group(source_type: &str, group_type: &str, body: Value) -> Value {
    let mut group = json!({ "SourceType": source_type, "Type": group_type });
    for (member, value) in body.as_object().expect("an object group body") {
        group[member] = value.clone();
    }
    group
}

/// `SegmentGroups` holding `groups`, with the `Include` option stated.
fn segment_groups(include: &str, groups: Vec<Value>) -> Value {
    json!({ "Include": include, "Groups": groups })
}

/// `Tags` as a segment-definition projection of a definition created by `request` must
/// report it: the map the create carried, and the EMPTY map for a create that carried
/// none — the member is reported either way.
fn expected_tags(request: &Value) -> Value {
    request.get("Tags").cloned().unwrap_or_else(|| json!({}))
}

/// The sentence a create that names NEITHER segment source is refused with: the service's
/// fixed text for a missing source, qualified by the domain and the segment-definition
/// name the request addressed.
fn no_source_refusal(domain: &str, name: &str) -> String {
    format!(
        "Must specify either SegmentSqlQuery or SegmentGroups. Domain: {domain}, \
         SegmentDefinitionName: {name}"
    )
}

/// Every member of `expected` as `actual` reports it.
fn assert_members(case: &str, actual: &Value, expected: &[(&str, Value)]) {
    for (member, value) in expected {
        assert_eq!(
            actual.get(member),
            Some(value),
            "case {case:?}: expected {member} = {value:?}, got {:?}",
            actual.get(member)
        );
    }
}

/// RULE: a create persists the criteria and labels the request carried, mints the ARN
/// once, and answers from the record it stored, so the reader reports the same definition
/// — with `SegmentType` following from WHICH criteria it was created with — and reading it
/// changes nothing.
#[tokio::test]
async fn a_created_segment_definition_reads_back_as_it_was_stored() {
    let emulator = Emulator::new();
    emulator.create_domain("segdom", 90).await;

    // The rows differ along what the rule generalises over: the kind of criteria, the
    // number of groups and dimensions, the dimension type and value count, the sort, and
    // which optional labels the request carries.
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            "a classic segment on one profile attribute",
            "Loyal_Customers",
            json!({
                "DisplayName": "Loyal customers",
                "SegmentGroups": segment_groups(
                    "ALL",
                    vec![group(
                        "ALL",
                        "ALL",
                        json!({ "Dimensions": [
                            { "ProfileAttributes": { "FirstName": dimension("INCLUSIVE", &["Jane"]) } },
                        ] }),
                    )],
                ),
            }),
            SEGMENT_TYPE_CLASSIC,
        ),
        (
            "a classic segment on two groups, with a description and tags",
            "Churn-Risk",
            json!({
                "DisplayName": "Churn risk",
                "Description": "profiles that stopped ordering",
                "SegmentGroups": segment_groups(
                    "ANY",
                    vec![
                        group(
                            "ANY",
                            "NONE",
                            json!({ "Dimensions": [
                                { "ProfileAttributes": {
                                    "LastName": dimension("BEGINS_WITH", &["A", "B", "C"]),
                                    "EmailAddress": dimension("ENDS_WITH", &["@example.com"]),
                                } },
                                { "ProfileAttributes": {
                                    "Attributes": { "tier": dimension("EXCLUSIVE", &["gold"]) },
                                } },
                            ] }),
                        ),
                        group(
                            "NONE",
                            "ALL",
                            json!({ "Dimensions": [
                                { "ProfileAttributes": {
                                    "BillingAddress": { "City": dimension("CONTAINS", &["Berlin"]) },
                                } },
                            ] }),
                        ),
                    ],
                ),
                "Tags": { "team": "crm", "cost-center": "42" },
            }),
            SEGMENT_TYPE_CLASSIC,
        ),
        (
            "a classic segment built off a source segment",
            "Loyal_Customers_In_Berlin",
            json!({
                "DisplayName": "Loyal customers in Berlin",
                "SegmentGroups": segment_groups(
                    "ALL",
                    vec![group(
                        "ANY",
                        "ALL",
                        json!({
                            "SourceSegments": [{ "SegmentDefinitionName": "Loyal_Customers" }],
                            "Dimensions": [
                                { "ProfileAttributes": {
                                    "Address": { "City": dimension("INCLUSIVE", &["Berlin"]) },
                                } },
                            ],
                        }),
                    )],
                ),
            }),
            SEGMENT_TYPE_CLASSIC,
        ),
        (
            "an enhanced segment from a SQL query, sorted",
            "High_Spenders",
            json!({
                "DisplayName": "High spenders",
                "Description": "the profiles the warehouse query selects",
                "SegmentSqlQuery": "select profile_id from profiles where lifetime_spend > 1000",
                "SegmentSort": { "Attributes": [
                    { "Name": "LastName", "Order": "ASC", "Type": "PROFILE" },
                    {
                        "Name": "_lifetime.spend",
                        "DataType": "NUMBER",
                        "Order": "DESC",
                        "Type": "CALCULATED",
                    },
                ] },
                "Tags": { "team": "finance" },
            }),
            SEGMENT_TYPE_ENHANCED,
        ),
        (
            "an enhanced segment carrying only the members a create requires",
            "Everyone",
            json!({
                "DisplayName": "Everyone",
                "SegmentSqlQuery": "select profile_id from profiles",
            }),
            SEGMENT_TYPE_ENHANCED,
        ),
    ];

    for (case, name, request, segment_type) in &rows {
        let (status, created) = create(&emulator, "segdom", name, request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateSegmentDefinition 200, got {status} {created:?}"
        );

        // The create answers from the record it stored: the name it was addressed by, the
        // ARN it minted, and the labels the request carried.
        assert_eq!(
            created.get("SegmentDefinitionName").and_then(Value::as_str),
            Some(*name),
            "case {case:?}: expected SegmentDefinitionName {name:?}, got {:?}",
            created.get("SegmentDefinitionName")
        );
        let arn = expected_arn("segdom", name);
        assert_eq!(
            created.get("SegmentDefinitionArn").and_then(Value::as_str),
            Some(arn.as_str()),
            "case {case:?}: expected SegmentDefinitionArn {arn:?}, got {:?}",
            created.get("SegmentDefinitionArn")
        );
        for member in ["DisplayName", "Description"] {
            assert_eq!(
                created.get(member),
                request.get(member),
                "case {case:?}: expected the {member} the request carried ({:?}), got {:?}",
                request.get(member),
                created.get(member)
            );
        }
        assert_eq!(
            created.get("Tags"),
            Some(&expected_tags(request)),
            "case {case:?}: expected the create to report Tags {:?}, got {:?}",
            expected_tags(request),
            created.get("Tags")
        );
        let created_at = created.get("CreatedAt").cloned().unwrap_or(Value::Null);
        assert!(
            created_at.is_number(),
            "case {case:?}: expected a CreatedAt timestamp, got {created_at:?}"
        );

        // The reader serves that same record, criteria included, and resolves the segment
        // type from them.
        let (status, read) = get(&emulator, "segdom", name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetSegmentDefinition 200, got {status} {read:?}"
        );
        assert_members(
            case,
            &read,
            &[
                ("SegmentDefinitionName", json!(name)),
                ("SegmentDefinitionArn", json!(arn)),
                ("SegmentType", json!(segment_type)),
                ("CreatedAt", created_at.clone()),
            ],
        );
        for member in [
            "DisplayName",
            "Description",
            "SegmentGroups",
            "SegmentSqlQuery",
            "SegmentSort",
        ] {
            assert_eq!(
                read.get(member),
                request.get(member),
                "case {case:?}: expected the stored {member} ({:?}), got {:?}",
                request.get(member),
                read.get(member)
            );
        }
        assert_eq!(
            read.get("Tags"),
            Some(&expected_tags(request)),
            "case {case:?}: expected the read to report Tags {:?}, got {:?}",
            expected_tags(request),
            read.get("Tags")
        );

        // A read reports, so a second one answers exactly the same.
        let (status, again) = get(&emulator, "segdom", name).await;
        assert_eq!(
            (status, &again),
            (200, &read),
            "case {case:?}: a read must not change what a later caller sees"
        );
    }
}

/// RULE: a definition is created from exactly one kind of criteria, so a request carrying
/// both the segment-group structure and a SQL query, or neither, is refused
/// `BadRequestException` (400) and nothing is persisted. A request naming NO source is
/// refused with the service's sentence for it, qualified by the domain and the segment
/// definition the request addressed.
#[tokio::test]
async fn a_segment_definition_is_created_from_exactly_one_kind_of_criteria() {
    let emulator = Emulator::new();
    emulator.create_domain("critdom", 90).await;

    let sql = "select profile_id from profiles";
    let groups = segment_groups(
        "ALL",
        vec![group(
            "ALL",
            "ALL",
            json!({ "Dimensions": [
                { "ProfileAttributes": { "FirstName": dimension("INCLUSIVE", &["Jane"]) } },
            ] }),
        )],
    );
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "neither kind of criteria",
            "No_Criteria",
            json!({ "DisplayName": "no criteria" }),
            no_source_refusal("critdom", "No_Criteria"),
        ),
        (
            "neither kind of criteria, but a sort",
            "Sort_Only",
            json!({
                "DisplayName": "sort only",
                "SegmentSort": { "Attributes": [
                    { "Name": "LastName", "Order": "ASC", "Type": "PROFILE" },
                ] },
            }),
            no_source_refusal("critdom", "Sort_Only"),
        ),
        (
            "neither kind of criteria, but tags",
            "Tagged-Only",
            json!({ "DisplayName": "tagged only", "Tags": { "team": "crm" } }),
            no_source_refusal("critdom", "Tagged-Only"),
        ),
        (
            "both kinds of criteria",
            "Both_Criteria",
            json!({
                "DisplayName": "both",
                "SegmentGroups": groups.clone(),
                "SegmentSqlQuery": sql,
            }),
            "SegmentGroups and SegmentSqlQuery cannot both be provided".to_string(),
        ),
    ];

    for (case, name, request, expected) in &rows {
        let (status, refused) = create(&emulator, "critdom", name, request.clone()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {refused:?}"
        );
        assert_eq!(
            error_message(&refused),
            *expected,
            "case {case:?}: expected the sentence {expected:?}, got {:?}",
            error_message(&refused)
        );

        // Nothing was persisted, so the reader holds no such definition.
        let (status, read) = get(&emulator, "critdom", name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the refused create to persist nothing, got {status} \
             {read:?}"
        );
    }

    // A tag key outside the bounds `TagKey` declares is refused the same way: the key
    // reaches a handler unchecked, and `aws:` is reserved by the pattern's lookahead.
    let (status, refused) = create(
        &emulator,
        "critdom",
        "Reserved_Tag",
        json!({
            "DisplayName": "reserved tag",
            "SegmentSqlQuery": sql,
            "Tags": { "aws:owner": "crm" },
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a reserved tag key\": expected HTTP 400, got {status} {refused:?}"
    );
    assert_eq!(
        error_code(&refused),
        "BadRequestException",
        "case \"a reserved tag key\": expected BadRequestException, got {refused:?}"
    );
}

/// RULE: the (domain, name) pair identifies one definition, so a create of a name the
/// domain already holds is refused `BadRequestException` (400) and leaves the stored
/// definition untouched, while the same name in another domain is a different definition
/// with its own criteria and its own ARN.
#[tokio::test]
async fn a_segment_definition_name_identifies_one_definition_in_its_domain() {
    let emulator = Emulator::new();
    emulator.create_domain("first", 90).await;
    emulator.create_domain("second", 90).await;

    let rows: Vec<(&str, &str, Value, Value)> = vec![
        (
            "a classic winner, an enhanced loser",
            "Shared_Name",
            json!({
                "DisplayName": "the winner",
                "SegmentGroups": segment_groups(
                    "ALL",
                    vec![group(
                        "ALL",
                        "ALL",
                        json!({ "Dimensions": [
                            { "ProfileAttributes": { "FirstName": dimension("INCLUSIVE", &["Jane"]) } },
                        ] }),
                    )],
                ),
            }),
            json!({
                "DisplayName": "the loser",
                "Description": "the loser's description",
                "SegmentSqlQuery": "select profile_id from profiles",
                "Tags": { "team": "crm" },
            }),
        ),
        (
            "an enhanced winner, a classic loser",
            "Another-Shared",
            json!({
                "DisplayName": "the winner",
                "SegmentSqlQuery": "select profile_id from profiles where lifetime_spend > 10",
            }),
            json!({
                "DisplayName": "the loser",
                "SegmentGroups": segment_groups("ANY", vec![group("ALL", "ALL", json!({}))]),
            }),
        ),
    ];

    for (case, name, winner, loser) in &rows {
        let (status, created) = create(&emulator, "first", name, winner.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the first create to win, got {status} {created:?}"
        );

        let (status, refused) = create(&emulator, "first", name, loser.clone()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a name the domain holds, got {status} \
             {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {refused:?}"
        );
        let expected = format!("Segment definition with name {name} already exists");
        assert_eq!(
            error_message(&refused),
            expected,
            "case {case:?}: expected the already-exists sentence {expected:?}, got {:?}",
            error_message(&refused)
        );

        // The refused create changed nothing: the read still reports the winner, and none
        // of the loser's members reached the record.
        let (status, read) = get(&emulator, "first", name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the winner still readable, got {status} {read:?}"
        );
        for (member, expected) in winner.as_object().expect("an object request") {
            assert_eq!(
                read.get(member),
                Some(expected),
                "case {case:?}: the refused create must leave {member} = {expected:?}, got {:?}",
                read.get(member)
            );
        }
        for member in ["Description", "SegmentGroups", "SegmentSqlQuery"] {
            if winner.get(member).is_none() {
                assert_eq!(
                    read.get(member),
                    None,
                    "case {case:?}: the refused create must not add {member}, got {:?}",
                    read.get(member)
                );
            }
        }
        assert_eq!(
            read.get("Tags"),
            Some(&expected_tags(winner)),
            "case {case:?}: the refused create must leave Tags {:?}, got {:?}",
            expected_tags(winner),
            read.get("Tags")
        );

        // The identity is the PAIR, so the same name in another domain is a different
        // definition, with its own criteria and its own ARN.
        let (status, elsewhere) = create(&emulator, "second", name, loser.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the same name in another domain to be created, got \
             {status} {elsewhere:?}"
        );
        let arn = expected_arn("second", name);
        assert_eq!(
            elsewhere
                .get("SegmentDefinitionArn")
                .and_then(Value::as_str),
            Some(arn.as_str()),
            "case {case:?}: expected SegmentDefinitionArn {arn:?}, got {:?}",
            elsewhere.get("SegmentDefinitionArn")
        );
        let (_, read_elsewhere) = get(&emulator, "second", name).await;
        for (member, expected) in loser.as_object().expect("an object request") {
            assert_eq!(
                read_elsewhere.get(member),
                Some(expected),
                "case {case:?}: expected the other domain's {member} = {expected:?}, got {:?}",
                read_elsewhere.get(member)
            );
        }
    }
}

/// RULE: a reference to a resource that is not stored is `ResourceNotFoundException`
/// (404), worded for the resource that is missing — the domain, or the definition in it.
#[tokio::test]
async fn an_absent_domain_or_definition_is_not_found() {
    let emulator = Emulator::new();
    emulator.create_domain("here", 90).await;

    let request = json!({
        "DisplayName": "anything",
        "SegmentSqlQuery": "select profile_id from profiles",
    });

    for (case, domain) in [
        ("a domain that was never created", "nowhere"),
        ("a domain whose name differs in case", "Here"),
    ] {
        for (operation, (status, refused)) in [
            (
                "CreateSegmentDefinition",
                create(&emulator, domain, "Any_Name", request.clone()).await,
            ),
            (
                "GetSegmentDefinition",
                get(&emulator, domain, "Any_Name").await,
            ),
        ] {
            assert_eq!(
                status, 404,
                "case {case:?}: expected {operation} 404, got {status} {refused:?}"
            );
            assert_eq!(
                error_code(&refused),
                "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException from {operation}, got \
                 {refused:?}"
            );
            let expected = format!("Domain {domain} is not found");
            assert_eq!(
                error_message(&refused),
                expected,
                "case {case:?}: expected {expected:?} from {operation}, got {:?}",
                error_message(&refused)
            );
        }
    }

    // The domain exists, the definition does not: the refusal names the definition.
    let (status, created) = create(&emulator, "here", "Stored", request.clone()).await;
    assert_eq!(
        status, 200,
        "case \"the stored definition\": expected 200, got {status} {created:?}"
    );
    for (case, name) in [
        ("a definition that was never created", "Absent"),
        ("a definition whose name differs in case", "stored"),
    ] {
        let (status, refused) = get(&emulator, "here", name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {refused:?}"
        );
        let expected = format!("SegmentDefinition {name} is not found");
        assert_eq!(
            error_message(&refused),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            error_message(&refused)
        );
    }
}

/// RULE: both segment-definition projections report the modelled `Tags` map PRESENT — the
/// map the create carried, and the EMPTY map for a definition carrying none — so a caller
/// that indexes `Tags` can tell "this definition carries no tags" from "tags were not
/// reported".
#[tokio::test]
async fn both_segment_definition_projections_report_tags_present() {
    let emulator = Emulator::new();
    emulator.create_domain("tagdom", 90).await;

    // `CreateSegmentDefinitionRequest$Tags` targets `TagMap`, bounded at 1..50 entries, so
    // the rows span no `Tags` member at all, one tag, several, and the published maximum.
    let mut at_the_maximum = serde_json::Map::new();
    for index in 0..50 {
        at_the_maximum.insert(format!("key{index:02}"), json!(format!("value{index:02}")));
    }
    let rows: Vec<(&str, &str, Option<Value>)> = vec![
        ("a create carrying no Tags member", "Untagged", None),
        ("one tag", "One_Tag", Some(json!({ "team": "crm" }))),
        (
            "several tags",
            "Three-Tags",
            Some(json!({ "team": "crm", "env": "dev", "cost-center": "42" })),
        ),
        (
            "as many tags as the map allows",
            "Fifty_Tags",
            Some(Value::Object(at_the_maximum)),
        ),
    ];

    for (case, name, tags) in &rows {
        let mut request = json!({
            "DisplayName": "a tagged definition",
            "SegmentSqlQuery": "select profile_id from profiles",
        });
        if let Some(tags) = tags {
            request["Tags"] = tags.clone();
        }
        let expected = expected_tags(&request);

        let (status, created) = create(&emulator, "tagdom", name, request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateSegmentDefinition 200, got {status} {created:?}"
        );
        assert_eq!(
            created.get("Tags"),
            Some(&expected),
            "case {case:?}: expected the create to report Tags {expected:?}, got {:?}",
            created.get("Tags")
        );

        let (status, read) = get(&emulator, "tagdom", name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetSegmentDefinition 200, got {status} {read:?}"
        );
        assert_eq!(
            read.get("Tags"),
            Some(&expected),
            "case {case:?}: expected the read to report Tags {expected:?}, got {:?}",
            read.get("Tags")
        );
    }
}

/// RULE: the refusal of a create that names no segment source carries the service's fixed
/// sentence for a missing source with the request's own `{DomainName}` and
/// `{SegmentDefinitionName}` labels interpolated, so the identifiers in the message are the
/// ones the rejected request carried.
#[tokio::test]
async fn the_missing_source_refusal_names_the_requested_domain_and_definition() {
    let emulator = Emulator::new();
    emulator.create_domain("alpha", 90).await;
    emulator.create_domain("beta-domain", 90).await;

    // The rows differ along the two variable parts of the sentence: the domain, the
    // definition name, and the length of each (`name` is bounded at 1..64 characters).
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a domain and a definition name",
            "alpha".to_string(),
            "Missing_Source".to_string(),
        ),
        (
            "another definition name in that domain",
            "alpha".to_string(),
            "Another-Missing".to_string(),
        ),
        (
            "the same definition name in another domain",
            "beta-domain".to_string(),
            "Missing_Source".to_string(),
        ),
        (
            "the shortest and the longest name the label allows",
            "a".to_string(),
            "N".repeat(64),
        ),
    ];

    for (case, domain, name) in &rows {
        let (status, refused) = create(
            &emulator,
            domain,
            name,
            json!({ "DisplayName": "no source at all" }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {refused:?}"
        );
        let expected = no_source_refusal(domain, name);
        assert_eq!(
            error_message(&refused),
            expected,
            "case {case:?}: expected the sentence {expected:?}, got {:?}",
            error_message(&refused)
        );
    }
}
