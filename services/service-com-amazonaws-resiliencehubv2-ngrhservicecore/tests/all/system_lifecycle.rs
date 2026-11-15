// SPDX-License-Identifier: CC0-1.0

//! The `System` lifecycle, end to end over HTTP: `CreateSystem` -> `GetSystem`.
//!
//! The rules under test:
//!
//! 1. `CreateSystem` STORES the system it was asked for and answers a `System` projected
//!    from that stored record alone — every member the request supplied echoed unchanged,
//!    `sharingEnabled` reporting the stored flag, `tags` always reported (empty map
//!    included), `systemArn` minted in the Region the request is signed for with the
//!    `systemId` it carries, and `createdAt` / `updatedAt` from the create instant — so a
//!    later `GetSystem` of that ARN answers the same projection.
//! 2. `clientToken` is an `@idempotencyToken`: a repeat of the create it was accepted for
//!    answers the system that call created and creates no second one, while the same token
//!    offered for DIFFERENT creation parameters is refused `ConflictException` and writes
//!    nothing.
//! 3. A name a live system already holds is refused `ConflictException`.
//! 4. A well-formed ARN naming no stored system is refused `ResourceNotFoundException`.
//!
//! Every suite drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers and the HTTP status codes are exercised
//! exactly as a client would exercise them.
//!
//! The suites are TABLES: each row is one input of the class the rule covers (which
//! optional members are present, name and description length at their modelled bounds, tag
//! count, each `kmsKeyId` form, which parameter a reused token differs in, Region), not one
//! recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::state::{
    POLICY_RESOURCE, SERVICE_RESOURCE, SYSTEM_RESOURCE, SYSTEM_RESOURCE_TYPE,
};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes these suites drive.
const CREATE_SYSTEM: &str = "/v2/create-system";
const GET_SYSTEM: &str = "/v2/get-system";
const TAGS: &str = "/v2/tags";

/// The Region these suites sign for unless a case names another one.
const REGION: &str = "us-east-1";

/// The account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// `EntityName`'s modelled bounds, which `CreateSystem$name` targets.
///
/// SOURCE: `smithy_query(query="shape", shape="EntityName")` — `length {min: 2, max: 60}`,
/// pattern `^[A-Za-z0-9][A-Za-z0-9_\-]{1,59}$`.
const ENTITY_NAME_MIN_LENGTH: usize = 2;
const ENTITY_NAME_MAX_LENGTH: usize = 60;

/// `EntityDescription`'s modelled ceiling, which `CreateSystem$description` targets.
///
/// SOURCE: `smithy_query(query="operation", operation="CreateSystem")` — `description`
/// targets `EntityDescription`, `length {max: 500}`.
const ENTITY_DESCRIPTION_MAX_LENGTH: usize = 500;

/// `Arn`'s modelled minimum length, which every minted `systemArn` must satisfy.
///
/// SOURCE: `smithy_query(query="shape", shape="Arn")` — `length {min: 31}`.
const ARN_MIN_LENGTH: usize = 31;

/// The members `System` shares with `CreateSystemRequest`, which a create echoes unchanged
/// and reports for no request that did not supply them.
///
/// SOURCE: `smithy_query(query="operation", operation="CreateSystem")` — the request
/// members other than `clientToken` (an `@idempotencyToken`, not a stored member),
/// `sharingEnabled` (a flag with a stored default, asserted separately) and `tags` (always
/// reported, asserted separately).
const ECHOED_MEMBERS: [&str; 3] = ["name", "description", "kmsKeyId"];

/// The members `System` carries that nothing in this service can fill, so every read
/// reports them absent rather than inventing one.
///
/// SOURCE: `smithy_query(query="shape", shape="System")` — `organizationId` is "The AWS
/// Organizations identifier for the system" and `ouId` "The organizational unit (OU)
/// identifier for the system"; this emulator joins no account to an organization.
const UNFILLED_MEMBERS: [&str; 2] = ["organizationId", "ouId"];

/// An emulator holding only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header whose credential scope names this service's signing name
/// (`resiliencehub`) and `region`. Signature verification is off for these tests, so the
/// value only has to parse.
fn signed_for(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY00000000/20260101/{region}\
         /resiliencehub/aws4_request, SignedHeaders=host;x-amz-date, \
         Signature=0123456789abcdef"
    )
}

async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("a JSON body serializes"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", signed_for(region))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    call_in(router, REGION, method, uri, body).await
}

async fn create_system(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(router, "POST", CREATE_SYSTEM, Some(body)).await
}

async fn get_system(router: &MultiServiceRouter, system_arn: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("{GET_SYSTEM}?systemArn={system_arn}"),
        None,
    )
    .await
}

async fn tag_resource(router: &MultiServiceRouter, arn: &str, tags: Value) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("{TAGS}/{}", label(arn)),
        Some(json!({"tags": tags})),
    )
    .await
}

/// An ARN inside a single `@httpLabel` path segment: its `:` and `/` travel as `%3A` and
/// `%2F`, so the whole ARN stays one segment.
fn label(arn: &str) -> String {
    arn.replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

/// The modelled error code of a refusal, from the restJson1 `__type` discriminator.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or("<no __type>")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The `system` member of a successful create or read.
fn system_of(case: &str, status: u16, body: &Value) -> Value {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} {body:?}"
    );
    let system = &body["system"];
    assert!(
        system.is_object(),
        "case {case:?}: the @required system member is absent from {body:?}"
    );
    system.clone()
}

fn system_arn_of(case: &str, system: &Value) -> String {
    system["systemArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no systemArn in {system:?}"))
        .to_string()
}

/// The message AWS words a reused `clientToken` refusal with: fixed text around the token
/// the REQUEST supplied, and nothing else.
fn token_conflict_message(client_token: &str) -> String {
    format!(
        "Got two different request with the same token: {client_token}, please use a \
         different token"
    )
}

/// The message AWS words a taken-name refusal with: the resource type, fixed text, and the
/// name the REQUEST supplied.
fn name_conflict_message(name: &str) -> String {
    format!("{SYSTEM_RESOURCE_TYPE} with name already exists: {name}")
}

/// One row of the create table: the body of a single `CreateSystem` call.
#[derive(Debug, Clone)]
struct Row {
    case: String,
    body: Value,
}

/// The create table.
///
/// Rows differ along which optional members are present (none, some, all), the length of
/// `name` and `description` (both modelled extremes, and an empty description),
/// `sharingEnabled` (absent, `false`, `true`), tag count (none, one, five) and each form
/// `kmsKeyId` accepts ("accepts key ID, key ARN, alias name, or alias ARN").
fn create_rows() -> Vec<Row> {
    vec![
        Row {
            case: "only the required name, at EntityName's minimum length".to_string(),
            body: json!({"name": "Ab"}),
        },
        Row {
            case: "longest name and longest description".to_string(),
            body: json!({
                "name": format!("Z{}", "y".repeat(ENTITY_NAME_MAX_LENGTH - 1)),
                "description": "d".repeat(ENTITY_DESCRIPTION_MAX_LENGTH),
            }),
        },
        Row {
            case: "an empty description and sharing enabled".to_string(),
            body: json!({
                "name": "Empty_Description",
                "description": "",
                "sharingEnabled": true,
            }),
        },
        Row {
            case: "sharing disabled explicitly, a bare key id".to_string(),
            body: json!({
                "name": "0-Shared-Off",
                "sharingEnabled": false,
                "kmsKeyId": "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            }),
        },
        Row {
            case: "an alias name and one tag".to_string(),
            body: json!({
                "name": "Payments-Platform",
                "description": "The payments system",
                "kmsKeyId": "alias/resilience",
                "tags": {"team": "payments"},
            }),
        },
        Row {
            case: "a key ARN and five tags".to_string(),
            body: json!({
                "name": "Tier0_System",
                "kmsKeyId": format!(
                    "arn:aws:kms:{REGION}:{ACCOUNT}:key/\
                     3f2504e0-4f89-11d3-9a0c-0305e82c3301"
                ),
                "tags": {
                    "team": "platform",
                    "env": "prod",
                    "tier": "0",
                    "owner": "resilience",
                    "cost-center": "4711",
                },
            }),
        },
        Row {
            case: "an alias ARN, every optional member set".to_string(),
            body: json!({
                "name": "Everything-Set",
                "description": "Every optional member of the create",
                "sharingEnabled": true,
                "kmsKeyId": format!("arn:aws:kms:{REGION}:{ACCOUNT}:alias/resilience"),
                "tags": {"team": "resilience"},
                "clientToken": "everything-set-token",
            }),
        },
    ]
}

#[tokio::test]
async fn a_created_system_reads_back_through_get_system() {
    let router = emulator();
    let mut minted: Vec<String> = Vec::new();
    for Row { case, body } in create_rows() {
        let (status, answered) = create_system(&router, body.clone()).await;
        let created = system_of(&case, status, &answered);

        // Every member the request supplied is echoed unchanged, and none it did not is
        // reported.
        for member in ECHOED_MEMBERS {
            assert_eq!(
                created[member], body[member],
                "case {case:?}: expected {member} to echo {:?}, got {:?}",
                body[member], created[member]
            );
        }
        // `sharingEnabled` is a stored flag: the value the request set, or off.
        let expected_sharing = body["sharingEnabled"].as_bool().unwrap_or(false);
        assert_eq!(
            created["sharingEnabled"].as_bool(),
            Some(expected_sharing),
            "case {case:?}: expected sharingEnabled {expected_sharing}, got {:?}",
            created["sharingEnabled"]
        );
        // The tag map is ALWAYS reported — a system carrying no tags has the empty map,
        // never an absent member.
        let expected_tags = body["tags"].clone();
        let expected_tags = if expected_tags.is_object() {
            expected_tags
        } else {
            json!({})
        };
        assert_eq!(
            created["tags"], expected_tags,
            "case {case:?}: expected tags {expected_tags:?}, got {:?}",
            created["tags"]
        );
        for member in UNFILLED_MEMBERS {
            assert!(
                created[member].is_null(),
                "case {case:?}: expected {member} to be absent, got {:?}",
                created[member]
            );
        }

        // The ARN is minted in the signing Region, under the system resource type, and
        // carries the `systemId` the body reports.
        let system_arn = system_arn_of(&case, &created);
        let prefix = format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{SYSTEM_RESOURCE}/");
        assert!(
            system_arn.starts_with(&prefix) && system_arn.len() >= ARN_MIN_LENGTH,
            "case {case:?}: expected an ARN starting {prefix:?} and at least \
             {ARN_MIN_LENGTH} characters, got {system_arn:?}"
        );
        let system_id = created["systemId"].as_str().unwrap_or_default();
        assert_eq!(
            system_arn.strip_prefix(&prefix),
            Some(system_id),
            "case {case:?}: expected the ARN to carry systemId {system_id:?}, got \
             {system_arn:?}"
        );
        assert!(
            !minted.contains(&system_arn),
            "case {case:?}: {system_arn:?} was already minted for another system"
        );
        minted.push(system_arn.clone());

        // A just-created system has not been changed since, so both timestamps are the
        // create instant.
        let created_at = created["createdAt"].clone();
        assert!(
            created_at.is_number(),
            "case {case:?}: expected a createdAt timestamp, got {created_at:?}"
        );
        assert_eq!(
            created["updatedAt"], created_at,
            "case {case:?}: expected updatedAt to equal createdAt {created_at:?}, got {:?}",
            created["updatedAt"]
        );

        // The read answers the same projection, from the same stored record.
        let (read_status, read_body) = get_system(&router, &system_arn).await;
        let read = system_of(&case, read_status, &read_body);
        assert_eq!(
            read, created,
            "case {case:?}: GetSystem must answer what CreateSystem stored"
        );
    }
}

#[tokio::test]
async fn a_replayed_client_token_answers_the_system_the_first_call_created() {
    // Rows differ along the token's form and length: `ClientToken` is `len 1..63` with
    // pattern `^[A-Za-z0-9_.-]{0,63}$` (`smithy_query(query="shape", shape="ClientToken")`),
    // so the shortest, a UUID-shaped one, one carrying every allowed punctuation class and
    // the longest are all tokens.
    let router = emulator();
    let tokens = [
        "t".to_string(),
        "3f2504e0-4f89-11d3-9a0c-0305e82c3301".to_string(),
        "token_1.2-3".to_string(),
        "k".repeat(63),
    ];
    for (index, token) in tokens.iter().enumerate() {
        let case = format!("client token of {} characters", token.len());
        let first_body = json!({
            "name": format!("Replayed-{index}"),
            "description": "the first call",
            "sharingEnabled": true,
            "tags": {"team": "payments", "env": "prod"},
            "clientToken": token,
        });
        let (status, body) = create_system(&router, first_body.clone()).await;
        let first = system_of(&case, status, &body);

        // The SAME create under the same token is a retry: it answers the stored system,
        // minting nothing. The tags arrive in the other order, which is the same map.
        let mut repeat = first_body.clone();
        repeat["tags"] = json!({"env": "prod", "team": "payments"});
        let (replay_status, replay_body) = create_system(&router, repeat).await;
        let replayed = system_of(&case, replay_status, &replay_body);
        assert_eq!(
            replayed, first,
            "case {case:?}: a retry under the same token must answer the first system"
        );

        // A DIFFERENT token under a different name is a different system. The token is the
        // same length, so it stays inside `ClientToken`'s bounds.
        let distinct = format!("Z{}", &token[1..]);
        let (other_status, other_body) = create_system(
            &router,
            json!({"name": format!("Distinct-{index}"), "clientToken": distinct}),
        )
        .await;
        let other = system_of(&case, other_status, &other_body);
        assert_ne!(
            system_arn_of(&case, &other),
            system_arn_of(&case, &first),
            "case {case:?}: a fresh clientToken must create a new system"
        );
    }
}

#[tokio::test]
async fn a_client_token_offered_for_different_parameters_is_refused() {
    // Rows differ along WHICH creation parameter the second call changes — the name, the
    // description (changed, added and dropped), the sharing flag, the KMS key, a tag's
    // value and a tag added — because every one of them is part of the create the token
    // was accepted for.
    let token = "shared-token";
    let first_body = json!({
        "name": "Guarded-System",
        "description": "the create the token was accepted for",
        "sharingEnabled": false,
        "kmsKeyId": "alias/resilience",
        "tags": {"team": "payments"},
        "clientToken": token,
    });
    let differences: [(&str, &str, Value); 7] = [
        ("a different name", "name", json!("Other-System")),
        ("a different description", "description", json!("changed")),
        ("no description at all", "description", Value::Null),
        ("the sharing flag flipped", "sharingEnabled", json!(true)),
        ("a different KMS key", "kmsKeyId", json!("alias/other")),
        (
            "a tag value changed",
            "tags",
            json!({"team": "platform"}),
        ),
        (
            "a tag added",
            "tags",
            json!({"team": "payments", "env": "prod"}),
        ),
    ];
    for (case, member, value) in differences {
        // One emulator per row: the rule is about one token against one stored system.
        let router = emulator();
        let (status, body) = create_system(&router, first_body.clone()).await;
        let first = system_of(case, status, &body);
        let system_arn = system_arn_of(case, &first);

        let mut differing = first_body.clone();
        if value.is_null() {
            differing
                .as_object_mut()
                .expect("the create body is an object")
                .remove(member);
        } else {
            differing[member] = value;
        }
        let (conflict_status, conflict) = create_system(&router, differing).await;
        assert_eq!(
            conflict_status, 409,
            "case {case:?}: expected HTTP 409 for a token offered for another create, got \
             {conflict_status} {conflict:?}"
        );
        assert_eq!(
            error_code(&conflict),
            "ConflictException",
            "case {case:?}: expected ConflictException, got {conflict:?}"
        );
        assert_eq!(
            conflict["message"].as_str(),
            Some(token_conflict_message(token).as_str()),
            "case {case:?}: expected the refusal {:?}, got {:?}",
            token_conflict_message(token),
            conflict["message"]
        );

        // The refused call wrote nothing: the stored system is the one the first call
        // created, unchanged.
        let (read_status, read_body) = get_system(&router, &system_arn).await;
        let read = system_of(case, read_status, &read_body);
        assert_eq!(
            read, first,
            "case {case:?}: a refused reuse must leave the stored system untouched"
        );

        // Nothing was reserved either: the same parameters under a FRESH token create a
        // second system.
        let mut retried = first_body.clone();
        retried["name"] = json!("Retried-System");
        retried["clientToken"] = json!("a-fresh-token");
        let (retry_status, retry_body) = create_system(&router, retried).await;
        let retried = system_of(case, retry_status, &retry_body);
        assert_ne!(
            system_arn_of(case, &retried),
            system_arn,
            "case {case:?}: a fresh token must create a second system"
        );
    }
}

#[tokio::test]
async fn a_name_already_taken_is_refused_as_a_conflict() {
    // Rows differ along the name's form and length, so the rule is asserted over the class
    // rather than on one spelling.
    let router = emulator();
    let names = [
        "Ab".to_string(),
        "system_with-punctuation".to_string(),
        "0leading-digit".to_string(),
        format!("W{}", "x".repeat(ENTITY_NAME_MAX_LENGTH - 1)),
    ];
    for name in names {
        assert!(
            name.len() >= ENTITY_NAME_MIN_LENGTH && name.len() <= ENTITY_NAME_MAX_LENGTH,
            "case {name:?}: the row must itself satisfy EntityName's bounds"
        );
        let (status, body) = create_system(&router, json!({"name": name})).await;
        let first = system_of(&name, status, &body);
        let system_arn = system_arn_of(&name, &first);

        let (conflict_status, conflict) =
            create_system(&router, json!({"name": name, "description": "second"})).await;
        assert_eq!(
            conflict_status, 409,
            "case {name:?}: expected HTTP 409 for a name already taken, got \
             {conflict_status} {conflict:?}"
        );
        assert_eq!(
            error_code(&conflict),
            "ConflictException",
            "case {name:?}: expected ConflictException, got {conflict:?}"
        );
        assert_eq!(
            conflict["message"].as_str(),
            Some(name_conflict_message(&name).as_str()),
            "case {name:?}: expected the refusal {:?}, got {:?}",
            name_conflict_message(&name),
            conflict["message"]
        );

        // The refused create left the live system as it was.
        let (read_status, read_body) = get_system(&router, &system_arn).await;
        assert_eq!(
            system_of(&name, read_status, &read_body),
            first,
            "case {name:?}: a refused duplicate must leave the live system untouched"
        );
    }
}

#[tokio::test]
async fn an_arn_naming_no_stored_system_is_not_found() {
    // Rows differ along the ARN's form: a system id this emulator never minted, another
    // Region's, another account's, and well-formed ARNs of the store's other two resource
    // types. Each satisfies `Arn`'s modelled pattern, so each reaches the handler.
    let router = emulator();
    // One stored system, so the refusals are about the ARN rather than an empty store.
    let (status, body) = create_system(&router, json!({"name": "A-Live-System"})).await;
    system_of("a live system", status, &body);
    let arns = [
        format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{SYSTEM_RESOURCE}/missing"),
        format!("arn:aws:resiliencehub:eu-central-1:{ACCOUNT}:{SYSTEM_RESOURCE}/absent"),
        format!("arn:aws:resiliencehub:{REGION}:210987654321:{SYSTEM_RESOURCE}/elsewhere"),
        format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{SERVICE_RESOURCE}/not-a-system"),
        format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{POLICY_RESOURCE}/not-a-system"),
    ];
    for arn in arns {
        let (status, body) = get_system(&router, &arn).await;
        assert_eq!(
            status, 404,
            "case {arn:?}: expected HTTP 404 for an ARN naming no system, got {status} \
             {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {arn:?}: expected ResourceNotFoundException, got {body:?}"
        );
        // The refusal identifies the resource by the ARN's resource segment, under the
        // type the model's shape for a system carries.
        let resource_id = arn.splitn(6, ':').nth(5).unwrap_or(&arn);
        assert_eq!(
            body["resourceId"].as_str(),
            Some(resource_id),
            "case {arn:?}: expected resourceId {resource_id:?}, got {:?}",
            body["resourceId"]
        );
        assert_eq!(
            body["resourceType"].as_str(),
            Some(SYSTEM_RESOURCE_TYPE),
            "case {arn:?}: expected resourceType {SYSTEM_RESOURCE_TYPE:?}, got {:?}",
            body["resourceType"]
        );
        let expected = format!("{SYSTEM_RESOURCE_TYPE} not found: {resource_id}");
        assert_eq!(
            body["message"].as_str(),
            Some(expected.as_str()),
            "case {arn:?}: expected the refusal {expected:?}, got {:?}",
            body["message"]
        );
    }
}

#[tokio::test]
async fn a_minted_system_arn_names_the_region_the_request_is_signed_for() {
    // Rows differ along the Region's partition form: a commercial Region, a European one, a
    // three-segment one and a GovCloud one all satisfy `AwsRegion`'s modelled pattern.
    let router = emulator();
    for (index, region) in ["us-east-1", "eu-west-2", "ap-southeast-3", "us-gov-west-1"]
        .into_iter()
        .enumerate()
    {
        let case = format!("signed for {region}");
        let (status, body) = call_in(
            &router,
            region,
            "POST",
            CREATE_SYSTEM,
            Some(json!({"name": format!("Regional-{index}")})),
        )
        .await;
        let created = system_of(&case, status, &body);
        let system_arn = system_arn_of(&case, &created);
        let prefix = format!("arn:aws:resiliencehub:{region}:{ACCOUNT}:{SYSTEM_RESOURCE}/");
        assert!(
            system_arn.starts_with(&prefix),
            "case {case:?}: expected an ARN starting {prefix:?}, got {system_arn:?}"
        );
    }
}

#[tokio::test]
async fn a_tag_written_to_a_system_is_visible_to_the_next_read() {
    // Rows differ along what the system already carries and what the write does: adding to
    // an untagged system, adding a key beside an existing one, and overwriting a key's
    // value.
    let router = emulator();
    let rows: [(&str, Value, Value, Value); 3] = [
        (
            "an untagged system gains a tag",
            json!({"name": "Untagged-System"}),
            json!({"team": "payments"}),
            json!({"team": "payments"}),
        ),
        (
            "a tagged system gains another key",
            json!({"name": "Tagged-System", "tags": {"team": "payments"}}),
            json!({"env": "prod"}),
            json!({"team": "payments", "env": "prod"}),
        ),
        (
            "an existing key takes the new value",
            json!({"name": "Retagged-System", "tags": {"team": "payments"}}),
            json!({"team": "platform"}),
            json!({"team": "platform"}),
        ),
    ];
    for (case, create_body, write, expected) in rows {
        let (status, body) = create_system(&router, create_body).await;
        let created = system_of(case, status, &body);
        let system_arn = system_arn_of(case, &created);

        let (tag_status, tag_body) = tag_resource(&router, &system_arn, write).await;
        assert!(
            (200..300).contains(&tag_status),
            "case {case:?}: expected TagResource to succeed, got {tag_status} {tag_body:?}"
        );

        let (read_status, read_body) = get_system(&router, &system_arn).await;
        let read = system_of(case, read_status, &read_body);
        assert_eq!(
            read["tags"], expected,
            "case {case:?}: expected tags {expected:?}, got {:?}",
            read["tags"]
        );
    }
}
