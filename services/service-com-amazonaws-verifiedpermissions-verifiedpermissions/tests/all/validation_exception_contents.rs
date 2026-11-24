// SPDX-License-Identifier: CC0-1.0

//! End-to-end suite for what a 400 `ValidationException` CARRIES, in the two
//! places this service fills one in.
//!
//! 1. A request that broke a constraint the MODEL declares is refused at the
//!    boundary, before any handler runs, and the sentence that refusal states
//!    addresses the offending member as a JSON pointer rooted at the request
//!    body, terminates the count with a period, and states nothing of the value
//!    that was submitted.
//! 2. Policy content a `STRICT` store's SCHEMA refuses reports ONE `fieldList`
//!    entry per diagnostic the Cedar validator raised about it — not just the
//!    first — so a client that reads the whole diagnostic set sees every problem
//!    with the policy at once.
//!
//! Every assertion goes over the wire through the same router the emulator binary
//! serves, reusing the harness in [`crate::harness`].

use serde_json::{json, Value};
use service_com_amazonaws_verifiedpermissions_verifiedpermissions::ops::INVALID_INPUT_MESSAGE;

use crate::harness::{
    call, error_code, fault_message, field_list, ok, router, store_with_mode, user_pool_arn,
};
use crate::schema::photo_app_schema;

// ---------------------------------------------------------------------------
// 1. The boundary refusal's sentence
// ---------------------------------------------------------------------------

/// `CognitoUserPoolConfiguration$userPoolArn`'s `@pattern`, as the model writes
/// it — anchors included (`smithy_query(query="shape",
/// shape="CognitoUserPoolConfiguration")`).
const USER_POOL_ARN_PATTERN: &str =
    r"^arn:[a-zA-Z0-9-]+:cognito-idp:(([a-zA-Z0-9-]+:\d{12}:userpool/[\w-]+_[0-9a-zA-Z]+))$";

/// `OpenIdConnectConfiguration$issuer`'s `@pattern`
/// (`smithy_query(query="shape", shape="OpenIdConnectConfiguration")`).
const ISSUER_PATTERN: &str = r"^https://.*$";

/// `UserPoolArn`'s `@length` maximum (`smithy_query(query="shape",
/// shape="CognitoUserPoolConfiguration")`).
const USER_POOL_ARN_MAX_LENGTH: usize = 255;

/// A well-formed Cognito user pool ARN that is `bytes` long, reached by padding
/// the POOL ID — so the only constraint such a value can break is the length
/// maximum, never the pattern.
fn user_pool_arn_of(bytes: usize) -> String {
    let base = user_pool_arn("us-east-1", "us-east-1_");
    format!(
        "{base}{}",
        "a".repeat(bytes.saturating_sub(base.chars().count()))
    )
}

/// The clause one broken constraint states: the member as a JSON pointer, the
/// requirement it failed, and nothing of the submitted value.
fn clause(pointer: &str, requirement: &str) -> String {
    format!("Value at '{pointer}' failed to satisfy constraint: {requirement}")
}

/// `TagValue`'s modelled length maximum, as the model writes it
/// (`smithy_query(query="shape", shape="TagMap")`: value `length 0..256`).
const TAG_VALUE_MAX_LENGTH: usize = 256;

/// The requirement a broken `@length` minimum of 1 states.
const LENGTH_AT_LEAST_ONE: &str = "Member must have length greater than or equal to 1";

/// `EntityIdentifier$entityType`'s modelled `@length` minimum is 1
/// (`smithy_query(query="shape", shape="EntityIdentifier")`), and an
/// `AttributeValue` can carry an `entityIdentifier` — so an attribute value whose
/// entity type is empty is refused by a constraint the model declares, from
/// INSIDE the map entry that carries it.
fn entity_identifier_without_a_type() -> Value {
    json!({ "entityIdentifier": { "entityType": "", "entityId": "alice" } })
}

/// One well-formed entity item carrying nothing but its identifier.
fn entity(id: &str) -> Value {
    json!({ "identifier": { "entityType": "PhotoApp::Photo", "entityId": id } })
}

/// A well-formed `IsAuthorized` request against `store`, carrying `payload`
/// under the member `name` (`context` or `entities`) — everything the call needs
/// to be evaluated, so the only thing a refusal can be about is that payload.
fn is_authorized(store: &str, name: &str, payload: Value) -> Value {
    let mut request = json!({
        "policyStoreId": store,
        "principal": { "entityType": "PhotoApp::User", "entityId": "alice" },
        "action": { "actionType": "PhotoApp::Action", "actionId": "viewPhoto" },
        "resource": { "entityType": "PhotoApp::Photo", "entityId": "vacation" },
    });
    request[name] = payload;
    request
}

/// RULE: a pointer segment for a step from a collection into one of its elements
/// spells the element the way the request body addresses it, decided by the TYPE
/// the model declares for that collection and never by how the segment reads: an
/// element of a LIST is named by its own 0-based position (RFC 6901), and an entry
/// of a MAP by the key the caller sent — all digits or not — with `~` and `/`
/// escaped. So every reported pointer resolves against the body that was sent,
/// which is the one property `ValidationExceptionField$path` exists to have.
///
/// The rows span which kind of collection the step enters (a map whose values are
/// scalar, a map whose values are structured, a list, a list inside a map entry, a
/// map inside a map entry, a map inside a list element), how the key READS (an
/// all-digit year, `1`, `0`, digits with a leading zero, a word, a key holding
/// both characters a pointer escapes), which operation and member carries the map
/// (`TagResource$tags`, `CreatePolicyStore$tags`, an authorization request's
/// `context.contextMap`, an entity's `attributes` and `tags`, a Cedar `record`
/// nested in an attribute value), and the constraint class that was broken (a
/// length maximum, a length minimum).
#[tokio::test]
async fn a_refusal_names_a_collection_element_the_way_the_request_body_addresses_it() {
    let router = router();
    let store_id = store_with_mode(&router, "OFF").await;
    let arn = ok(
        &router,
        "CreatePolicyStore",
        &json!({ "validationSettings": { "mode": "OFF" } }),
    )
    .await["arn"]
        .as_str()
        .expect("arn is required on the CreatePolicyStore response")
        .to_string();
    let over_long_value = "v".repeat(TAG_VALUE_MAX_LENGTH + 1);
    let too_long = format!("Member must have length less than or equal to {TAG_VALUE_MAX_LENGTH}");

    // (case, operation, request, the pointer the clause must name, the
    //  requirement it must state)
    let cases: Vec<(&str, &str, Value, &str, String)> = vec![
        (
            // The member the issue names. A tag map's values are SCALAR, so the
            // walk spells an entry's path with the key and no collection step —
            // this row pins that spelling rather than changing it.
            "an all-digit key on a tag map",
            "TagResource",
            json!({ "resourceArn": arn, "tags": { "2024": over_long_value } }),
            "/tags/2024",
            too_long.clone(),
        ),
        (
            // The one key a 1-based count would renumber to the first element.
            "the key 1 on a tag map",
            "TagResource",
            json!({ "resourceArn": arn, "tags": { "1": over_long_value } }),
            "/tags/1",
            too_long.clone(),
        ),
        (
            "the key 0 on a tag map",
            "TagResource",
            json!({ "resourceArn": arn, "tags": { "0": over_long_value } }),
            "/tags/0",
            too_long.clone(),
        ),
        (
            "digits with a leading zero on a tag map",
            "TagResource",
            json!({ "resourceArn": arn, "tags": { "007": over_long_value } }),
            "/tags/007",
            too_long.clone(),
        ),
        (
            "a word key on a tag map",
            "TagResource",
            json!({ "resourceArn": arn, "tags": { "env": over_long_value } }),
            "/tags/env",
            too_long.clone(),
        ),
        (
            // The same member name on a different operation's map.
            "an all-digit key on a create's tag map",
            "CreatePolicyStore",
            json!({
                "validationSettings": { "mode": "OFF" },
                "tags": { "2024": over_long_value },
            }),
            "/tags/2024",
            too_long.clone(),
        ),
        (
            // A map whose VALUES are structured, so the refusal is reported from
            // inside the entry and the clause addresses a member reached THROUGH
            // it — the step a key that reads like a position is spelled by.
            "an all-digit key on an authorization request's context map",
            "IsAuthorized",
            is_authorized(
                &store_id,
                "context",
                json!({ "contextMap": { "2024": entity_identifier_without_a_type() } }),
            ),
            "/context/contextMap/2024/entityIdentifier/entityType",
            LENGTH_AT_LEAST_ONE.to_string(),
        ),
        (
            // The one key a 1-based count would renumber to the first element.
            "the key 1 on a context map",
            "IsAuthorized",
            is_authorized(
                &store_id,
                "context",
                json!({ "contextMap": { "1": entity_identifier_without_a_type() } }),
            ),
            "/context/contextMap/1/entityIdentifier/entityType",
            LENGTH_AT_LEAST_ONE.to_string(),
        ),
        (
            // A word key: the spelling that was always kept, so the rows that
            // keep a digit key and this one are the same shape.
            "a word key on a context map",
            "IsAuthorized",
            is_authorized(
                &store_id,
                "context",
                json!({ "contextMap": { "tenant": entity_identifier_without_a_type() } }),
            ),
            "/context/contextMap/tenant/entityIdentifier/entityType",
            LENGTH_AT_LEAST_ONE.to_string(),
        ),
        (
            // Both characters a pointer segment gives a meaning of its own.
            "a context-map key holding the two characters a pointer escapes",
            "IsAuthorized",
            is_authorized(
                &store_id,
                "context",
                json!({ "contextMap": { "a/b~c": entity_identifier_without_a_type() } }),
            ),
            "/context/contextMap/a~1b~0c/entityIdentifier/entityType",
            LENGTH_AT_LEAST_ONE.to_string(),
        ),
        (
            // A map inside a map entry: two keyed steps in one pointer.
            "an all-digit field name of a record nested in a context value",
            "IsAuthorized",
            is_authorized(
                &store_id,
                "context",
                json!({ "contextMap": {
                    "42": { "record": { "7": entity_identifier_without_a_type() } } } }),
            ),
            "/context/contextMap/42/record/7/entityIdentifier/entityType",
            LENGTH_AT_LEAST_ONE.to_string(),
        ),
        (
            // A LIST inside a map entry: the key is kept and the element is
            // renumbered, in the same pointer.
            "the second element of a set inside a context-map entry",
            "IsAuthorized",
            is_authorized(
                &store_id,
                "context",
                json!({ "contextMap": { "5": { "set": [
                    { "string": "first" },
                    entity_identifier_without_a_type(),
                ] } } }),
            ),
            "/context/contextMap/5/set/1/entityIdentifier/entityType",
            LENGTH_AT_LEAST_ONE.to_string(),
        ),
        (
            // A map inside a LIST element: the element is renumbered and the
            // attribute name is kept.
            "an all-digit attribute name on the second entity of a list",
            "IsAuthorized",
            is_authorized(
                &store_id,
                "entities",
                json!({ "entityList": [
                    entity("first"),
                    {
                        "identifier": { "entityType": "PhotoApp::Photo", "entityId": "second" },
                        "attributes": { "2024": entity_identifier_without_a_type() },
                    },
                ] }),
            ),
            "/entities/entityList/1/attributes/2024/entityIdentifier/entityType",
            LENGTH_AT_LEAST_ONE.to_string(),
        ),
        (
            // The same rule on an entity's Cedar TAG map, whose values are a
            // different union of the same shape.
            "an all-digit tag name on an entity of a list",
            "IsAuthorized",
            is_authorized(
                &store_id,
                "entities",
                json!({ "entityList": [
                    {
                        "identifier": { "entityType": "PhotoApp::Photo", "entityId": "only" },
                        "tags": { "2024": entity_identifier_without_a_type() },
                    },
                ] }),
            ),
            "/entities/entityList/0/tags/2024/entityIdentifier/entityType",
            LENGTH_AT_LEAST_ONE.to_string(),
        ),
        (
            // The other side of the rule: a plain list element, which a pointer
            // numbers from 0.
            "a member inside the third element of a list",
            "BatchIsAuthorized",
            json!({
                "policyStoreId": store_id,
                "requests": [
                    {
                        "principal": { "entityType": "PhotoApp::User", "entityId": "alice" },
                        "action": { "actionType": "PhotoApp::Action", "actionId": "viewPhoto" },
                        "resource": { "entityType": "PhotoApp::Photo", "entityId": "vacation" },
                    },
                    {
                        "principal": { "entityType": "PhotoApp::User", "entityId": "bob" },
                        "action": { "actionType": "PhotoApp::Action", "actionId": "viewPhoto" },
                        "resource": { "entityType": "PhotoApp::Photo", "entityId": "vacation" },
                    },
                    {
                        "principal": { "entityType": "PhotoApp::User", "entityId": "" },
                        "action": { "actionType": "PhotoApp::Action", "actionId": "viewPhoto" },
                        "resource": { "entityType": "PhotoApp::Photo", "entityId": "vacation" },
                    },
                ],
            }),
            "/requests/2/principal/entityId",
            LENGTH_AT_LEAST_ONE.to_string(),
        ),
    ];

    for (case, operation, request, pointer, requirement) in cases {
        let (status, body) = call(&router, operation, &request).await;
        assert_eq!(status, 400, "case {case:?}: expected HTTP 400, got {body}");
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected the ValidationException code, got {body}"
        );
        let expected = format!("1 validation error detected. {}", clause(pointer, &requirement));
        let stated = fault_message(&body);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected message {expected:?}, got {stated:?}"
        );
        // "The path to the specific element": resolved against the body that was
        // sent, the pointer must land on a member that body really has.
        assert!(
            request.pointer(pointer).is_some(),
            "case {case:?}: the reported pointer {pointer:?} does not resolve against the \
             request body that was sent"
        );
    }
}

/// RULE: a request that broke a constraint the model declares is refused 400
/// `ValidationException`, and the message is
/// `N validation error(s) detected. <clause>; …` — a PERIOD after the count, each
/// clause naming its member as a slash-delimited JSON pointer rooted at the
/// request body, and no clause echoing the value that was submitted.
///
/// The rows span the operations, the depth the offending member sits at (top
/// level, inside a union arm, inside the second element of a list), and the
/// constraint class that was broken (a pattern, a length maximum, a length
/// minimum, an absent required member).
#[tokio::test]
async fn a_boundary_refusal_addresses_the_member_as_a_pointer_and_withholds_the_value() {
    let router = router();
    let store_id = store_with_mode(&router, "OFF").await;
    let over_long_arn = user_pool_arn_of(USER_POOL_ARN_MAX_LENGTH + 1);

    // (case, operation, request, expected message, the submitted value the
    //  message must NOT echo)
    let cases: Vec<(&str, &str, Value, String, Option<&str>)> = vec![
        (
            // The recorded case: an ARN that is not a user pool ARN at all.
            "a pattern broken by a member inside a union arm",
            "CreateIdentitySource",
            json!({
                "policyStoreId": store_id,
                "configuration": { "cognitoUserPoolConfiguration": {
                    "userPoolArn": "arn:aws:s3:::not-a-user-pool-us-west-2" } },
            }),
            format!(
                "1 validation error detected. {}",
                clause(
                    "/configuration/cognitoUserPoolConfiguration/userPoolArn",
                    &format!(
                        "Member must satisfy regular expression pattern: {USER_POOL_ARN_PATTERN}"
                    ),
                )
            ),
            Some("arn:aws:s3:::not-a-user-pool-us-west-2"),
        ),
        (
            // The same rule on the OTHER arm of the same union, so the pointer
            // follows the arm the request carried rather than one arm's names.
            "a pattern broken by a member inside the other union arm",
            "CreateIdentitySource",
            json!({
                "policyStoreId": store_id,
                "principalEntityType": "PhotoApp::User",
                "configuration": { "openIdConnectConfiguration": {
                    "issuer": "http://idp.example.com",
                    "tokenSelection": { "identityTokenOnly": {} } } },
            }),
            format!(
                "1 validation error detected. {}",
                clause(
                    "/configuration/openIdConnectConfiguration/issuer",
                    &format!("Member must satisfy regular expression pattern: {ISSUER_PATTERN}"),
                )
            ),
            Some("http://idp.example.com"),
        ),
        (
            // A length MAXIMUM, on a value that satisfies the pattern: only its
            // size is wrong, so the clause states the size requirement.
            "a length maximum broken by a member inside a union arm",
            "CreateIdentitySource",
            json!({
                "policyStoreId": store_id,
                "configuration": { "cognitoUserPoolConfiguration": {
                    "userPoolArn": over_long_arn } },
            }),
            format!(
                "1 validation error detected. {}",
                clause(
                    "/configuration/cognitoUserPoolConfiguration/userPoolArn",
                    &format!(
                        "Member must have length less than or equal to {USER_POOL_ARN_MAX_LENGTH}"
                    ),
                )
            ),
            Some(over_long_arn.as_str()),
        ),
        (
            // A length MINIMUM, on a top-level member of a read operation.
            "a length minimum broken by a top-level member",
            "GetPolicy",
            json!({ "policyStoreId": "", "policyId": "SPEXAMPLE" }),
            format!(
                "1 validation error detected. {}",
                clause(
                    "/policyStoreId",
                    "Member must have length greater than or equal to 1",
                )
            ),
            None,
        ),
        (
            // An absent required member: the same envelope and the same pointer
            // form, with the requirement AWS states for absence.
            "a required top-level member absent from the request",
            "CreatePolicyStore",
            json!({}),
            format!(
                "1 validation error detected. {}",
                clause("/validationSettings", "Member must not be null")
            ),
            None,
        ),
        (
            // A member inside the SECOND element of a list: the pointer names
            // the element's own 0-based position, with no `member` step.
            "a length minimum broken inside the second element of a list",
            "BatchIsAuthorized",
            json!({
                "policyStoreId": store_id,
                "requests": [
                    {
                        "principal": { "entityType": "PhotoApp::User", "entityId": "alice" },
                        "action": { "actionType": "PhotoApp::Action", "actionId": "viewPhoto" },
                        "resource": { "entityType": "PhotoApp::Photo", "entityId": "vacation" },
                    },
                    {
                        "principal": { "entityType": "PhotoApp::User", "entityId": "" },
                        "action": { "actionType": "PhotoApp::Action", "actionId": "viewPhoto" },
                        "resource": { "entityType": "PhotoApp::Photo", "entityId": "vacation" },
                    },
                ],
            }),
            format!(
                "1 validation error detected. {}",
                clause(
                    "/requests/1/principal/entityId",
                    "Member must have length greater than or equal to 1",
                )
            ),
            None,
        ),
    ];

    for (case, operation, request, expected, withheld) in cases {
        let (status, body) = call(&router, operation, &request).await;
        assert_eq!(status, 400, "case {case:?}: expected HTTP 400, got {body}");
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected the ValidationException code, got {body}"
        );
        let stated = fault_message(&body);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected message {expected:?}, got {stated:?}"
        );
        if let Some(value) = withheld {
            assert!(
                !stated.contains(value),
                "case {case:?}: expected the submitted value to be withheld, got {stated:?}"
            );
        }
    }
}

/// RULE (the same one, on a request that broke TWO constraints): the count is
/// pluralised and still followed by a period, and the message carries one clause
/// per violation, each addressing its own member as a pointer.
#[tokio::test]
async fn a_boundary_refusal_states_one_clause_per_broken_constraint() {
    let router = router();
    let (status, body) = call(
        &router,
        "GetPolicy",
        &json!({ "policyStoreId": "", "policyId": "" }),
    )
    .await;
    assert_eq!(status, 400, "expected HTTP 400, got {body}");
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "expected the ValidationException code, got {body}"
    );
    let stated = fault_message(&body);
    assert!(
        stated.starts_with("2 validation errors detected. "),
        "expected the plural envelope terminated with a period, got {stated:?}"
    );
    for pointer in ["/policyStoreId", "/policyId"] {
        let expected = clause(pointer, "Member must have length greater than or equal to 1");
        assert!(
            stated.contains(&expected),
            "expected a clause {expected:?} for {pointer}, got {stated:?}"
        );
    }
    assert!(
        stated.contains("; "),
        "expected the two clauses separated by '; ', got {stated:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. Every reason the schema raised
// ---------------------------------------------------------------------------

/// RULE: a policy the store's SCHEMA refuses in `STRICT` mode is answered with
/// ONE `fieldList` entry per diagnostic the Cedar validator raised about it —
/// every one of them, not only the first — each naming the request member whose
/// content was validated, and a policy the validator raises NOTHING about is
/// stored.
///
/// The rows span the classes of diagnostic the service's own
/// `ValidationException` documentation enumerates (an action id the schema does
/// not declare, entity types it does not declare, an action applied to a
/// principal and resource the schema does not sanction, a condition that can
/// never hold), the operation that validates (policy create, policy update,
/// template create), and HOW MANY diagnostics one policy raises — which is what
/// pins "every reason" rather than "one".
#[tokio::test]
async fn a_strict_schema_refusal_reports_every_reason_cedar_raised() {
    let router = router();
    let store_id = store_with_mode(&router, "STRICT").await;
    ok(
        &router,
        "PutSchema",
        &json!({
            "policyStoreId": store_id,
            "definition": { "cedarJson": photo_app_schema() },
        }),
    )
    .await;

    // A policy the schema sanctions, so the UpdatePolicy row has something to
    // replace — and proof that a policy the validator raises nothing about is
    // stored rather than refused.
    let created = ok(
        &router,
        "CreatePolicy",
        &json!({
            "policyStoreId": store_id,
            "definition": { "static": { "statement":
                "permit(principal == PhotoApp::User::\"alice\", \
                 action == PhotoApp::Action::\"viewPhoto\", \
                 resource == PhotoApp::Photo::\"vacation.jpg\");" } },
        }),
    )
    .await;
    let policy_id = created["policyId"].as_str().unwrap_or_default().to_string();

    // (case, operation, request, the validated member, the fragment each
    //  reported reason must state, in order)
    let cases: Vec<(&str, &str, Value, &str, Vec<&str>)> = vec![
        (
            // The recorded case: Cedar reports BOTH the action id it does not
            // know and the scope that therefore applies to nothing.
            "an action id the schema does not declare",
            "CreatePolicy",
            json!({
                "policyStoreId": store_id,
                "definition": { "static": { "statement":
                    "permit(principal == PhotoApp::User::\"alice\", \
                     action == PhotoApp::Action::\"deletePhoto\", \
                     resource == PhotoApp::Photo::\"vacation.jpg\");" } },
            }),
            "/definition",
            vec![
                "unrecognized action `PhotoApp::Action::\"deletePhoto\"`",
                "unable to find an applicable action given the policy scope constraints",
            ],
        ),
        (
            // Two undeclared entity types raise one reason each, and the scope
            // they leave applies to nothing: three reasons, three entries.
            "two entity types the schema does not declare",
            "CreatePolicy",
            json!({
                "policyStoreId": store_id,
                "definition": { "static": { "statement":
                    "permit(principal == PhotoApp::Ghost::\"g\", \
                     action == PhotoApp::Action::\"viewPhoto\", \
                     resource == PhotoApp::Album::\"a\");" } },
            }),
            "/definition",
            vec![
                "unrecognized entity type `PhotoApp::Ghost`",
                "unrecognized entity type `PhotoApp::Album`",
                "unable to find an applicable action given the policy scope constraints",
            ],
        ),
        (
            // An action the schema declares, applied to a principal and a
            // resource it does not sanction for that action.
            "an action applied to a principal and resource the schema does not sanction",
            "CreatePolicy",
            json!({
                "policyStoreId": store_id,
                "definition": { "static": { "statement":
                    "permit(principal == PhotoApp::Photo::\"vacation.jpg\", \
                     action == PhotoApp::Action::\"viewPhoto\", \
                     resource == PhotoApp::User::\"alice\");" } },
            }),
            "/definition",
            vec!["unable to find an applicable action given the policy scope constraints"],
        ),
        (
            // A condition that can never hold: the policy could never affect a
            // decision, which is its own reason.
            "a condition that always evaluates to false",
            "CreatePolicy",
            json!({
                "policyStoreId": store_id,
                "definition": { "static": { "statement":
                    "permit(principal == PhotoApp::User::\"alice\", \
                     action == PhotoApp::Action::\"viewPhoto\", \
                     resource == PhotoApp::Photo::\"vacation.jpg\") \
                     when { 1 == 2 };" } },
            }),
            "/definition",
            vec!["policy is impossible"],
        ),
        (
            // The same rule on the UPDATE path: only the action changes, so the
            // schema is what refuses the replacement.
            "an update whose action the schema does not declare",
            "UpdatePolicy",
            json!({
                "policyStoreId": store_id,
                "policyId": policy_id,
                "definition": { "static": { "statement":
                    "permit(principal == PhotoApp::User::\"alice\", \
                     action == PhotoApp::Action::\"deletePhoto\", \
                     resource == PhotoApp::Photo::\"vacation.jpg\");" } },
            }),
            "/definition",
            vec![
                "unrecognized action `PhotoApp::Action::\"deletePhoto\"`",
                "unable to find an applicable action given the policy scope constraints",
            ],
        ),
        (
            // And on a policy TEMPLATE, which the same validation covers — at
            // the member whose content a template operation validates.
            "a template whose action the schema does not declare",
            "CreatePolicyTemplate",
            json!({
                "policyStoreId": store_id,
                "statement": "permit(principal == ?principal, \
                              action == PhotoApp::Action::\"deletePhoto\", \
                              resource == PhotoApp::Photo::\"vacation.jpg\");",
            }),
            "/template/statement",
            vec![
                "unrecognized action `PhotoApp::Action::\"deletePhoto\"`",
                "unable to find an applicable action given the policy scope constraints",
            ],
        ),
    ];

    for (case, operation, request, path, fragments) in cases {
        let (status, body) = call(&router, operation, &request).await;
        assert_eq!(status, 400, "case {case:?}: expected HTTP 400, got {body}");
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected the ValidationException code, got {body}"
        );
        assert_eq!(
            fault_message(&body),
            INVALID_INPUT_MESSAGE,
            "case {case:?}: expected the fixed summary message, got {body}"
        );
        let fields = field_list(&body);
        assert_eq!(
            fields.len(),
            fragments.len(),
            "case {case:?}: expected one fieldList entry per reason ({}), got {fields:?}",
            fragments.len()
        );
        for (index, fragment) in fragments.iter().enumerate() {
            let (stated_path, stated_message) = &fields[index];
            assert_eq!(
                stated_path, path,
                "case {case:?}: expected entry {index} at {path:?}, got {fields:?}"
            );
            assert!(
                stated_message.contains(fragment),
                "case {case:?}: expected entry {index} to state {fragment:?}, got {fields:?}"
            );
        }
    }
}
