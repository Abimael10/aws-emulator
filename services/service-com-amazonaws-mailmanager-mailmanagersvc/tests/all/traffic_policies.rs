// SPDX-License-Identifier: CC0-1.0

//! The traffic-policy lifecycle (`CreateTrafficPolicy`, `GetTrafficPolicy`), end to end over
//! the wire.
//!
//! Every assertion here goes through the real request path — `MultiServiceRouter::dispatch`
//! with `awsJson1_0` bodies, `POST /` and `X-Amz-Target: MailManagerSvc.<Operation>` — so
//! what it pins is the emulator's HTTP behaviour, not an internal helper.
//!
//! The tests are tables: each row is one input drawn from the class the rule covers
//! (several `TrafficPolicyName` shapes across the modelled pattern and both length bounds,
//! several `PolicyStatements` lists from the empty one through every `PolicyCondition`
//! variant, both `DefaultAction` values, the `MaxMessageSizeBytes` bound, several tag sets,
//! the idempotency contract, and the traffic-policy quota).

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_mailmanager_mailmanagersvc::state::MAX_TRAFFIC_POLICIES;
use service_com_amazonaws_mailmanager_mailmanagersvc::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for; the ARNs the service returns must carry it.
const REGION: &str = "eu-west-1";

/// The account id every emulated ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// One emulator holding one MailManager service on one shared engine.
struct Harness {
    router: MultiServiceRouter,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The signature itself is not under test here, and the fixed test credential
        // below is not this process's signing key.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Call one operation and return `(http status, parsed body)`.
    async fn call(&self, operation: &str, body: Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("MailManagerSvc.{operation}"))
            // A signed SDK request routes by its SigV4 credential scope, whose 4th field
            // is this service's signing name (`ses`) and whose 3rd is the region the
            // response's ARNs must use.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261021/{REGION}/ses/aws4_request, \
                     SignedHeaders=host;x-amz-target, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the request body serializes"),
            ))
            .expect("a valid HTTP request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            json!({})
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Create a traffic policy and return its `TrafficPolicyId`, failing the test on any
    /// refusal.
    async fn create(&self, body: Value) -> String {
        let (status, out) = self.call("CreateTrafficPolicy", body.clone()).await;
        assert_eq!(
            status, 200,
            "case {body:?}: expected CreateTrafficPolicy to answer 200, got {status} {out:?}"
        );
        out["TrafficPolicyId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {body:?}: CreateTrafficPolicy returned no TrafficPolicyId: {out:?}")
            })
            .to_string()
    }
}

/// The wire error code of a failure body: the `__type` member, without its namespace.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .split('#')
        .next_back()
        .unwrap_or_default()
        .to_string()
}

/// A minted `TrafficPolicyId` satisfies `TrafficPolicyId`'s modelled constraint: length
/// 1..100 (`smithy_query(query="shape",shape="TrafficPolicyId")` declares no pattern).
fn assert_well_formed_id(case: &str, id: &str) {
    assert!(
        (1..=100).contains(&id.len()),
        "case {case:?}: TrafficPolicyId must be 1..100 characters, got {id:?} of {} characters",
        id.len()
    );
}

/// The `PolicyStatements` lists the round-trip table spans, each with the case it is named
/// by.
///
/// They walk the shape of a `PolicyStatement` as the model declares it
/// (`smithy_query(query="shape",shape="PolicyStatement,PolicyCondition")`): the empty list
/// (`PolicyStatementList` declares no length bound), one statement per `PolicyCondition`
/// union variant — string, IPv4, IPv6, TLS and boolean — a statement carrying several
/// conditions, and several statements whose stored ORDER is observable in the read-back.
fn statement_lists() -> Vec<(String, Value)> {
    let string_condition = json!({
        "StringExpression": {
            "Evaluate": { "Attribute": "RECIPIENT" },
            "Operator": "EQUALS",
            "Values": ["someone@example.com", "other@example.com"],
        }
    });
    let ip_condition = json!({
        "IpExpression": {
            "Evaluate": { "Attribute": "SENDER_IP" },
            "Operator": "CIDR_MATCHES",
            "Values": ["10.0.0.0/8", "192.168.1.1/32"],
        }
    });
    let ipv6_condition = json!({
        "Ipv6Expression": {
            "Evaluate": { "Attribute": "SENDER_IPV6" },
            "Operator": "NOT_CIDR_MATCHES",
            "Values": ["2001:db8::/32"],
        }
    });
    let tls_condition = json!({
        "TlsExpression": {
            "Evaluate": { "Attribute": "TLS_PROTOCOL" },
            "Operator": "MINIMUM_TLS_VERSION",
            "Value": "TLS1_3",
        }
    });
    let boolean_condition = json!({
        "BooleanExpression": {
            "Evaluate": {
                "IsInAddressList": {
                    "Attribute": "RECIPIENT",
                    "AddressLists": [
                        format!("arn:aws:ses:{REGION}:{ACCOUNT}:mailmanager-address-list/abc"),
                    ],
                }
            },
            "Operator": "IS_TRUE",
        }
    });

    vec![
        (
            "no statements at all, which PolicyStatementList admits".to_string(),
            json!([]),
        ),
        (
            "one ALLOW statement on a string condition".to_string(),
            json!([{ "Conditions": [string_condition.clone()], "Action": "ALLOW" }]),
        ),
        (
            "one DENY statement on an IPv4 condition".to_string(),
            json!([{ "Conditions": [ip_condition.clone()], "Action": "DENY" }]),
        ),
        (
            "one DENY statement on an IPv6 condition".to_string(),
            json!([{ "Conditions": [ipv6_condition.clone()], "Action": "DENY" }]),
        ),
        (
            "one ALLOW statement on a TLS condition".to_string(),
            json!([{ "Conditions": [tls_condition.clone()], "Action": "ALLOW" }]),
        ),
        (
            "one DENY statement on an address-list boolean condition".to_string(),
            json!([{ "Conditions": [boolean_condition.clone()], "Action": "DENY" }]),
        ),
        (
            "one statement carrying several conditions of different kinds".to_string(),
            json!([{
                "Conditions": [
                    string_condition.clone(),
                    ip_condition.clone(),
                    tls_condition.clone(),
                ],
                "Action": "DENY",
            }]),
        ),
        (
            "three statements, whose order is part of the policy".to_string(),
            json!([
                { "Conditions": [string_condition], "Action": "DENY" },
                { "Conditions": [ip_condition], "Action": "ALLOW" },
                { "Conditions": [boolean_condition], "Action": "DENY" },
            ]),
        ),
    ]
}

/// Every created traffic policy is readable back, exactly as it was stored, whatever the
/// shape of its name, whatever statements it carries, whichever default action it carries,
/// with or without a message-size ceiling, and whatever tags it carries.
///
/// The names span `TrafficPolicyName`'s modelled pattern `^[A-Za-z0-9_\-]+$` and its length
/// bounds 3..63 (`smithy_query(query="shape",shape="TrafficPolicyName")`): the shortest name
/// the pattern admits, each punctuation character it allows, digits only, mixed case, and
/// the longest name it admits.
#[tokio::test]
async fn create_then_get_round_trips_every_member_of_a_traffic_policy() {
    let harness = Harness::new();

    let names: [String; 6] = [
        "abc".to_string(),
        "with_underscore".to_string(),
        "with-hyphens-too".to_string(),
        "0123456789".to_string(),
        "MiXeD_Case-9".to_string(),
        "x".repeat(63),
    ];
    // Rows also vary the default action, the message-size ceiling (absent, the bottom of
    // `MaxMessageSizeBytes`' 1.. range, and an ordinary ceiling) and the tag set.
    let default_actions: [&str; 2] = ["ALLOW", "DENY"];
    let size_limits: [Value; 3] = [Value::Null, json!(1), json!(10_485_760)];
    let tag_sets: [Value; 3] = [
        Value::Null,
        json!([{ "Key": "team", "Value": "mail" }]),
        json!([
            { "Key": "team", "Value": "mail" },
            { "Key": "env", "Value": "test" },
            { "Key": "empty-value", "Value": "" },
        ]),
    ];

    let statements = statement_lists();
    let mut minted: Vec<String> = Vec::new();
    for (index, (statements_case, statement_list)) in statements.iter().enumerate() {
        let name = &names[index % names.len()];
        let default_action = default_actions[index % default_actions.len()];
        let size_limit = &size_limits[index % size_limits.len()];
        let tags = &tag_sets[index % tag_sets.len()];

        let mut body = json!({
            "TrafficPolicyName": name,
            "PolicyStatements": statement_list.clone(),
            "DefaultAction": default_action,
        });
        if !size_limit.is_null() {
            body["MaxMessageSizeBytes"] = size_limit.clone();
        }
        if !tags.is_null() {
            body["Tags"] = tags.clone();
        }
        let case = format!(
            "name {name:?} with {statements_case}, default action {default_action}, size limit \
             {size_limit:?} and tags {tags:?}"
        );

        let traffic_policy_id = harness.create(body).await;
        assert_well_formed_id(&case, &traffic_policy_id);
        minted.push(traffic_policy_id.clone());

        let (status, out) = harness
            .call(
                "GetTrafficPolicy",
                json!({ "TrafficPolicyId": traffic_policy_id }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetTrafficPolicy to answer 200, got {status} {out:?}"
        );
        assert_eq!(
            out["TrafficPolicyId"].as_str(),
            Some(traffic_policy_id.as_str()),
            "case {case:?}: GetTrafficPolicy must report the id the create minted, got {out:?}"
        );
        assert_eq!(
            out["TrafficPolicyName"].as_str(),
            Some(name.as_str()),
            "case {case:?}: the stored name must be reported verbatim, got {out:?}"
        );
        assert_eq!(
            out["TrafficPolicyArn"].as_str(),
            Some(
                format!(
                    "arn:aws:ses:{REGION}:{ACCOUNT}:mailmanager-traffic-policy/{traffic_policy_id}"
                )
                .as_str()
            ),
            "case {case:?}: unexpected TrafficPolicyArn, got {out:?}"
        );
        assert_eq!(
            &out["PolicyStatements"], statement_list,
            "case {case:?}: the statements must be reported as submitted, in the submitted \
             order, got {out:?}"
        );
        assert_eq!(
            out["DefaultAction"].as_str(),
            Some(default_action),
            "case {case:?}: the stored default action must be reported, got {out:?}"
        );
        if size_limit.is_null() {
            assert!(
                out.get("MaxMessageSizeBytes").is_none(),
                "case {case:?}: a policy created without a MaxMessageSizeBytes must report \
                 none, got {out:?}"
            );
        } else {
            assert_eq!(
                &out["MaxMessageSizeBytes"], size_limit,
                "case {case:?}: the stored message-size ceiling must be reported, got {out:?}"
            );
        }

        let created = out["CreatedTimestamp"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: GetTrafficPolicy must report a CreatedTimestamp, got {out:?}")
        });
        let updated = out["LastUpdatedTimestamp"].as_f64().unwrap_or_else(|| {
            panic!(
                "case {case:?}: GetTrafficPolicy must report a LastUpdatedTimestamp, got {out:?}"
            )
        });
        assert!(
            created > 0.0,
            "case {case:?}: CreatedTimestamp must be an instant, got {created}"
        );
        assert_eq!(
            created, updated,
            "case {case:?}: a traffic policy nothing has updated reports its creation instant \
             as its LastUpdatedTimestamp, got {created} and {updated}"
        );

        // A second read of the same policy answers the same thing: the read changed nothing.
        let (again_status, again) = harness
            .call(
                "GetTrafficPolicy",
                json!({ "TrafficPolicyId": traffic_policy_id }),
            )
            .await;
        assert_eq!(
            again_status, 200,
            "case {case:?}: expected the second GetTrafficPolicy to answer 200, got \
             {again_status} {again:?}"
        );
        assert_eq!(
            again, out,
            "case {case:?}: two reads of one traffic policy must report the same thing"
        );
    }

    minted.sort();
    let distinct = {
        let mut ids = minted.clone();
        ids.dedup();
        ids.len()
    };
    assert_eq!(
        distinct,
        statements.len(),
        "every create must mint its own TrafficPolicyId, got {minted:?}"
    );
}

/// Two traffic policies created with the SAME name are two policies: a policy is addressed
/// by the `TrafficPolicyId` the create minted, and the model documents `TrafficPolicyName`
/// only as "a user-friendly name for the traffic policy resource" with no uniqueness of its
/// own.
#[tokio::test]
async fn the_same_name_can_name_two_traffic_policies() {
    let harness = Harness::new();

    // Rows vary what else the two creates share: everything, and the name alone.
    let cases: [(&str, Value, Value); 2] = [
        (
            "identical requests",
            json!({ "TrafficPolicyName": "shared", "PolicyStatements": [], "DefaultAction": "ALLOW" }),
            json!({ "TrafficPolicyName": "shared", "PolicyStatements": [], "DefaultAction": "ALLOW" }),
        ),
        (
            "same name, different default action and statements",
            json!({ "TrafficPolicyName": "shared-too", "PolicyStatements": [], "DefaultAction": "ALLOW" }),
            json!({
                "TrafficPolicyName": "shared-too",
                "PolicyStatements": [{
                    "Conditions": [{
                        "IpExpression": {
                            "Evaluate": { "Attribute": "SENDER_IP" },
                            "Operator": "CIDR_MATCHES",
                            "Values": ["172.16.0.0/12"],
                        }
                    }],
                    "Action": "DENY",
                }],
                "DefaultAction": "DENY",
            }),
        ),
    ];
    for (case, first_body, second_body) in cases {
        let first = harness.create(first_body).await;
        let second = harness.create(second_body.clone()).await;
        assert_ne!(first, second, "case {case:?}: two creates must mint two ids");

        let (status, out) = harness
            .call("GetTrafficPolicy", json!({ "TrafficPolicyId": second }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the second policy must be readable, got {status} {out:?}"
        );
        assert_eq!(
            &out["PolicyStatements"], &second_body["PolicyStatements"],
            "case {case:?}: the second policy must carry its OWN statements, got {out:?}"
        );
        assert_eq!(
            out["DefaultAction"].as_str(),
            second_body["DefaultAction"].as_str(),
            "case {case:?}: the second policy must carry its OWN default action, got {out:?}"
        );

        let (status, first_out) = harness
            .call("GetTrafficPolicy", json!({ "TrafficPolicyId": first }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first policy must still be readable, got {status} {first_out:?}"
        );
    }
}

/// `ClientToken` is `@idempotencyToken`: repeating one create returns the id the first call
/// minted and provisions nothing new, while the same token presented with a DIFFERENT
/// request is refused with the modelled `ConflictException` rather than answered with the
/// first request's policy.
#[tokio::test]
async fn a_client_token_makes_a_retry_return_the_first_traffic_policy() {
    let harness = Harness::new();

    // Rows: a retry of each distinguishable part of the request — the name, the statements,
    // the default action, the size ceiling and the tags — plus a plain repeat of the
    // identical request.
    let cases: [(&str, Value, Value); 5] = [
        (
            "identical repeat",
            json!({
                "TrafficPolicyName": "retry-one",
                "PolicyStatements": [],
                "DefaultAction": "ALLOW",
                "ClientToken": "token-one",
            }),
            json!({
                "TrafficPolicyName": "retry-one",
                "PolicyStatements": [],
                "DefaultAction": "ALLOW",
                "ClientToken": "token-one",
            }),
        ),
        (
            "different name",
            json!({
                "TrafficPolicyName": "retry-two",
                "PolicyStatements": [],
                "DefaultAction": "ALLOW",
                "ClientToken": "token-two",
            }),
            json!({
                "TrafficPolicyName": "retry-two-b",
                "PolicyStatements": [],
                "DefaultAction": "ALLOW",
                "ClientToken": "token-two",
            }),
        ),
        (
            "different statements",
            json!({
                "TrafficPolicyName": "retry-three",
                "PolicyStatements": [],
                "DefaultAction": "DENY",
                "ClientToken": "token-three",
            }),
            json!({
                "TrafficPolicyName": "retry-three",
                "PolicyStatements": [{
                    "Conditions": [{
                        "TlsExpression": {
                            "Evaluate": { "Attribute": "TLS_PROTOCOL" },
                            "Operator": "IS",
                            "Value": "TLS1_2",
                        }
                    }],
                    "Action": "ALLOW",
                }],
                "DefaultAction": "DENY",
                "ClientToken": "token-three",
            }),
        ),
        (
            "different default action",
            json!({
                "TrafficPolicyName": "retry-four",
                "PolicyStatements": [],
                "DefaultAction": "ALLOW",
                "ClientToken": "token-four",
            }),
            json!({
                "TrafficPolicyName": "retry-four",
                "PolicyStatements": [],
                "DefaultAction": "DENY",
                "ClientToken": "token-four",
            }),
        ),
        (
            "different size ceiling and tags",
            json!({
                "TrafficPolicyName": "retry-five",
                "PolicyStatements": [],
                "DefaultAction": "ALLOW",
                "MaxMessageSizeBytes": 1024,
                "Tags": [{ "Key": "a", "Value": "1" }],
                "ClientToken": "token-five",
            }),
            json!({
                "TrafficPolicyName": "retry-five",
                "PolicyStatements": [],
                "DefaultAction": "ALLOW",
                "MaxMessageSizeBytes": 2048,
                "Tags": [{ "Key": "a", "Value": "2" }],
                "ClientToken": "token-five",
            }),
        ),
    ];
    for (case, first_body, second_body) in cases {
        let first = harness.create(first_body.clone()).await;

        // The identical repeat is the retry; every other row differs from the first
        // request, so it is not one.
        let is_retry = first_body == second_body;
        let (status, out) = harness
            .call("CreateTrafficPolicy", second_body.clone())
            .await;
        if is_retry {
            assert_eq!(
                status, 200,
                "case {case:?}: a retry of the same request must be served, got {status} {out:?}"
            );
            assert_eq!(
                out["TrafficPolicyId"].as_str(),
                Some(first.as_str()),
                "case {case:?}: a retry must answer with the id the first call minted, got {out:?}"
            );
        } else {
            assert_eq!(
                status, 409,
                "case {case:?}: reusing a ClientToken for a different request must be refused, \
                 got {status} {out:?}"
            );
            assert_eq!(
                error_code(&out),
                "ConflictException",
                "case {case:?}: unexpected error code, got {out:?}"
            );
            assert_eq!(
                out["Message"].as_str(),
                Some(
                    format!(
                        "ClientToken was already used to create traffic policy {first} with \
                         different parameters."
                    )
                    .as_str()
                ),
                "case {case:?}: unexpected refusal message, got {out:?}"
            );
        }

        // Whichever answer the second call got, the first policy is unchanged.
        let (status, stored) = harness
            .call("GetTrafficPolicy", json!({ "TrafficPolicyId": first }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first policy must still be readable, got {status} {stored:?}"
        );
        assert_eq!(
            stored["TrafficPolicyName"].as_str(),
            first_body["TrafficPolicyName"].as_str(),
            "case {case:?}: the first policy must keep the name it was created with, got \
             {stored:?}"
        );
        assert_eq!(
            &stored["PolicyStatements"], &first_body["PolicyStatements"],
            "case {case:?}: the first policy must keep the statements it was created with, got \
             {stored:?}"
        );
        assert_eq!(
            stored["DefaultAction"].as_str(),
            first_body["DefaultAction"].as_str(),
            "case {case:?}: the first policy must keep the default action it was created with, \
             got {stored:?}"
        );
    }
}

/// A `TrafficPolicyId` nothing created is refused with the modelled
/// `ResourceNotFoundException` (`@httpError 404`) and a message naming the id the request
/// asked for, and the refusal creates nothing.
#[tokio::test]
async fn get_traffic_policy_on_an_unknown_id_is_refused() {
    let harness = Harness::new();
    let live = harness
        .create(json!({
            "TrafficPolicyName": "present",
            "PolicyStatements": [],
            "DefaultAction": "ALLOW",
        }))
        .await;

    // Rows: ids inside `TrafficPolicyId`'s 1..100 length that name nothing — the shortest
    // one, a hyphenated one, the longest one, and the live id with one character changed.
    let unknown: [String; 4] = [
        "x".to_string(),
        "no-such-traffic-policy".to_string(),
        "9".repeat(100),
        format!("{}0", &live[..live.len() - 1]),
    ];
    for id in &unknown {
        if id == &live {
            continue;
        }
        let (status, out) = harness
            .call("GetTrafficPolicy", json!({ "TrafficPolicyId": id }))
            .await;
        assert_eq!(
            status, 404,
            "case {id:?}: expected an unknown TrafficPolicyId to answer 404, got {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ResourceNotFoundException",
            "case {id:?}: unexpected error code, got {out:?}"
        );
        assert_eq!(
            out["Message"].as_str(),
            Some(format!("Could not find TrafficPolicy with id {id}").as_str()),
            "case {id:?}: unexpected refusal message, got {out:?}"
        );
    }

    let (status, out) = harness
        .call("GetTrafficPolicy", json!({ "TrafficPolicyId": live }))
        .await;
    assert_eq!(
        status, 200,
        "case live-after-misses: the stored policy must still be readable, got {status} {out:?}"
    );
}

/// A request outside the modelled constraints is refused with a 400 `ValidationException`
/// before anything is stored — never a 501 and never a create.
#[tokio::test]
async fn requests_outside_the_modelled_constraints_are_refused() {
    let harness = Harness::new();

    // Rows walk each published bound of the input shapes and one step past it:
    // `TrafficPolicyName` (len 3..63, pattern `^[A-Za-z0-9_\-]+$`), `MaxMessageSizeBytes`
    // (range 1..), `AcceptAction` (ALLOW | DENY), `PolicyConditions` (len 1..) and the
    // members the model makes required.
    let cases: [(&str, Value); 8] = [
        (
            "two characters, one below the minimum name length",
            json!({ "TrafficPolicyName": "ab", "PolicyStatements": [], "DefaultAction": "ALLOW" }),
        ),
        (
            "64 characters, one past the maximum name length",
            json!({
                "TrafficPolicyName": "x".repeat(64),
                "PolicyStatements": [],
                "DefaultAction": "ALLOW",
            }),
        ),
        (
            "a dot, outside the name pattern",
            json!({
                "TrafficPolicyName": "not.allowed",
                "PolicyStatements": [],
                "DefaultAction": "ALLOW",
            }),
        ),
        (
            "a zero size ceiling, one below the MaxMessageSizeBytes minimum",
            json!({
                "TrafficPolicyName": "zero-size",
                "PolicyStatements": [],
                "DefaultAction": "ALLOW",
                "MaxMessageSizeBytes": 0,
            }),
        ),
        (
            "a default action outside the AcceptAction value set",
            json!({
                "TrafficPolicyName": "bad-action",
                "PolicyStatements": [],
                "DefaultAction": "QUARANTINE",
            }),
        ),
        (
            "a statement with no conditions, one below the PolicyConditions minimum",
            json!({
                "TrafficPolicyName": "no-conditions",
                "PolicyStatements": [{ "Conditions": [], "Action": "DENY" }],
                "DefaultAction": "ALLOW",
            }),
        ),
        (
            "no DefaultAction member at all, which the model makes required",
            json!({ "TrafficPolicyName": "no-default", "PolicyStatements": [] }),
        ),
        (
            "no PolicyStatements member at all, which the model makes required",
            json!({ "TrafficPolicyName": "no-statements", "DefaultAction": "DENY" }),
        ),
    ];
    for (case, body) in cases {
        let (status, out) = harness.call("CreateTrafficPolicy", body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ValidationException",
            "case {case:?}: unexpected error code, got {out:?}"
        );
    }

    // The bounds themselves are accepted, so the refusals above are the bound and not a
    // blanket rejection: the shortest and the longest name the pattern admits, and the
    // smallest size ceiling the range admits.
    for (case, body) in [
        (
            "three characters, at the minimum name length",
            json!({
                "TrafficPolicyName": "abc",
                "PolicyStatements": [],
                "DefaultAction": "DENY",
                "MaxMessageSizeBytes": 1,
            }),
        ),
        (
            "63 characters, at the maximum name length",
            json!({
                "TrafficPolicyName": "y".repeat(63),
                "PolicyStatements": [],
                "DefaultAction": "ALLOW",
            }),
        ),
    ] {
        let accepted = harness.create(body).await;
        assert_well_formed_id(case, &accepted);
    }
}

/// The traffic-policy store is BOUNDED: a create past the per-account quota is refused with
/// the modelled `ServiceQuotaExceededException`, and the refusal does not corrupt what is
/// already stored.
#[tokio::test]
async fn creates_past_the_traffic_policy_quota_are_refused() {
    let harness = Harness::new();

    let mut created: Vec<String> = Vec::new();
    for index in 0..MAX_TRAFFIC_POLICIES {
        created.push(
            harness
                .create(json!({
                    "TrafficPolicyName": format!("quota-{index:04}"),
                    "PolicyStatements": [],
                    "DefaultAction": "ALLOW",
                }))
                .await,
        );
    }

    // Rows: a plain create, and one carrying a ClientToken that was never used — neither
    // may be served past the quota.
    let cases: [(&str, Value); 2] = [
        (
            "plain",
            json!({
                "TrafficPolicyName": "quota-overflow",
                "PolicyStatements": [],
                "DefaultAction": "ALLOW",
            }),
        ),
        (
            "with a fresh ClientToken",
            json!({
                "TrafficPolicyName": "quota-overflow-2",
                "PolicyStatements": [],
                "DefaultAction": "DENY",
                "ClientToken": "never-seen",
            }),
        ),
    ];
    for (case, body) in cases {
        let (status, out) = harness.call("CreateTrafficPolicy", body).await;
        assert_eq!(
            status, 402,
            "case {case:?}: expected a create past the quota to answer 402, got {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ServiceQuotaExceededException",
            "case {case:?}: unexpected error code, got {out:?}"
        );
        assert_eq!(
            out["Message"].as_str(),
            Some(
                format!(
                    "The maximum number of traffic policies ({MAX_TRAFFIC_POLICIES}) has been \
                     reached. Delete a traffic policy before creating another."
                )
                .as_str()
            ),
            "case {case:?}: unexpected refusal message, got {out:?}"
        );
    }

    // Every policy created before the quota was reached is still readable.
    for id in &created {
        let (status, out) = harness
            .call("GetTrafficPolicy", json!({ "TrafficPolicyId": id }))
            .await;
        assert_eq!(
            status, 200,
            "case {id:?}: a stored policy must survive a refused create, got {status} {out:?}"
        );
    }
}
