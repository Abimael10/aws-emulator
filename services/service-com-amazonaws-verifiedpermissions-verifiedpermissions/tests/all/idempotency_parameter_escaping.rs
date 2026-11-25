// SPDX-License-Identifier: CC0-1.0

//! What an `@idempotencyToken` create does with a token it has already seen:
//! the SAME parameters replay the first call's resource, and ANY different
//! parameters are the modelled 409 `ConflictException` — whatever characters
//! those parameters carry.
//!
//! The rendering a create compares the two calls by is a canonical one in which
//! every caller-supplied value is quoted and escaped, so a value carrying the
//! `=` or `;` characters that rendering uses as its own separators cannot make
//! two different requests look like one call. The rows below are exactly such
//! requests: pairs that differ only in WHERE a separator-carrying value's
//! boundaries fall, which a rendering that interpolated its values plainly would
//! collapse into one — answering the second request as a replay, so the resource
//! it asked for was never created.
//!
//! The same token also decides WHEN that comparison happens: the parameters are
//! rendered from the request's own members, so the replay is settled before ANY
//! refusal of the request's own payload — a token already recorded against
//! different parameters is the 409 however badly the rest of the request is
//! formed
//! ([`a_client_token_replay_outranks_a_refusal_of_the_requests_own_payload`],
//! [`a_reused_identity_source_token_conflicts_however_its_payload_would_be_refused`]),
//! and a token recorded against these parameters is the idempotent success even
//! once the payload stopped resolving
//! ([`an_identity_source_replay_answers_with_the_first_call_even_once_its_pool_is_gone`]).
//!
//! Every assertion goes over the wire through the shared [`super::harness`], so a
//! resource is only visible if the create really reached state.

use serde_json::{json, Value};
use service_com_amazonaws_verifiedpermissions_verifiedpermissions::state::cedar::MAX_STATEMENT_BYTES;
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{
    call, cognito_provider, delete_user_pool, error_code, field_list, ok, router,
    store_with_validation_off as store, user_pool, user_pool_arn, SIGNING_REGION,
};

/// A Cognito user pool this emulator can resolve: one freshly created in the
/// region the harness signs for, so the pool the rows reference really exists
/// — an identity source can only reference a pool the user-pool service holds.
async fn pool_arn(router: &MultiServiceRouter) -> String {
    user_pool_arn(
        SIGNING_REGION,
        &super::harness::user_pool(router, "escaping-pool").await,
    )
}

/// A Cedar static policy every policy row below carries, so a row differs from
/// its partner only in the member it is about.
const STATEMENT: &str = "permit(principal, action, resource);";

/// A Cedar policy TEMPLATE statement, which has to carry a placeholder.
const TEMPLATE_STATEMENT: &str = "permit(principal == ?principal, action, resource);";

/// The `identitySourceId`s a store holds, in the order they are listed.
async fn listed_source_ids(router: &MultiServiceRouter, store_id: &str) -> Vec<String> {
    let listed = ok(
        router,
        "ListIdentitySources",
        &json!({ "policyStoreId": store_id }),
    )
    .await;
    listed["identitySources"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            item["identitySourceId"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Two `CreateIdentitySource` requests that are genuinely different, and what a
/// read of the SECOND one's own identity source must report once it is created
/// under a token of its own.
struct SourcePair {
    case: &'static str,
    /// Request members beside `policyStoreId` and `clientToken`.
    first: Value,
    second: Value,
    /// `(JSON pointer into the GetIdentitySource response, expected value)` for
    /// the second request's identity source.
    second_reads: Vec<(&'static str, Value)>,
}

// ---------------------------------------------------------------------------
// CreateIdentitySource
// ---------------------------------------------------------------------------

/// Two different `CreateIdentitySource` requests under one `clientToken`
/// conflict, the first one's identity source is untouched, and each request
/// creates its own identity source under a token of its own.
///
/// The rows differ along WHICH member carries the separators and across both
/// arms of the `Configuration` union and both arms of
/// `OpenIdConnectTokenSelection`. The members used are the ones whose modelled
/// types admit those characters — `PrincipalEntityType` (`^.*$`), `Issuer`
/// (`^https://.*$`), `Claim`, `EntityIdPrefix` (no pattern) and `ClientId`
/// (`^.*$`), from smithy_query(query="shape",
/// shape="PrincipalEntityType,Issuer,Claim,EntityIdPrefix,ClientId") — so every
/// row is legal input.
#[tokio::test]
async fn two_different_identity_source_requests_under_one_client_token_conflict() {
    let router = router();
    let arn = pool_arn(&router).await;
    let pairs: Vec<SourcePair> = vec![
        SourcePair {
            // The separators move the boundary between `principalEntityType`
            // and the configuration that follows it.
            case: "oidc-issuer-and-principal-entity-type-swap-the-boundary",
            first: json!({
                "principalEntityType": "MyCorp::User",
                "configuration": {
                    "openIdConnectConfiguration": {
                        "issuer": "https://first.example.com;configuration=oidc:issuer=https://second.example.com",
                        "tokenSelection": { "identityTokenOnly": { "principalIdClaim": "sub" } },
                    }
                },
            }),
            second: json!({
                "principalEntityType": "MyCorp::User;configuration=oidc:issuer=https://first.example.com",
                "configuration": {
                    "openIdConnectConfiguration": {
                        "issuer": "https://second.example.com",
                        "tokenSelection": { "identityTokenOnly": { "principalIdClaim": "sub" } },
                    }
                },
            }),
            second_reads: vec![
                (
                    "/principalEntityType",
                    json!("MyCorp::User;configuration=oidc:issuer=https://first.example.com"),
                ),
                (
                    "/configuration/openIdConnectConfiguration/issuer",
                    json!("https://second.example.com"),
                ),
            ],
        },
        SourcePair {
            // The separators move the boundary between the issuer and the
            // claim the token selection names.
            case: "oidc-issuer-and-principal-id-claim-swap-the-boundary",
            first: json!({
                "principalEntityType": "MyCorp::Person",
                "configuration": {
                    "openIdConnectConfiguration": {
                        "issuer": "https://idp.example.org;principalIdClaim=sub",
                        "tokenSelection": { "identityTokenOnly": { "principalIdClaim": "email" } },
                    }
                },
            }),
            second: json!({
                "principalEntityType": "MyCorp::Person",
                "configuration": {
                    "openIdConnectConfiguration": {
                        "issuer": "https://idp.example.org",
                        "tokenSelection": {
                            "identityTokenOnly": { "principalIdClaim": "sub;principalIdClaim=email" }
                        },
                    }
                },
            }),
            second_reads: vec![
                (
                    "/configuration/openIdConnectConfiguration/issuer",
                    json!("https://idp.example.org"),
                ),
                (
                    "/configuration/openIdConnectConfiguration/tokenSelection/identityTokenOnly/principalIdClaim",
                    json!("sub;principalIdClaim=email"),
                ),
            ],
        },
        SourcePair {
            // The separators move the boundary between the issuer and the
            // optional entity-id prefix: one request sets the prefix, the other
            // spells it inside its issuer and sets none.
            case: "oidc-entity-id-prefix-present-or-spelled-in-the-issuer",
            first: json!({
                "principalEntityType": "MyCorp::Person",
                "configuration": {
                    "openIdConnectConfiguration": {
                        "issuer": "https://prefix.example.org",
                        "entityIdPrefix": "MyOIDCProvider",
                        "tokenSelection": { "identityTokenOnly": { "principalIdClaim": "sub" } },
                    }
                },
            }),
            second: json!({
                "principalEntityType": "MyCorp::Person",
                "configuration": {
                    "openIdConnectConfiguration": {
                        "issuer": "https://prefix.example.org;entityIdPrefix=MyOIDCProvider",
                        "tokenSelection": { "identityTokenOnly": { "principalIdClaim": "sub" } },
                    }
                },
            }),
            second_reads: vec![
                (
                    "/configuration/openIdConnectConfiguration/issuer",
                    json!("https://prefix.example.org;entityIdPrefix=MyOIDCProvider"),
                ),
                (
                    "/configuration/openIdConnectConfiguration/tokenSelection/identityTokenOnly/principalIdClaim",
                    json!("sub"),
                ),
            ],
        },
        SourcePair {
            // A LIST member: the same characters, split across one element or
            // two, is a different set of accepted client ids.
            case: "oidc-client-ids-split-across-one-element-or-two",
            first: json!({
                "principalEntityType": "MyCorp::Person",
                "configuration": {
                    "openIdConnectConfiguration": {
                        "issuer": "https://clients.example.org",
                        "tokenSelection": {
                            "identityTokenOnly": {
                                "principalIdClaim": "sub",
                                "clientIds": ["first;second"],
                            }
                        },
                    }
                },
            }),
            second: json!({
                "principalEntityType": "MyCorp::Person",
                "configuration": {
                    "openIdConnectConfiguration": {
                        "issuer": "https://clients.example.org",
                        "tokenSelection": {
                            "identityTokenOnly": {
                                "principalIdClaim": "sub",
                                "clientIds": ["first", "second"],
                            }
                        },
                    }
                },
            }),
            second_reads: vec![(
                "/configuration/openIdConnectConfiguration/tokenSelection/identityTokenOnly/clientIds",
                json!(["first", "second"]),
            )],
        },
        SourcePair {
            // The other arm of the token selection, with its own list member.
            case: "oidc-audiences-split-across-one-element-or-two",
            first: json!({
                "principalEntityType": "MyCorp::Person",
                "configuration": {
                    "openIdConnectConfiguration": {
                        "issuer": "https://audiences.example.org",
                        "tokenSelection": {
                            "accessTokenOnly": {
                                "principalIdClaim": "sub",
                                "audiences": ["https://a.example.com;https://b.example.com"],
                            }
                        },
                    }
                },
            }),
            second: json!({
                "principalEntityType": "MyCorp::Person",
                "configuration": {
                    "openIdConnectConfiguration": {
                        "issuer": "https://audiences.example.org",
                        "tokenSelection": {
                            "accessTokenOnly": {
                                "principalIdClaim": "sub",
                                "audiences": ["https://a.example.com", "https://b.example.com"],
                            }
                        },
                    }
                },
            }),
            second_reads: vec![(
                "/configuration/openIdConnectConfiguration/tokenSelection/accessTokenOnly/audiences",
                json!(["https://a.example.com", "https://b.example.com"]),
            )],
        },
        SourcePair {
            // The other arm of the `Configuration` union, whose client ids are a
            // list too.
            case: "cognito-client-ids-split-across-one-element-or-two",
            first: json!({
                "principalEntityType": "MyCorp::User",
                "configuration": {
                    "cognitoUserPoolConfiguration": {
                        "userPoolArn": arn,
                        "clientIds": ["first;second"],
                    }
                },
            }),
            second: json!({
                "principalEntityType": "MyCorp::User",
                "configuration": {
                    "cognitoUserPoolConfiguration": {
                        "userPoolArn": arn,
                        "clientIds": ["first", "second"],
                    }
                },
            }),
            second_reads: vec![(
                "/configuration/cognitoUserPoolConfiguration/clientIds",
                json!(["first", "second"]),
            )],
        },
        SourcePair {
            // A Cognito source whose `principalEntityType` spells out the
            // configuration that follows it.
            case: "cognito-principal-entity-type-spells-the-configuration",
            first: json!({
                "principalEntityType": "MyCorp::User",
                "configuration": { "cognitoUserPoolConfiguration": { "userPoolArn": arn } },
            }),
            second: json!({
                "principalEntityType": format!("MyCorp::User;configuration=cognito:userPoolArn={arn}"),
                "configuration": { "cognitoUserPoolConfiguration": { "userPoolArn": arn } },
            }),
            second_reads: vec![(
                "/principalEntityType",
                json!(format!("MyCorp::User;configuration=cognito:userPoolArn={arn}")),
            )],
        },
    ];

    for (index, pair) in pairs.iter().enumerate() {
        let case = pair.case;
        // One store per row, so what a row's store holds is only what that row
        // created.
        let store_id = store(&router).await;
        let token = format!("token-{index}");

        let mut first = pair.first.clone();
        first["policyStoreId"] = json!(store_id);
        first["clientToken"] = json!(token);
        let created = ok(&router, "CreateIdentitySource", &first).await;
        let created_id = created["identitySourceId"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        // The same request under the same token is a replay, not a second create.
        let replay = ok(&router, "CreateIdentitySource", &first).await;
        assert_eq!(
            replay, created,
            "case {case:?}: the same parameters must replay the first result {created}, got {replay}"
        );

        // The partner request under that token is a different request.
        let mut second = pair.second.clone();
        second["policyStoreId"] = json!(store_id);
        second["clientToken"] = json!(token);
        let (status, body) = call(&router, "CreateIdentitySource", &second).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected HTTP 409 for a reused token with different parameters, \
             got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ConflictException",
            "case {case:?}: expected a ConflictException, got {body}"
        );
        assert_eq!(
            body["resources"],
            json!([{ "resourceId": created_id, "resourceType": "IDENTITY_SOURCE" }]),
            "case {case:?}: expected the conflict to name {created_id}, got {:?}",
            body["resources"]
        );
        assert_eq!(
            listed_source_ids(&router, &store_id).await,
            vec![created_id.clone()],
            "case {case:?}: the refused create must have persisted nothing, got {:?}",
            listed_source_ids(&router, &store_id).await
        );

        // Under a token of its own the second request is served, and what it
        // asked for is what state holds.
        second["clientToken"] = json!(format!("other-{index}"));
        let other = ok(&router, "CreateIdentitySource", &second).await;
        let other_id = other["identitySourceId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_ne!(
            other_id, created_id,
            "case {case:?}: expected a distinct identity source, got {other}"
        );
        let got = ok(
            &router,
            "GetIdentitySource",
            &json!({ "policyStoreId": store_id, "identitySourceId": other_id }),
        )
        .await;
        for (pointer, expected) in &pair.second_reads {
            assert_eq!(
                got.pointer(pointer),
                Some(expected),
                "case {case:?}: expected {pointer} to read back as {expected}, got {got}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The other creates that carry an @idempotencyToken
// ---------------------------------------------------------------------------

/// The same rule holds for every create in this service that carries an
/// `@idempotencyToken`: the rows are one pair per create, differing only in a
/// value that carries the rendering's own separators.
#[tokio::test]
async fn two_different_requests_under_one_client_token_conflict_in_every_create() {
    let router = router();
    let store_id = store(&router).await;

    // (case, operation, the member of the response that names the created
    //  resource, first request, second request)
    let pairs: Vec<(&str, &str, &str, Value, Value)> = vec![
        (
            // `PolicyStoreDescription` carries no pattern, so a description may
            // spell out the members that follow it.
            "policy-store-description-spells-the-following-members",
            "CreatePolicyStore",
            "policyStoreId",
            json!({
                "validationSettings": { "mode": "OFF" },
                "description": "shared;deletionProtection=\"ENABLED\"",
            }),
            json!({
                "validationSettings": { "mode": "OFF" },
                "description": "shared",
                "deletionProtection": "ENABLED",
            }),
        ),
        (
            // `TagKey` and `TagValue` carry no pattern either, so the same
            // characters split across a key or a value is a different tag set.
            "policy-store-tags-split-across-the-key-or-the-value",
            "CreatePolicyStore",
            "policyStoreId",
            json!({
                "validationSettings": { "mode": "OFF" },
                "tags": { "team;env": "prod" },
            }),
            json!({
                "validationSettings": { "mode": "OFF" },
                "tags": { "team": "env;prod" },
            }),
        ),
        (
            "policy-description-spells-the-following-members",
            "CreatePolicy",
            "policyId",
            json!({
                "policyStoreId": store_id,
                "definition": {
                    "static": {
                        "statement": STATEMENT,
                        "description": "shared;statement=",
                    }
                },
            }),
            json!({
                "policyStoreId": store_id,
                "definition": { "static": { "statement": STATEMENT, "description": "shared" } },
            }),
        ),
        (
            "policy-template-description-spells-the-following-members",
            "CreatePolicyTemplate",
            "policyTemplateId",
            json!({
                "policyStoreId": store_id,
                "statement": TEMPLATE_STATEMENT,
                "name": "template-a",
                "description": "shared;name=template-b",
            }),
            json!({
                "policyStoreId": store_id,
                "statement": TEMPLATE_STATEMENT,
                "name": "template-b",
                "description": "shared",
            }),
        ),
    ];

    for (index, (case, operation, id_member, first, second)) in pairs.iter().enumerate() {
        let token = format!("shared-token-{index}");
        let mut first = first.clone();
        first["clientToken"] = json!(token);
        let created = ok(&router, operation, &first).await;
        let created_id = created[*id_member]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !created_id.is_empty(),
            "case {case:?}: expected {operation} to report a {id_member}, got {created}"
        );

        let replay = ok(&router, operation, &first).await;
        assert_eq!(
            replay, created,
            "case {case:?}: the same parameters must replay the first result {created}, got {replay}"
        );

        let mut second = second.clone();
        second["clientToken"] = json!(token);
        let (status, body) = call(&router, operation, &second).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected HTTP 409 for a reused token with different parameters, \
             got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ConflictException",
            "case {case:?}: expected a ConflictException, got {body}"
        );
        assert_eq!(
            body["resources"][0]["resourceId"],
            json!(created_id),
            "case {case:?}: expected the conflict to name {created_id}, got {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// WHEN the replay is decided: before the request's own payload is judged
// ---------------------------------------------------------------------------

/// The published *Tags per resource* quota, which the model states twice: in
/// `TagResource`'s documentation ("You can associate as many as 50 tags with a
/// resource") and in `TooManyTagsException`'s ("the limit (50)").
///
/// Written out here rather than imported on purpose: the rows pin the PUBLISHED
/// quota, so a change to the implementation constant has to fail this test
/// instead of moving with it.
const MAX_TAGS_PER_RESOURCE: usize = 50;

/// `TagMap`'s modelled length maximum, from `smithy_query(query="operation",
/// operation="CreatePolicyStore")`: `tags` is a `TagMap` of length ..200. It
/// bounds ONE request's map, so a map this large is legal input that the
/// per-resource quota still refuses.
const MAX_TAGS_PER_REQUEST: usize = 200;

/// A tag map of `count` distinct keys.
fn tag_map(count: usize) -> Value {
    Value::Object(
        (0..count)
            .map(|index| (format!("key{index:03}"), json!("v")))
            .collect(),
    )
}

/// A statement whose text is longer than [`MAX_STATEMENT_BYTES`], reached by
/// padding a string literal inside a condition, so the only thing wrong with it
/// is its size.
fn over_long_statement() -> String {
    format!(
        "permit(principal, action, resource) when {{ context.tag == \"{}\" }};",
        "x".repeat(MAX_STATEMENT_BYTES + 1)
    )
}

/// One create that carries an `@idempotencyToken`, paired with a request this
/// service refuses on its OWN payload checks.
struct PayloadFault {
    case: &'static str,
    operation: &'static str,
    /// The member of the create's response that names the resource it made.
    id_member: &'static str,
    /// The `resourceType` the `ConflictException` reports for that resource.
    resource_type: &'static str,
    /// A request this operation serves, against the supplied `policyStoreId`.
    accepted: fn(&str) -> Value,
    /// A request whose payload this operation refuses, against the same store.
    refused: fn(&str) -> Value,
    /// The member `ValidationException$fieldList` names for `refused`.
    field: &'static str,
    /// The list operation that counts the resources this create produces, its
    /// request body and the output member carrying the list.
    list: (&'static str, fn(&str) -> Value, &'static str),
}

/// Every create in this crate that carries an `@idempotencyToken` and also has a
/// payload refusal of its own to run.
///
/// The rows span the three such operations and, within each, the KIND of payload
/// fault — a map larger than one resource may hold, Cedar text the parser cannot
/// read, a placeholder Cedar admits nowhere, and text past the size bound — so
/// what they share is only that the request's own payload is unservable.
///
/// `CreateIdentitySource` is absent from THIS table only because both of its
/// payload refusals need a user pool seeded through another service, which a
/// `const` row cannot do. The same rule is driven for it, over the same steps, by
/// [`a_reused_identity_source_token_conflicts_however_its_payload_would_be_refused`]
/// and
/// [`an_identity_source_replay_answers_with_the_first_call_even_once_its_pool_is_gone`].
const PAYLOAD_FAULTS: &[PayloadFault] = &[
    PayloadFault {
        case: "a create-time tag map one past the per-resource quota",
        operation: "CreatePolicyStore",
        id_member: "policyStoreId",
        resource_type: "POLICY_STORE",
        accepted: |_| json!({ "validationSettings": { "mode": "OFF" }, "tags": { "k": "v" } }),
        refused: |_| {
            json!({
                "validationSettings": { "mode": "OFF" },
                "tags": tag_map(MAX_TAGS_PER_RESOURCE + 1),
            })
        },
        field: "/tags",
        list: ("ListPolicyStores", |_| json!({}), "policyStores"),
    },
    PayloadFault {
        case: "a create-time tag map at the modelled per-request maximum",
        operation: "CreatePolicyStore",
        id_member: "policyStoreId",
        resource_type: "POLICY_STORE",
        accepted: |_| json!({ "validationSettings": { "mode": "OFF" } }),
        refused: |_| {
            json!({
                "validationSettings": { "mode": "OFF" },
                "tags": tag_map(MAX_TAGS_PER_REQUEST),
            })
        },
        field: "/tags",
        list: ("ListPolicyStores", |_| json!({}), "policyStores"),
    },
    PayloadFault {
        case: "a static policy whose statement is not Cedar at all",
        operation: "CreatePolicy",
        id_member: "policyId",
        resource_type: "POLICY",
        accepted: |store| {
            json!({
                "policyStoreId": store,
                "definition": { "static": { "statement": STATEMENT } },
            })
        },
        refused: |store| {
            json!({
                "policyStoreId": store,
                "definition": { "static": { "statement": "this is not cedar" } },
            })
        },
        field: "/definition/static/statement",
        list: (
            "ListPolicies",
            |store| json!({ "policyStoreId": store }),
            "policies",
        ),
    },
    PayloadFault {
        case: "a static policy whose statement is past the size bound",
        operation: "CreatePolicy",
        id_member: "policyId",
        resource_type: "POLICY",
        accepted: |store| {
            json!({
                "policyStoreId": store,
                "definition": { "static": { "statement": STATEMENT } },
            })
        },
        refused: |store| {
            json!({
                "policyStoreId": store,
                "definition": { "static": { "statement": over_long_statement() } },
            })
        },
        field: "/definition/static/statement",
        list: (
            "ListPolicies",
            |store| json!({ "policyStoreId": store }),
            "policies",
        ),
    },
    PayloadFault {
        case: "a policy template whose statement is not Cedar at all",
        operation: "CreatePolicyTemplate",
        id_member: "policyTemplateId",
        resource_type: "POLICY_TEMPLATE",
        accepted: |store| json!({ "policyStoreId": store, "statement": TEMPLATE_STATEMENT }),
        refused: |store| json!({ "policyStoreId": store, "statement": "this is not cedar" }),
        field: "/template/statement",
        list: (
            "ListPolicyTemplates",
            |store| json!({ "policyStoreId": store }),
            "policyTemplates",
        ),
    },
    PayloadFault {
        case: "a policy template with a placeholder Cedar admits nowhere",
        operation: "CreatePolicyTemplate",
        id_member: "policyTemplateId",
        resource_type: "POLICY_TEMPLATE",
        accepted: |store| json!({ "policyStoreId": store, "statement": TEMPLATE_STATEMENT }),
        refused: |store| {
            json!({
                "policyStoreId": store,
                "statement": "permit(principal, action == ?action, resource);",
            })
        },
        field: "/template/statement",
        list: (
            "ListPolicyTemplates",
            |store| json!({ "policyStoreId": store }),
            "policyTemplates",
        ),
    },
];

/// How many resources a row's list operation reports.
async fn listed_count(router: &MultiServiceRouter, row: &PayloadFault, store_id: &str) -> usize {
    let (operation, body, member) = row.list;
    let listed = ok(router, operation, &body(store_id)).await;
    listed[member].as_array().map_or(0, Vec::len)
}

/// RULE: every create in this crate that carries an `@idempotencyToken` settles
/// the token replay BEFORE it judges the request's own payload, so a token
/// already recorded against different parameters answers the modelled 409
/// `ConflictException` naming the resource that token created — whatever the
/// request's payload would otherwise be refused for — and the refused payload
/// still earns its 400 `ValidationException`, naming the member at fault, under
/// a token the service has never seen.
#[tokio::test]
async fn a_client_token_replay_outranks_a_refusal_of_the_requests_own_payload() {
    for row in PAYLOAD_FAULTS {
        let case = row.case;
        // One engine per row, so the counts below are of what this row created.
        let router = router();
        let store_id = store(&router).await;
        let token = "a1b2c3d4";

        // The control: under a token the service has never seen, this payload is
        // refused on its own merits, 400 naming the member at fault. What the
        // rows pin is therefore the PRECEDENCE, not a blanket 409.
        let (status, body) = call(&router, row.operation, &(row.refused)(&store_id)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 under an unseen token, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected a ValidationException under an unseen token, got {body}"
        );
        let paths: Vec<String> = field_list(&body).into_iter().map(|(path, _)| path).collect();
        assert!(
            paths.iter().any(|path| path == row.field),
            "case {case:?}: expected fieldList to name {}, got {paths:?} in {body}",
            row.field
        );

        // The token is now recorded against a request the service served.
        let created = ok(
            &router,
            row.operation,
            &{
                let mut request = (row.accepted)(&store_id);
                request["clientToken"] = json!(token);
                request
            },
        )
        .await;
        let created_id = created[row.id_member]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !created_id.is_empty(),
            "case {case:?}: expected {} to report a {}, got {created}",
            row.operation,
            row.id_member
        );
        let after_create = listed_count(&router, row, &store_id).await;

        // That same token, replayed with the unservable payload: the replay is
        // decided first, so the answer is the conflict naming the resource the
        // token already created — not the payload's 400.
        let mut replay = (row.refused)(&store_id);
        replay["clientToken"] = json!(token);
        let (status, body) = call(&router, row.operation, &replay).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected HTTP 409 for the reused token, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ConflictException",
            "case {case:?}: expected a ConflictException for the reused token, got {body}"
        );
        assert_eq!(
            body["resources"],
            json!([{ "resourceId": created_id, "resourceType": row.resource_type }]),
            "case {case:?}: expected the conflict to name {created_id} as a {}, got {body}",
            row.resource_type
        );

        // Neither answer may have reached state: the token still replays the
        // resource it was recorded against, and nothing else was created.
        let mut again = (row.accepted)(&store_id);
        again["clientToken"] = json!(token);
        let replayed = ok(&router, row.operation, &again).await;
        assert_eq!(
            replayed, created,
            "case {case:?}: the token must still replay {created}, got {replayed}"
        );
        let after_conflict = listed_count(&router, row, &store_id).await;
        assert_eq!(
            after_conflict, after_create,
            "case {case:?}: expected {after_create} resources after the conflict, got \
             {after_conflict}"
        );
    }
}

// ---------------------------------------------------------------------------
// CreateIdentitySource, whose payload refusals read live state
// ---------------------------------------------------------------------------

/// The member `ValidationException$fieldList` names when the Cognito user pool a
/// configuration references cannot be resolved, WITHIN
/// `CreateIdentitySourceInput`'s own provider member.
const POOL_ARN_FIELD: &str = "/configuration/cognitoUserPoolConfiguration/userPoolArn";

/// The member `ValidationException$fieldList` names when there is no
/// `principalEntityType` for the provider the request describes.
const PRINCIPAL_ENTITY_TYPE_FIELD: &str = "/principalEntityType";

/// The entity type the accepted identity-source requests below name.
const SOURCE_ENTITY_TYPE: &str = "MyCorp::User";

/// A `CreateIdentitySource` body: the row's members, plus the store and the
/// token under test.
fn source_request(store_id: &str, token: &str, members: &Value) -> Value {
    let mut request = members.clone();
    request["policyStoreId"] = json!(store_id);
    request["clientToken"] = json!(token);
    request
}

/// The `identitySourceId`s a store holds, and what one of them reads back as.
async fn read_source(router: &MultiServiceRouter, store_id: &str, source_id: &str) -> Value {
    ok(
        router,
        "GetIdentitySource",
        &json!({ "policyStoreId": store_id, "identitySourceId": source_id }),
    )
    .await
}

/// One recorded `CreateIdentitySource` call whose payload stops resolving after
/// it was served, because the user pool it references is deleted.
struct VanishedPool {
    case: &'static str,
    /// The request members beside `policyStoreId` and `clientToken`, built from
    /// the ARN of the pool the row creates.
    members: fn(&str) -> Value,
}

/// RULE: a create that carries an `@idempotencyToken` settles the token replay
/// before it judges the request's own payload, so a token replayed with the SAME
/// parameters answers with the identity source the first call created even once
/// that payload no longer resolves — the documented idempotent success, not the
/// 400 the payload would now earn.
///
/// The rows differ along WHICH members the recorded call carried — none of the
/// optional ones, an entity type of the caller's own, a client-id list and a
/// group configuration — so what they share is only that a served request's
/// payload stopped resolving after the service recorded its token.
#[tokio::test]
async fn an_identity_source_replay_answers_with_the_first_call_even_once_its_pool_is_gone() {
    let rows: &[VanishedPool] = &[
        VanishedPool {
            // Every optional member omitted, so the entity type and the group
            // entity type a read reports are the service's own defaults.
            case: "a create that named only the pool",
            members: |arn| json!({ "configuration": cognito_provider(arn) }),
        },
        VanishedPool {
            case: "a create that named an entity type of its own",
            members: |arn| {
                json!({
                    "configuration": cognito_provider(arn),
                    "principalEntityType": SOURCE_ENTITY_TYPE,
                })
            },
        },
        VanishedPool {
            case: "a create that named a client-id list and a group configuration",
            members: |arn| {
                json!({
                    "configuration": {
                        "cognitoUserPoolConfiguration": {
                            "userPoolArn": arn,
                            "clientIds": ["a1b2c3d4e5f6g7h8i9j0kalbmc", "zz1yy2xx3ww4vv5uu6tt7ss8rr"],
                            "groupConfiguration": { "groupEntityType": "MyCorp::UserGroup" },
                        }
                    },
                    "principalEntityType": SOURCE_ENTITY_TYPE,
                })
            },
        },
    ];

    let router = router();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // One store per row, so what a row's store holds is only what that row
        // created.
        let store_id = store(&router).await;
        let token = format!("vanished-{index}");
        let pool = user_pool(&router, &format!("vanished-pool-{index}")).await;
        let request = source_request(&store_id, &token, &(row.members)(&user_pool_arn(
            SIGNING_REGION,
            &pool,
        )));

        let created = ok(&router, "CreateIdentitySource", &request).await;
        let created_id = created["identitySourceId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !created_id.is_empty(),
            "case {case:?}: expected CreateIdentitySource to report an identitySourceId, \
             got {created}"
        );
        let before = read_source(&router, &store_id, &created_id).await;

        // The pool the recorded request references stops existing.
        delete_user_pool(&router, &pool).await;

        // The control: under a token the service has never seen, that same body
        // is now refused on its payload, 400 naming the member at fault. What
        // the row pins is therefore the PRECEDENCE of the replay, not that the
        // payload became servable.
        let mut unseen = request.clone();
        unseen["clientToken"] = json!(format!("unseen-{index}"));
        let (status, body) = call(&router, "CreateIdentitySource", &unseen).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 under an unseen token once the pool is gone, \
             got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected a ValidationException under an unseen token, got {body}"
        );
        let paths: Vec<String> = field_list(&body).into_iter().map(|(path, _)| path).collect();
        assert!(
            paths.iter().any(|path| path == POOL_ARN_FIELD),
            "case {case:?}: expected fieldList to name {POOL_ARN_FIELD}, got {paths:?} in {body}"
        );

        // The recorded token with those same parameters: the replay is decided
        // first, so the answer is the first call's output.
        let replayed = ok(&router, "CreateIdentitySource", &request).await;
        assert_eq!(
            replayed, created,
            "case {case:?}: expected the replay to answer with {created}, got {replayed}"
        );

        // The replay reported state rather than writing any: the identity source
        // still reads back exactly as it did, and the store holds only it.
        let after = read_source(&router, &store_id, &created_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: the replay must change nothing, expected {before}, got {after}"
        );
        assert_eq!(
            listed_source_ids(&router, &store_id).await,
            vec![created_id.clone()],
            "case {case:?}: expected the store to hold only {created_id}, got {:?}",
            listed_source_ids(&router, &store_id).await
        );
    }
}

/// One `CreateIdentitySource` payload this service refuses on its own checks,
/// beside a body it serves under the same token.
struct SourcePayloadFault {
    case: &'static str,
    /// The request members beside `policyStoreId` and `clientToken` whose
    /// payload this service refuses.
    refused: Value,
    /// The member `ValidationException$fieldList` names for `refused`.
    field: &'static str,
}

/// RULE (the other half): the same token replayed with DIFFERENT parameters is
/// the modelled 409 `ConflictException` naming the resource that token created,
/// whatever the replayed request's payload would otherwise be refused for, and
/// that payload still earns its 400 `ValidationException` naming the member at
/// fault under a token the service has never seen.
///
/// The rows differ along the KIND of payload fault: a pool reference that
/// resolves to nothing for each of the reasons the service distinguishes — a pool
/// nothing minted, one that was deleted, one in another Region, one in another
/// account — and an OpenID Connect provider for which there is no
/// `principalEntityType` to resolve, which is the other member this operation
/// refuses on.
#[tokio::test]
async fn a_reused_identity_source_token_conflicts_however_its_payload_would_be_refused() {
    let router = router();
    // The pool the accepted body references: one the user-pool service really
    // minted, in the region the harness signs for.
    let live_arn = user_pool_arn(SIGNING_REGION, &user_pool(&router, "reused-live-pool").await);
    // A pool that existed and then stopped existing.
    let deleted = user_pool(&router, "reused-deleted-pool").await;
    delete_user_pool(&router, &deleted).await;
    // A pool minted in another Region: an identity source can only reference one
    // in its policy store's Region.
    let other_region = "us-west-2";
    let elsewhere = super::harness::user_pool_in(&router, other_region, "reused-other-region").await;

    let accepted = json!({
        "configuration": cognito_provider(&live_arn),
        "principalEntityType": SOURCE_ENTITY_TYPE,
    });
    let rows = [
        SourcePayloadFault {
            case: "a pool id nothing ever minted",
            refused: json!({
                "configuration": cognito_provider(&user_pool_arn(
                    SIGNING_REGION,
                    &format!("{SIGNING_REGION}_0a1b2c3d4"),
                )),
                "principalEntityType": SOURCE_ENTITY_TYPE,
            }),
            field: POOL_ARN_FIELD,
        },
        SourcePayloadFault {
            case: "a pool that was minted and then deleted",
            refused: json!({
                "configuration": cognito_provider(&user_pool_arn(SIGNING_REGION, &deleted)),
                "principalEntityType": SOURCE_ENTITY_TYPE,
            }),
            field: POOL_ARN_FIELD,
        },
        SourcePayloadFault {
            case: "a live pool in another Region than the policy store",
            refused: json!({
                "configuration": cognito_provider(&user_pool_arn(other_region, &elsewhere)),
                "principalEntityType": SOURCE_ENTITY_TYPE,
            }),
            field: POOL_ARN_FIELD,
        },
        SourcePayloadFault {
            case: "a pool in another account",
            refused: json!({
                "configuration": cognito_provider(&format!(
                    "arn:aws:cognito-idp:{SIGNING_REGION}:210987654321:userpool/{SIGNING_REGION}_1a2b3c4d5"
                )),
                "principalEntityType": SOURCE_ENTITY_TYPE,
            }),
            field: POOL_ARN_FIELD,
        },
        SourcePayloadFault {
            // An OIDC source's principals are of a type only the caller can
            // name, so a request that names none has nothing to resolve to.
            case: "an OpenID Connect provider with no entity type to resolve",
            refused: json!({
                "configuration": {
                    "openIdConnectConfiguration": {
                        "issuer": "https://reused.example.com",
                        "tokenSelection": { "accessTokenOnly": { "principalIdClaim": "sub" } },
                    }
                },
            }),
            field: PRINCIPAL_ENTITY_TYPE_FIELD,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let store_id = store(&router).await;
        let token = format!("reused-{index}");

        // The control: under a token the service has never seen, this payload is
        // refused on its own merits, 400 naming the member at fault.
        let unseen = source_request(&store_id, &format!("unseen-{index}"), &row.refused);
        let (status, body) = call(&router, "CreateIdentitySource", &unseen).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 under an unseen token, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected a ValidationException under an unseen token, got {body}"
        );
        let paths: Vec<String> = field_list(&body).into_iter().map(|(path, _)| path).collect();
        assert!(
            paths.iter().any(|path| path == row.field),
            "case {case:?}: expected fieldList to name {}, got {paths:?} in {body}",
            row.field
        );

        // The token is now recorded against a request the service served.
        let served = source_request(&store_id, &token, &accepted);
        let created = ok(&router, "CreateIdentitySource", &served).await;
        let created_id = created["identitySourceId"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        // That same token, replayed with the unservable payload: the replay is
        // decided first, so the answer is the conflict naming the identity
        // source the token already created — not the payload's 400.
        let replay = source_request(&store_id, &token, &row.refused);
        let (status, body) = call(&router, "CreateIdentitySource", &replay).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected HTTP 409 for the reused token, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ConflictException",
            "case {case:?}: expected a ConflictException for the reused token, got {body}"
        );
        assert_eq!(
            body["resources"],
            json!([{ "resourceId": created_id, "resourceType": "IDENTITY_SOURCE" }]),
            "case {case:?}: expected the conflict to name {created_id} as an IDENTITY_SOURCE, \
             got {body}"
        );

        // Neither answer reached state: the token still replays the identity
        // source it was recorded against, and nothing else was created.
        let replayed = ok(&router, "CreateIdentitySource", &served).await;
        assert_eq!(
            replayed, created,
            "case {case:?}: the token must still replay {created}, got {replayed}"
        );
        assert_eq!(
            listed_source_ids(&router, &store_id).await,
            vec![created_id.clone()],
            "case {case:?}: expected the store to hold only {created_id}, got {:?}",
            listed_source_ids(&router, &store_id).await
        );
    }
}
