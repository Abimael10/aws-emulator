// SPDX-License-Identifier: CC0-1.0

//! The Scope lifecycle, end to end over HTTP: `CreateScope` then `GetScope`.
//!
//! RULE under test: `CreateScope` mints a scope and PERSISTS it, answering 201 with the
//! `scopeName` and `scopeConfiguration` the caller submitted, the `EntityStatus`
//! `isPublished` selects, `version` "1" for a first create, and a freshly generated
//! `scopeId`, `scopeArn`, `updateToken` and `updatedAt`; every member it answers comes out
//! of the stored record, so a following `GetScope` — by id or by ARN — answers exactly the
//! same thing.
//!
//! Every case drives the real wire path (`MultiServiceRouter::dispatch` with a signed
//! restJson1 request), so what is asserted is what a client sees. The rows differ along
//! what the rule generalises over: which resource types the configuration selects, which
//! form of resource set each selects (all, explicit ARNs, a tag/ALB expression), which
//! `accountFilter` variant is set (including the valueless one), whether the scope is
//! published, and whether a description is supplied.

use std::sync::Arc;

use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// What a caller sees: the HTTP facts it branches on plus the JSON body.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    json: serde_json::Value,
}

/// The emulator under test: this one service mounted on the production router.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The wire behaviour under test is the operations', not SigV4's.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<serde_json::Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "network-security-manager.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            // The credential scope is what routes the request to this service and names
            // the region its ARNs carry.
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261021/us-east-1/\
                 network-security-manager/aws4_request, SignedHeaders=host, Signature=0",
            )
            .body(payload)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let raw = response.into_body();
        let json = if raw.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null)
        };
        Reply {
            status,
            error_code,
            json,
        }
    }

    async fn create(&self, body: serde_json::Value) -> Reply {
        self.call("POST", "/scopes", Some(body)).await
    }

    async fn get(&self, identifier: &str) -> Reply {
        self.call(
            "GET",
            &format!("/scopes/{}", path_label(identifier)),
            None,
        )
        .await
    }
}

/// An identifier in a `@httpLabel` position: a scope ARN carries `:` and `/`, which the
/// label must percent-encode to reach the handler whole.
fn path_label(identifier: &str) -> String {
    identifier
        .chars()
        .map(|c| match c {
            ':' => "%3A".to_string(),
            '/' => "%2F".to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// `ScopeId`'s modelled `@pattern` is `^[a-z0-9]{1,64}$`.
fn is_modelled_scope_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// `UpdateToken`'s modelled `@pattern` is
/// `^([0-9a-f]{8})-([0-9a-f]{4}-){3}([0-9a-f]{12})$`.
fn is_modelled_update_token(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(width, group)| {
                group.len() == *width
                    && group
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
}

/// One `CreateScope` case: the body it sends and what the rule says the answer carries.
struct Row {
    label: &'static str,
    body: serde_json::Value,
    /// The `EntityStatus` `isPublished` selects for this row.
    status: &'static str,
}

/// The rows: one per form of `scopeConfiguration` and one per `isPublished` choice. The
/// resource types come from `ScopeResourceType`
/// (`smithy_query(query="shape", shape="ScopeResourceType")`), whose members are
/// `AWS::ApiGateway::Stage`, `AWS::CloudFront::Distribution`, `AWS::EC2::EIP`,
/// `AWS::ElasticLoadBalancingV2::LoadBalancer::application` and
/// `AWS::ElasticLoadBalancing::LoadBalancer`.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "a resource type selected whole, the recorded case",
            body: serde_json::json!({
                "scopeName": "scope-include-all",
                "scopeConfiguration": {
                    "resourceScopes": { "AWS::EC2::EIP": { "includeAll": true } },
                },
            }),
            status: "ACTIVE",
        },
        Row {
            label: "a resource type the issue does not cite, selected by explicit ARNs",
            body: serde_json::json!({
                "scopeName": "scope-explicit-arns",
                "scopeDescription": "selected by ARN",
                "scopeConfiguration": {
                    "resourceScopes": {
                        "AWS::CloudFront::Distribution": {
                            "include": {
                                "explicitArns": [
                                    "arn:aws:cloudfront::000000000000:distribution/E1EXAMPLE",
                                ],
                            },
                        },
                    },
                },
            }),
            status: "ACTIVE",
        },
        Row {
            label: "two resource types, one excluded by a tag expression",
            body: serde_json::json!({
                "scopeName": "scope-tag-expression",
                "scopeConfiguration": {
                    "resourceScopes": {
                        "AWS::ApiGateway::Stage": {
                            "exclude": {
                                "expression": { "criteria": { "tags": { "env": "prod" } } },
                            },
                        },
                        "AWS::ElasticLoadBalancing::LoadBalancer": { "includeAll": false },
                    },
                },
            }),
            status: "ACTIVE",
        },
        Row {
            label: "a nested and/or/not expression over an ALB configuration",
            body: serde_json::json!({
                "scopeName": "scope-nested-expression",
                "scopeConfiguration": {
                    "resourceScopes": {
                        "AWS::ElasticLoadBalancingV2::LoadBalancer::application": {
                            "include": {
                                "expression": {
                                    "and": [
                                        {
                                            "criteria": {
                                                "albConfig": {
                                                    "scheme": "internet-facing",
                                                    "ipAddressType": "ipv4",
                                                },
                                            },
                                        },
                                        {
                                            "not": {
                                                "or": [
                                                    {
                                                        "criteria": {
                                                            "tags": { "tier": "test" },
                                                        },
                                                    },
                                                ],
                                            },
                                        },
                                    ],
                                },
                            },
                        },
                    },
                },
            }),
            status: "ACTIVE",
        },
        Row {
            label: "an accountFilter selecting every account, the union's valueless member",
            body: serde_json::json!({
                "scopeName": "scope-accounts-all",
                "scopeConfiguration": {
                    "accountFilter": { "includeAll": {} },
                    "resourceScopes": { "AWS::EC2::EIP": { "includeAll": true } },
                },
            }),
            status: "ACTIVE",
        },
        Row {
            label: "an accountFilter including named accounts and organizational units",
            body: serde_json::json!({
                "scopeName": "scope-accounts-included",
                "scopeConfiguration": {
                    "accountFilter": {
                        "include": {
                            "accountIds": ["012345678901", "210987654321"],
                            "organizationalUnits": ["ou-abcd-11111111"],
                        },
                    },
                    "resourceScopes": { "AWS::EC2::EIP": { "includeAll": true } },
                },
            }),
            status: "ACTIVE",
        },
        Row {
            label: "an accountFilter excluding named accounts",
            body: serde_json::json!({
                "scopeName": "scope-accounts-excluded",
                "scopeConfiguration": {
                    "accountFilter": { "exclude": { "accountIds": ["012345678901"] } },
                    "resourceScopes": { "AWS::EC2::EIP": { "includeAll": true } },
                },
            }),
            status: "ACTIVE",
        },
        Row {
            label: "an unpublished scope is a draft",
            body: serde_json::json!({
                "scopeName": "scope-draft",
                "isPublished": false,
                "scopeConfiguration": {
                    "resourceScopes": { "AWS::EC2::EIP": { "includeAll": true } },
                },
            }),
            status: "DRAFT",
        },
        Row {
            label: "an explicitly published scope is active",
            body: serde_json::json!({
                "scopeName": "scope-published",
                "isPublished": true,
                "scopeConfiguration": {
                    "resourceScopes": { "AWS::ApiGateway::Stage": { "includeAll": true } },
                },
            }),
            status: "ACTIVE",
        },
    ]
}

/// RULE: a create answers 201 with the submitted name and configuration, the status
/// `isPublished` selects, version "1" and freshly generated identifiers — and a read of
/// the scope, by id or by ARN, answers exactly the same record.
#[tokio::test]
async fn a_created_scope_is_answered_from_and_read_back_out_of_the_store() {
    let emulator = Emulator::new();
    let mut minted: Vec<String> = Vec::new();

    for row in rows() {
        let label = row.label;
        let created = emulator.create(row.body.clone()).await;
        assert_eq!(
            created.status, 201,
            "case {label}: expected 201, got {} {:?}",
            created.status, created.json
        );
        assert_eq!(
            created.error_code, None,
            "case {label}: a success carries no error code, got {:?}",
            created.error_code
        );

        let scope_id = created.json["scopeId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {label}: expected a scopeId, got {:?}", created.json))
            .to_string();
        assert!(
            is_modelled_scope_id(&scope_id),
            "case {label}: expected a modelled scopeId, got {scope_id:?}"
        );
        assert!(
            !minted.contains(&scope_id),
            "case {label}: scopeId {scope_id:?} was already handed out"
        );
        minted.push(scope_id.clone());

        let scope_arn = created.json["scopeArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {label}: expected a scopeArn, got {:?}", created.json))
            .to_string();
        assert!(
            scope_arn.starts_with("arn:") && scope_arn.ends_with(&format!(":scope/{scope_id}")),
            "case {label}: expected an ARN naming the scope, got {scope_arn:?}"
        );

        assert_eq!(
            created.json["scopeName"], row.body["scopeName"],
            "case {label}: the answer must echo the submitted scopeName"
        );
        assert_eq!(
            created.json["scopeConfiguration"], row.body["scopeConfiguration"],
            "case {label}: the answer must echo the submitted scopeConfiguration, got {:?}",
            created.json["scopeConfiguration"]
        );
        assert_eq!(
            created.json["scopeDescription"],
            row.body
                .get("scopeDescription")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            "case {label}: unexpected scopeDescription"
        );
        assert_eq!(
            created.json["status"].as_str(),
            Some(row.status),
            "case {label}: expected status {:?}, got {:?}",
            row.status,
            created.json["status"]
        );
        assert_eq!(
            created.json["version"].as_str(),
            Some("1"),
            "case {label}: a first create is version 1, got {:?}",
            created.json["version"]
        );
        assert_eq!(
            created.json["isSnapshot"].as_bool(),
            Some(false),
            "case {label}: a created scope is not a snapshot, got {:?}",
            created.json["isSnapshot"]
        );
        assert_eq!(
            created.json["hasPublishedVersion"].as_bool(),
            Some(row.status == "ACTIVE"),
            "case {label}: unexpected hasPublishedVersion, got {:?}",
            created.json["hasPublishedVersion"]
        );
        let update_token = created.json["updateToken"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {label}: expected an updateToken, got {:?}",
                    created.json
                )
            })
            .to_string();
        assert!(
            is_modelled_update_token(&update_token),
            "case {label}: expected a modelled updateToken, got {update_token:?}"
        );
        // `updatedAt` targets `DateTimestamp`, which declares `@timestampFormat`
        // `date-time`, so it is carried as an RFC 3339 string (pinned across the service in
        // `timestamp_wire_form`).
        assert!(
            created.json["updatedAt"]
                .as_str()
                .is_some_and(|value| value.contains('T') && value.ends_with('Z')),
            "case {label}: expected a date-time updatedAt, got {:?}",
            created.json["updatedAt"]
        );

        // The read is what proves the create PERSISTED: both identifiers resolve to the
        // one record, and the record is what the create answered with.
        for identifier in [scope_id.as_str(), scope_arn.as_str()] {
            let read = emulator.get(identifier).await;
            assert_eq!(
                read.status, 200,
                "case {label}: reading {identifier:?} expected 200, got {} {:?}",
                read.status, read.json
            );
            assert_eq!(
                read.json, created.json,
                "case {label}: reading {identifier:?} must answer what the create answered"
            );
        }
    }
}

/// RULE: a `clientToken` is `@idempotencyToken` — replaying a create with the same token
/// answers with the first call's scope instead of minting a second one.
#[tokio::test]
async fn replaying_a_create_with_one_client_token_answers_the_first_result() {
    let emulator = Emulator::new();
    let body = serde_json::json!({
        "clientToken": "scope-token-1",
        "scopeName": "scope-idempotent",
        "scopeConfiguration": {
            "resourceScopes": { "AWS::EC2::EIP": { "includeAll": true } },
        },
    });

    let first = emulator.create(body.clone()).await;
    assert_eq!(
        first.status, 201,
        "the first create expected 201, got {} {:?}",
        first.status, first.json
    );
    let replay = emulator.create(body).await;
    assert_eq!(
        replay.status, 201,
        "the replay expected 201, got {} {:?}",
        replay.status, replay.json
    );
    assert_eq!(
        replay.json, first.json,
        "case replay: the same token must answer the first call's scope"
    );

    // A create with a DIFFERENT token is a different scope, so the replay above is
    // idempotence and not a name collision.
    let other = emulator
        .create(serde_json::json!({
            "clientToken": "scope-token-2",
            "scopeName": "scope-idempotent",
            "scopeConfiguration": {
                "resourceScopes": { "AWS::EC2::EIP": { "includeAll": true } },
            },
        }))
        .await;
    assert_eq!(
        other.status, 201,
        "case second token: expected 201, got {} {:?}",
        other.status, other.json
    );
    assert_ne!(
        other.json["scopeId"], first.json["scopeId"],
        "case second token: a fresh token mints a fresh scope"
    );
}

/// `ResourceLogicalExpression` nesting `depth` levels deep: `depth - 1` `not` wrappers
/// around one `criteria` expression, so the count is the number of expressions a reader
/// has to descend through.
fn nested_expression(depth: usize) -> serde_json::Value {
    let mut value = serde_json::json!({ "criteria": { "tags": { "env": "prod" } } });
    for _ in 1..depth {
        value = serde_json::json!({ "not": value });
    }
    value
}

/// A `CreateScope` body whose one resource set selects by `expression`.
fn expression_body(name: &str, expression: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "scopeName": name,
        "scopeConfiguration": {
            "resourceScopes": {
                "AWS::EC2::EIP": { "include": { "expression": expression } },
            },
        },
    })
}

/// RULE: `ResourceLogicalExpression` is self-referential, so the nesting of a request body
/// drives the recursion of reading it; a body nested past the bound the service carries is
/// refused with a 4xx, and one inside it is taken and echoed back.
#[tokio::test]
async fn an_expression_nested_past_the_bound_is_refused() {
    let emulator = Emulator::new();

    // The rows differ along nesting depth: one level, the deepest level accepted, the
    // first level past it, and a body far past it (which the codec's own nesting bound
    // catches first) — none of which may take the process down.
    let accepted = [1_usize, 20];
    let refused = [21_usize, 64, 500];

    for depth in accepted {
        let expression = nested_expression(depth);
        let reply = emulator
            .create(expression_body(
                &format!("scope-nested-{depth}"),
                expression.clone(),
            ))
            .await;
        assert_eq!(
            reply.status, 201,
            "case depth {depth}: expected 201, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            reply.json["scopeConfiguration"]["resourceScopes"]["AWS::EC2::EIP"]["include"]
                ["expression"],
            expression,
            "case depth {depth}: the answer must echo the submitted expression"
        );
    }

    for depth in refused {
        let reply = emulator
            .create(expression_body(
                &format!("scope-nested-{depth}"),
                nested_expression(depth),
            ))
            .await;
        assert_eq!(
            reply.status, 400,
            "case depth {depth}: expected 400, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some("ValidationException"),
            "case depth {depth}: unexpected x-amzn-errortype, got {:?}",
            reply.error_code
        );
    }
}

/// RULE: a read whose identifier names no stored scope is refused with the modelled
/// `ResourceNotFoundException` (HTTP 404), not with an invented record.
#[tokio::test]
async fn reading_a_scope_that_does_not_exist_is_refused() {
    let emulator = Emulator::new();
    let created = emulator
        .create(serde_json::json!({
            "scopeName": "scope-present",
            "scopeConfiguration": {
                "resourceScopes": { "AWS::EC2::EIP": { "includeAll": true } },
            },
        }))
        .await;
    assert_eq!(
        created.status, 201,
        "the create expected 201, got {} {:?}",
        created.status, created.json
    );

    // The rows differ along the FORM of the identifier that names nothing: a well-formed
    // id, and an ARN of the right shape for another scope.
    let cases = [
        "0123456789abcdef",
        "arn:aws:network-security-manager:us-east-1:000000000000:scope/0123456789abcdef",
    ];
    for identifier in cases {
        let read = emulator.get(identifier).await;
        assert_eq!(
            read.status, 404,
            "case {identifier:?}: expected 404, got {} {:?}",
            read.status, read.json
        );
        assert_eq!(
            read.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {identifier:?}: unexpected x-amzn-errortype, got {:?}",
            read.error_code
        );
        assert_eq!(
            read.json["resourceType"].as_str(),
            Some("Scope"),
            "case {identifier:?}: unexpected resourceType, got {:?}",
            read.json["resourceType"]
        );
    }
}
