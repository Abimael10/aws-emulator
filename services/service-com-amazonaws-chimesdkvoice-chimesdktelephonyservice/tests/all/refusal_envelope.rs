// SPDX-License-Identifier: CC0-1.0

//! What this service ANSWERS a refused request with, end to end over the wire.
//!
//! Four rules, each pinned over a SET of inputs rather than one request:
//!
//! 1. a request that breaks a rule the MODEL declares is refused by the input-validation
//!    layer with the COUNTED report `N validation error(s) detected: <clause>; …` in the
//!    lowercase `message` member, under that layer's own error class — so the table below
//!    spans the constraint classes (an absent member, a pattern, an enumerated value set)
//!    and both counts, over two operations;
//! 2. a request that addresses a resource by id that names none states the resource class
//!    it could not resolve and quotes the id the request gave;
//! 3. every client-error body of this service names the request it answers in a `RequestId`
//!    member, whichever layer answered;
//! 4. a Voice Connector group's create RESOLVES the connectors its routing references: a
//!    group whose references all resolve is created and reads back, and one that references
//!    a connector nothing holds is the lookup failure 404 — which is answered ahead of the
//!    request's remaining input constraints, so a request that is also outside one of them
//!    still reports the unresolved reference.
//!
//! The harness is the sibling suite's (`crate::voice_connector_lifecycle`): one shared
//! engine, the service, and the router dispatching signed restJson1 requests to it, so
//! routing, deserialization, the modelled constraints, the handler and the response envelope
//! are all exercised.

use serde_json::{json, Value};
use service_common::query_error_envelope::is_request_id;

use crate::voice_connector_lifecycle::{call, emulator, Reply, MODELLED_RULE_CLASS};

/// The error class a not-found refusal is answered under: the shape the model declares for
/// it (`smithy_query(query="operation", operation="GetVoiceConnector")` lists
/// `NotFoundException`, `@httpError 404`).
const NOT_FOUND_CLASS: &str = "NotFoundException";

/// A `KmsKeyArn` inside the expression `Arn` declares
/// (`smithy_query(query="shape", shape="Arn")`: `^arn[\/\:\-\_\.a-zA-Z0-9]+$`), so a case
/// using it breaks no rule of its own.
const WELL_FORMED_KEY_ARN: &str = "arn:aws:kms:us-east-1:000000000000:key/abcd1234";

/// `VoiceProfileDomainName`'s expression as this API publishes it: the model's
/// `^[a-zA-Z0-9 _.-]+$` without the whole-value anchor pair
/// (`smithy_query(query="shape", shape="VoiceProfileDomainName")`).
const PUBLISHED_NAME_PATTERN: &str = "[a-zA-Z0-9 _.-]+";

/// `Arn`'s expression as this API publishes it: as the model declares it, anchors included.
const PUBLISHED_ARN_PATTERN: &str = r"^arn[\/\:\-\_\.a-zA-Z0-9]+$";

/// Every value `CallDistributionType` admits, in the order a refusal lists them
/// (lexicographic, whatever order the model declares).
/// Source: `smithy_query(query="shape", shape="CallDistributionType")`.
const CALL_DISTRIBUTION_TYPES: [&str; 2] =
    ["LoadBalancedDistribution", "PriorityWeightedDistribution"];

/// The `message` member of a refusal body, which is where the validation report travels.
fn report(reply: &Reply, case: &str) -> String {
    reply.json(case)["message"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a lowercase `message` member, got {}",
                reply.json(case)
            )
        })
        .to_string()
}

/// The `RequestId` member of an error body — the member this service's error ENVELOPE names
/// the request it answers in.
fn request_id(reply: &Reply, case: &str) -> String {
    reply.json(case)["RequestId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a RequestId member, got {}",
                reply.json(case)
            )
        })
        .to_string()
}

/// RULE 1: a refusal of a modelled rule states the COUNT of violations it found and then one
/// clause per violation, naming the member and the requirement it broke.
#[tokio::test]
async fn a_modelled_rule_is_refused_with_the_counted_validation_report() {
    let router = emulator();
    // The rows span the constraint classes and both operations that create a resource from a
    // body, so the report is the validation layer's and not one handler's.
    let rows: Vec<(String, &str, String, Option<Value>, String)> = vec![
        (
            "the group's required Name absent".to_string(),
            "POST",
            "/voice-connector-groups".to_string(),
            Some(json!({})),
            "1 validation error detected: Value null at 'name' failed to satisfy constraint: \
             Member must not be null"
                .to_string(),
        ),
        (
            "the domain's required Name absent".to_string(),
            "POST",
            "/voice-profile-domains".to_string(),
            Some(json!({
                "ServerSideEncryptionConfiguration": { "KmsKeyArn": WELL_FORMED_KEY_ARN },
            })),
            "1 validation error detected: Value null at 'name' failed to satisfy constraint: \
             Member must not be null"
                .to_string(),
        ),
        (
            "the domain's required encryption configuration absent".to_string(),
            "POST",
            "/voice-profile-domains".to_string(),
            Some(json!({ "Name": "a-domain" })),
            "1 validation error detected: Value null at \
             'serverSideEncryptionConfiguration' failed to satisfy constraint: Member must \
             not be null"
                .to_string(),
        ),
        (
            "a name outside the published expression".to_string(),
            "POST",
            "/voice-profile-domains".to_string(),
            Some(json!({
                "Name": "bad!name!",
                "ServerSideEncryptionConfiguration": { "KmsKeyArn": WELL_FORMED_KEY_ARN },
            })),
            format!(
                "1 validation error detected: Value 'bad!name!' at 'name' failed to satisfy \
                 constraint: Member must satisfy regular expression pattern: \
                 {PUBLISHED_NAME_PATTERN}"
            ),
        ),
        (
            // The member is `@sensitive`, so the clause names it and the expression and
            // quotes no value at all.
            "a key ARN outside the published expression".to_string(),
            "POST",
            "/voice-profile-domains".to_string(),
            Some(json!({
                "Name": "a-domain",
                "ServerSideEncryptionConfiguration": { "KmsKeyArn": "not-an-arn" },
            })),
            format!(
                "1 validation error detected: Value at \
                 'serverSideEncryptionConfiguration.kmsKeyArn' failed to satisfy constraint: \
                 Member must satisfy regular expression pattern: {PUBLISHED_ARN_PATTERN}"
            ),
        ),
        (
            "a call distribution type outside the enumerated set".to_string(),
            "POST",
            "/voice-connector-groups".to_string(),
            Some(json!({ "Name": "a-group", "CallDistributionType": "NOT_A_TYPE" })),
            format!(
                "1 validation error detected: Value 'NOT_A_TYPE' at 'callDistributionType' \
                 failed to satisfy constraint: Member must satisfy enum value set: [{}, {}]",
                CALL_DISTRIBUTION_TYPES[0], CALL_DISTRIBUTION_TYPES[1]
            ),
        ),
        (
            "two members outside their expressions at once".to_string(),
            "POST",
            "/voice-profile-domains".to_string(),
            Some(json!({
                "Name": "bad!name!",
                "ServerSideEncryptionConfiguration": { "KmsKeyArn": "not-an-arn" },
            })),
            format!(
                "2 validation errors detected: Value 'bad!name!' at 'name' failed to satisfy \
                 constraint: Member must satisfy regular expression pattern: \
                 {PUBLISHED_NAME_PATTERN}; Value at \
                 'serverSideEncryptionConfiguration.kmsKeyArn' failed to satisfy constraint: \
                 Member must satisfy regular expression pattern: {PUBLISHED_ARN_PATTERN}"
            ),
        ),
    ];

    for (case, method, path, body, expected) in rows {
        let reply = call(&router, method, &path, body).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {:?}",
            reply.status,
            String::from_utf8_lossy(&reply.body)
        );
        assert_eq!(
            report(&reply, &case),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            report(&reply, &case)
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some(MODELLED_RULE_CLASS),
            "case {case:?}: expected the error class {MODELLED_RULE_CLASS}, got {:?}",
            reply.error_type
        );
    }
}

/// RULE 2: an id that names no stored resource is refused with the resource CLASS that could
/// not be resolved and the id the request gave.
#[tokio::test]
async fn an_unknown_id_states_the_resource_class_and_the_id() {
    let router = emulator();
    // Both alternatives of the connector id pattern and two spellings of a group id, so the
    // sentence is not keyed to one id.
    let connector_ids = [
        "abcdef0123456789abcde",
        "abcdef0123456789abcdef",
        "0a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d",
    ];
    let group_ids = ["abcdef0123456789abcde", "a-group-id-nothing-holds"];

    for id in connector_ids {
        let expected = format!("Could not find VoiceConnector with Id '{id}'");
        for (method, path, body) in [
            ("GET", format!("/voice-connectors/{id}"), None),
            (
                "PUT",
                format!("/voice-connectors/{id}"),
                Some(json!({ "Name": "nobody", "RequireEncryption": true })),
            ),
            ("DELETE", format!("/voice-connectors/{id}"), None),
        ] {
            let case = format!("{method} {path}");
            let reply = call(&router, method, &path, body).await;
            assert_eq!(
                reply.status, 404,
                "case {case:?}: expected HTTP 404, got {} with body {:?}",
                reply.status,
                String::from_utf8_lossy(&reply.body)
            );
            let body = reply.json(&case);
            assert_eq!(
                body["Code"],
                json!("NotFound"),
                "case {case:?}: expected the NotFound code, got {body}"
            );
            assert_eq!(
                body["Message"],
                json!(expected),
                "case {case:?}: expected {expected:?}, got {body}"
            );
        }
    }

    for id in group_ids {
        let case = format!("GET /voice-connector-groups/{id}");
        let expected =
            format!("Could not find VoiceConnectorGroup for voice connector group id '{id}'");
        let reply = call(&router, "GET", &format!("/voice-connector-groups/{id}"), None).await;
        assert_eq!(
            reply.status, 404,
            "case {case:?}: expected HTTP 404, got {} with body {:?}",
            reply.status,
            String::from_utf8_lossy(&reply.body)
        );
        let body = reply.json(&case);
        assert_eq!(
            body["Code"],
            json!("NotFound"),
            "case {case:?}: expected the NotFound code, got {body}"
        );
        assert_eq!(
            body["Message"],
            json!(expected),
            "case {case:?}: expected {expected:?}, got {body}"
        );
    }
}

/// RULE 3: every client-error body names the request it answers, whichever layer answered —
/// the validation layer ahead of the handler, a handler's modelled error, and the
/// unresolved-reference refusal — and two answers never name the same request.
#[tokio::test]
async fn every_error_body_names_the_request_it_answers() {
    let router = emulator();
    let cases: Vec<(String, &str, String, Option<Value>)> = vec![
        (
            "a modelled rule refused before any handler ran".to_string(),
            "POST",
            "/voice-connector-groups".to_string(),
            Some(json!({})),
        ),
        (
            "a handler's own not-found".to_string(),
            "GET",
            "/voice-connectors/abcdef0123456789abcde".to_string(),
            None,
        ),
        (
            "a group referencing a connector nothing holds".to_string(),
            "POST",
            "/voice-connector-groups".to_string(),
            Some(json!({
                "Name": "a-group",
                "VoiceConnectorItems": [{ "VoiceConnectorId": "abcdef0123456789abcde" }],
            })),
        ),
    ];

    let mut reported: Vec<String> = Vec::new();
    for (case, method, path, body) in cases {
        let reply = call(&router, method, &path, body.clone()).await;
        assert!(
            reply.status >= 400,
            "case {case:?}: expected a client error, got {}",
            reply.status
        );
        let id = request_id(&reply, &case);
        assert!(
            is_request_id(&id),
            "case {case:?}: expected a well-formed request id, got {id:?}"
        );
        // The same call again answers a DIFFERENT request, so the id belongs to the
        // response rather than to the input.
        let again = call(&router, method, &path, body).await;
        let second = request_id(&again, &case);
        assert_ne!(
            id, second,
            "case {case:?}: two answers named the same request ({id})"
        );
        reported.push(id);
        reported.push(second);
    }

    // A success is left alone: its body is the modelled output, which declares no such
    // member.
    let created = call(
        &router,
        "POST",
        "/voice-connectors",
        Some(json!({ "Name": "named-request-probe", "RequireEncryption": true })),
    )
    .await;
    assert_eq!(created.status, 201, "the probe create failed");
    assert_eq!(
        created.json("a success")["RequestId"],
        Value::Null,
        "case \"a success\": expected no RequestId member, got {}",
        created.json("a success")
    );
}

/// RULE 4 (the positive half): a group whose every reference resolves is created, and the
/// stored group reads back exactly as the create reported it.
#[tokio::test]
async fn a_group_whose_references_resolve_is_created_and_reads_back() {
    let router = emulator();
    let mut connectors: Vec<String> = Vec::new();
    for n in 0..3 {
        let reply = call(
            &router,
            "POST",
            "/voice-connectors",
            Some(json!({ "Name": format!("routed-{n}"), "RequireEncryption": true })),
        )
        .await;
        assert_eq!(reply.status, 201, "seeding connector {n} failed");
        connectors.push(
            reply.json("seed")["VoiceConnector"]["VoiceConnectorId"]
                .as_str()
                .expect("a minted id")
                .to_string(),
        );
    }

    // The rows differ along how many connectors the group routes to (none, one, the
    // documented maximum of three), along whether a priority is given or defaulted, and
    // along whether a call distribution type is named.
    let rows: Vec<(String, Value)> = vec![
        (
            "no items at all".to_string(),
            json!({ "Name": "group-no-items" }),
        ),
        (
            "one item, priority defaulted".to_string(),
            json!({
                "Name": "group-one-item",
                "VoiceConnectorItems": [{ "VoiceConnectorId": connectors[0] }],
            }),
        ),
        (
            "three items with stated priorities and a distribution type".to_string(),
            json!({
                "Name": "group-three-items",
                "CallDistributionType": CALL_DISTRIBUTION_TYPES[0],
                "VoiceConnectorItems": [
                    { "VoiceConnectorId": connectors[0], "Priority": 1 },
                    { "VoiceConnectorId": connectors[1], "Priority": 2 },
                    { "VoiceConnectorId": connectors[2], "Priority": 99 },
                ],
            }),
        ),
    ];

    for (case, body) in rows {
        let created = call(&router, "POST", "/voice-connector-groups", Some(body.clone())).await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected HTTP 201, got {} with body {:?}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        let group = created.json(&case)["VoiceConnectorGroup"].clone();
        let id = group["VoiceConnectorGroupId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the created group has no id: {group}"))
            .to_string();
        assert_eq!(
            group["Name"], body["Name"],
            "case {case:?}: expected the name the request gave, got {group}"
        );
        assert_eq!(
            group["VoiceConnectorGroupArn"],
            json!(format!(
                "arn:aws:chime:us-east-1:000000000000:vcg/{id}"
            )),
            "case {case:?}: expected the ARN derived from the minted id, got {group}"
        );
        // Every referenced connector is reported back, in the order the request named
        // them, each with the priority the request gave or the modelled default.
        let expected_items: Vec<Value> = body["VoiceConnectorItems"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|item| {
                json!({
                    "VoiceConnectorId": item["VoiceConnectorId"],
                    "Priority": item.get("Priority").cloned().unwrap_or(json!(1)),
                })
            })
            .collect();
        assert_eq!(
            group["VoiceConnectorItems"],
            json!(expected_items),
            "case {case:?}: expected the items the request named, got {group}"
        );

        // The write is in state, not just in the answer.
        let fetched = call(
            &router,
            "GET",
            &format!("/voice-connector-groups/{id}"),
            None,
        )
        .await;
        assert_eq!(
            fetched.status, 200,
            "case {case:?}: the read-back failed with {} and body {:?}",
            fetched.status,
            String::from_utf8_lossy(&fetched.body)
        );
        assert_eq!(
            fetched.json(&case)["VoiceConnectorGroup"], group,
            "case {case:?}: the read-back does not match the create's answer"
        );
    }
}

/// RULE 4 (the refusal half): a group that references a connector nothing holds is the
/// lookup failure — 404, the not-found code, the fixed sentence for an unresolved reference
/// and the identifier that resolved to nothing — and it is answered AHEAD of the request's
/// remaining input constraints.
#[tokio::test]
async fn a_group_referencing_an_absent_connector_is_not_found() {
    let router = emulator();
    let seeded = call(
        &router,
        "POST",
        "/voice-connectors",
        Some(json!({ "Name": "one-that-exists", "RequireEncryption": true })),
    )
    .await;
    assert_eq!(seeded.status, 201, "seeding the existing connector failed");
    let existing = seeded.json("seed")["VoiceConnector"]["VoiceConnectorId"]
        .as_str()
        .expect("a minted id")
        .to_string();

    let absent = "abcdef0123456789abcde";
    let other_absent = "0a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d";
    // The rows differ along where the absent reference sits, how it is spelled, and whether
    // the same request is ALSO outside a modelled bound — which the lookup is resolved
    // ahead of, so the answer still names the reference.
    let rows: Vec<(String, Value, &str)> = vec![
        (
            "the only reference is absent".to_string(),
            json!({
                "Name": "a-group",
                "VoiceConnectorItems": [{ "VoiceConnectorId": absent }],
            }),
            absent,
        ),
        (
            "the absent reference follows one that resolves".to_string(),
            json!({
                "Name": "a-group",
                "VoiceConnectorItems": [
                    { "VoiceConnectorId": existing },
                    { "VoiceConnectorId": other_absent },
                ],
            }),
            other_absent,
        ),
        (
            "the first absent reference of two".to_string(),
            json!({
                "Name": "a-group",
                "VoiceConnectorItems": [
                    { "VoiceConnectorId": absent },
                    { "VoiceConnectorId": other_absent },
                ],
            }),
            absent,
        ),
        (
            "an absent reference beside a priority below the modelled minimum".to_string(),
            json!({
                "Name": "a-group",
                "VoiceConnectorItems": [{ "VoiceConnectorId": absent, "Priority": 0 }],
            }),
            absent,
        ),
    ];

    for (case, body, named) in rows {
        let reply = call(&router, "POST", "/voice-connector-groups", Some(body)).await;
        assert_eq!(
            reply.status, 404,
            "case {case:?}: expected HTTP 404, got {} with body {:?}",
            reply.status,
            String::from_utf8_lossy(&reply.body)
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some(NOT_FOUND_CLASS),
            "case {case:?}: expected the error class {NOT_FOUND_CLASS}, got {:?}",
            reply.error_type
        );
        let answered = reply.json(&case);
        assert_eq!(
            answered["Code"],
            json!("NotFound"),
            "case {case:?}: expected the NotFound code, got {answered}"
        );
        assert_eq!(
            answered["Message"],
            json!("Unable to find the requested identifier"),
            "case {case:?}: expected the unresolved-reference sentence, got {answered}"
        );
        assert_eq!(
            answered["Resource"],
            json!(named),
            "case {case:?}: expected the identifier that resolved to nothing, got {answered}"
        );
    }

    // A group whose references all resolve is still created, so the refusals above are the
    // rule's edge and not a blanket rejection.
    let created = call(
        &router,
        "POST",
        "/voice-connector-groups",
        Some(json!({
            "Name": "a-resolvable-group",
            "VoiceConnectorItems": [{ "VoiceConnectorId": existing }],
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case \"every reference resolves\": expected HTTP 201, got {} with body {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
}

/// RULE 4, the other half of the group's own bounds: a group may route to at most the
/// documented number of Voice Connectors, and one past it is the error the model declares
/// for a resource limit.
#[tokio::test]
async fn a_group_routes_to_at_most_the_documented_number_of_connectors() {
    let router = emulator();
    let limit = service_com_amazonaws_chimesdkvoice_chimesdktelephonyservice::state::MAX_VOICE_CONNECTOR_ITEMS_PER_GROUP;
    let mut connectors: Vec<Value> = Vec::new();
    for n in 0..=limit {
        let reply = call(
            &router,
            "POST",
            "/voice-connectors",
            Some(json!({ "Name": format!("bounded-{n}"), "RequireEncryption": true })),
        )
        .await;
        assert_eq!(reply.status, 201, "seeding connector {n} failed");
        connectors.push(json!({
            "VoiceConnectorId": reply.json("seed")["VoiceConnector"]["VoiceConnectorId"],
        }));
    }

    // At the bound, and one past it.
    let at_the_bound = call(
        &router,
        "POST",
        "/voice-connector-groups",
        Some(json!({ "Name": "at-the-bound", "VoiceConnectorItems": connectors[..limit] })),
    )
    .await;
    assert_eq!(
        at_the_bound.status, 201,
        "case \"{limit} items\": expected HTTP 201 at the bound, got {} with body {:?}",
        at_the_bound.status,
        String::from_utf8_lossy(&at_the_bound.body)
    );

    let past_the_bound = call(
        &router,
        "POST",
        "/voice-connector-groups",
        Some(json!({ "Name": "past-the-bound", "VoiceConnectorItems": connectors })),
    )
    .await;
    let case = format!("{} items", limit + 1);
    assert_eq!(
        past_the_bound.status, 400,
        "case {case:?}: expected HTTP 400 past the bound, got {} with body {:?}",
        past_the_bound.status,
        String::from_utf8_lossy(&past_the_bound.body)
    );
    assert_eq!(
        past_the_bound.error_code(&case),
        "ResourceLimitExceededException",
        "case {case:?}: expected ResourceLimitExceededException, got {}",
        past_the_bound.error_code(&case)
    );
}
