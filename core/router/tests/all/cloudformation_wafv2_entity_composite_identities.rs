// SPDX-License-Identifier: CC0-1.0

//! The identifier a CloudFormation stack reports for a WAFv2 entity is that resource type's
//! COMPOSITE primary identifier.
//!
//! The rule under test: **where a resource type's primary identifier is composite, the
//! `PhysicalResourceId` CloudFormation reports for one of its resources — and what `Ref`
//! resolves to — is the values of those identifier properties joined with `|`, in the order the
//! type's schema declares them.** For the four WAFv2 ENTITY types that order is the name the
//! entity was created under, the `Id` AWS WAF assigned and the `Scope` it lives in, which is
//! exactly the triple `GetWebACL`, `GetIPSet`, `GetRuleGroup` and `GetRegexPatternSet` take to
//! address one entity: a client reads the resource out of the stack, splits the id and hands
//! the parts straight to that service. An identifier carrying no separator at all leaves that
//! client with no way to recover either the entity id or the scope.
//!
//! Two crates have to agree for that to hold (CloudFormation reports the id and makes the
//! calls, AWS WAF mints the id and serves them), so the suite lives here: a crate-local test
//! can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: which of the four entity kinds the
//! template declares, the scope it is created in (`REGIONAL` and `CLOUDFRONT`), and whether the
//! template NAMES the entity or leaves the name to CloudFormation. Every expected value is
//! computed from what the row declares plus the id WAF minted, never read back out of the
//! answer being checked — and the last half of the suite deletes the stack, which addresses
//! each entity by the id reported here, so an identifier that could not be decomposed again
//! would leave the entity behind.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery wire helpers this test crate publishes ONCE, in `query_wire`: a suite that
// writes its own percent-encoder, element reader or `EntityId` reading drifts from the others.
use crate::query_wire::{
    form_encode, is_waf_entity_id, opt_element, outputs_of, physical_ids_of,
};

/// The region every request here is signed for, and so the region the stack's entities belong
/// to: WAF serves the CLOUDFRONT namespace at that endpoint ALONE ("API and SDKs - For all
/// calls, use the Region endpoint us-east-1", `Scope`'s own doc), and one row declares a
/// CLOUDFRONT-scoped entity. A REGIONAL entity is served at every endpoint, so the regional
/// rows are unaffected.
const REGION: &str = "us-east-1";

/// The two `Scope` values a WAFv2 entity is created in (`smithy_query(query="shape",
/// shape="Scope")`).
const SCOPE_REGIONAL: &str = "REGIONAL";
const SCOPE_CLOUDFRONT: &str = "CLOUDFRONT";

/// The separator CloudFormation joins the parts of a composite primary identifier with.
const SEPARATOR: char = '|';

/// How many characters CloudFormation's generated-name suffix carries.
const GENERATED_SUFFIX_LEN: usize = 12;

/// How long a stack may take to reach a terminal status before this suite gives up, and how
/// long it waits between looks. Generous: the wait is on the stack's own reported STATUS, never
/// on the clock, so a loaded host only takes more looks.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other suite
/// here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_wafv2_awswaf_20190729::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call that must answer HTTP 200.
async fn stack_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = cloudformation(router, action, members).await;
    assert!(
        status == 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Call one WAFv2 operation over its awsJson1_1 wire.
async fn wafv2(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("wafv2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSWAF_20190729.{operation}"))
        .header("authorization", authorization("wafv2"))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The wire error code of a WAFv2 refusal: awsJson1_1 carries it as the body's `__type`.
fn waf_error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// Read `DescribeStacks` until the stack reports a status that is no longer in progress, and
/// answer that body.
///
/// The completion is a transition the CLOCK owes, so this LOOKS AGAIN until the stack reports it
/// rather than waiting a span out: nothing is nudged between reads.
async fn settled(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = stack_ok(router, case, "DescribeStacks", &[("StackName", stack_name)]).await;
        let status = opt_element(&body, "StackStatus").unwrap_or_default();
        if !status.ends_with("_IN_PROGRESS") {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stack_name} to leave {status:?}, last read {body}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// Create a stack and answer the `DescribeStacks` body it settles at, asserting it completed.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    stack_ok(
        router,
        case,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", template)],
    )
    .await;
    let body = settled(router, case, stack_name).await;
    assert!(
        opt_element(&body, "StackStatus").as_deref() == Some("CREATE_COMPLETE"),
        "case {case:?}: expected {stack_name} to complete, got {body}"
    );
    body
}

/// Whether `name` is the `<stack>-<logical id>-<suffix>` name CloudFormation generates for a
/// resource the template does not name.
fn is_generated_name(name: &str, stack_name: &str, logical_id: &str) -> bool {
    let prefix = format!("{stack_name}-{logical_id}-");
    name.strip_prefix(&prefix).is_some_and(|suffix| {
        suffix.len() == GENERATED_SUFFIX_LEN
            && suffix
                .chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
    })
}

/// The ARN AWS WAF gives an entity of `kind` named `name` with the minted `id`, in `scope`:
/// `arn:aws:wafv2:<region>:<account>:<global|regional>/<kind>/<name>/<id>`. Written out here
/// from that layout rather than taken from the answer.
fn expected_arn(scope: &str, kind: &str, name: &str, id: &str) -> String {
    let segment = if scope == SCOPE_CLOUDFRONT {
        "global"
    } else {
        "regional"
    };
    format!(
        "arn:aws:wafv2:{REGION}:{}:{segment}/{kind}/{name}/{id}",
        engine::emulator_account_id()
    )
}

/// The `VisibilityConfig` a web ACL and a rule group each require.
fn visibility(metric: &str) -> Value {
    json!({
        "SampledRequestsEnabled": false,
        "CloudWatchMetricsEnabled": false,
        "MetricName": metric,
    })
}

/// What the row says about one WAFv2 entity a template declares.
struct DeclaredEntity {
    /// The resource's logical id, which a generated name is built from and which the outputs
    /// are keyed by.
    logical_id: &'static str,
    /// The resource type, the resource-path segment its kind occupies in an ARN, and the
    /// operation that reads one back.
    resource_type: &'static str,
    kind: &'static str,
    get: &'static str,
    /// The `Scope` the declaration states, which is the identifier's last part.
    scope: &'static str,
    /// The name the template states, or `None` where CloudFormation generates it.
    declared_name: Option<&'static str>,
    /// The properties of the type beyond `Name` and `Scope`, which differ per kind.
    properties: fn() -> Value,
}

impl DeclaredEntity {
    /// The declaration this entity is, as the template carries it.
    fn declaration(&self) -> Value {
        let mut properties = (self.properties)();
        let members = properties
            .as_object_mut()
            .expect("the per-kind properties are an object");
        members.insert("Scope".to_string(), json!(self.scope));
        if let Some(name) = self.declared_name {
            members.insert("Name".to_string(), json!(name));
        }
        json!({"Type": self.resource_type, "Properties": properties})
    }

    /// The three outputs the template publishes for this entity: what `Ref` resolves to, and
    /// the `Name`, `Id` and `Arn` the type publishes as return values.
    fn outputs(&self) -> Vec<(String, Value)> {
        let attribute = |name: &str| json!({"Value": {"Fn::GetAtt": [self.logical_id, name]}});
        vec![
            (
                format!("{}Ref", self.logical_id),
                json!({"Value": {"Ref": self.logical_id}}),
            ),
            (format!("{}Name", self.logical_id), attribute("Name")),
            (format!("{}Id", self.logical_id), attribute("Id")),
            (format!("{}Arn", self.logical_id), attribute("Arn")),
        ]
    }
}

/// One row: a stack and the WAFv2 entities its template declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    entities: &'static [DeclaredEntity],
}

const ROWS: &[Row] = &[
    Row {
        // The reported case, beside the three sibling kinds the same stacks declare.
        case: "the four REGIONAL entity kinds, two of them named",
        stack_name: "wafv2-identity-regional",
        entities: &[
            DeclaredEntity {
                logical_id: "WebAcl",
                resource_type: "AWS::WAFv2::WebACL",
                kind: "webacl",
                get: "GetWebACL",
                scope: SCOPE_REGIONAL,
                declared_name: Some("identity-regional-acl"),
                properties: || {
                    json!({
                        "DefaultAction": {"Allow": {}},
                        "VisibilityConfig": visibility("identityRegionalAcl"),
                    })
                },
            },
            DeclaredEntity {
                logical_id: "Listed",
                resource_type: "AWS::WAFv2::IPSet",
                kind: "ipset",
                get: "GetIPSet",
                scope: SCOPE_REGIONAL,
                // Unnamed: CloudFormation generates the name, so the identifier's first part
                // is a value no declaration states.
                declared_name: None,
                properties: || {
                    json!({
                        "IPAddressVersion": "IPV4",
                        "Addresses": ["198.51.100.17/32"],
                    })
                },
            },
            DeclaredEntity {
                logical_id: "Guard",
                resource_type: "AWS::WAFv2::RuleGroup",
                kind: "rulegroup",
                get: "GetRuleGroup",
                scope: SCOPE_REGIONAL,
                declared_name: Some("identity-regional-group"),
                properties: || {
                    json!({
                        "Capacity": 1,
                        "VisibilityConfig": visibility("identityRegionalGroup"),
                    })
                },
            },
            DeclaredEntity {
                logical_id: "Patterns",
                resource_type: "AWS::WAFv2::RegexPatternSet",
                kind: "regexpatternset",
                get: "GetRegexPatternSet",
                scope: SCOPE_REGIONAL,
                declared_name: None,
                properties: || json!({"RegularExpressionList": [{"RegexString": "^/admin/"}]}),
            },
        ],
    },
    Row {
        case: "a CLOUDFRONT-scoped web ACL and regex pattern set",
        stack_name: "wafv2-identity-global",
        entities: &[
            DeclaredEntity {
                logical_id: "GlobalAcl",
                resource_type: "AWS::WAFv2::WebACL",
                kind: "webacl",
                get: "GetWebACL",
                scope: SCOPE_CLOUDFRONT,
                declared_name: Some("identity-global-acl"),
                properties: || {
                    json!({
                        "DefaultAction": {"Block": {}},
                        "VisibilityConfig": visibility("identityGlobalAcl"),
                    })
                },
            },
            DeclaredEntity {
                logical_id: "GlobalPatterns",
                resource_type: "AWS::WAFv2::RegexPatternSet",
                kind: "regexpatternset",
                get: "GetRegexPatternSet",
                scope: SCOPE_CLOUDFRONT,
                declared_name: None,
                properties: || json!({"RegularExpressionList": [{"RegexString": "^/health$"}]}),
            },
        ],
    },
];

/// The template one row declares, with every entity and the outputs that publish its values.
fn template_of(row: &Row) -> String {
    let mut resources = serde_json::Map::new();
    let mut outputs = serde_json::Map::new();
    for entity in row.entities {
        resources.insert(entity.logical_id.to_string(), entity.declaration());
        for (key, output) in entity.outputs() {
            outputs.insert(key, output);
        }
    }
    json!({"Resources": resources, "Outputs": outputs}).to_string()
}

/// Every row: the identifier the stack reports for each WAFv2 entity is `<Name>|<Id>|<Scope>`,
/// the triple that type's primary identifier is — and each part addresses the entity in AWS WAF.
#[tokio::test]
async fn a_stack_reports_each_wafv2_entity_under_its_composite_primary_identifier() {
    for row in ROWS {
        let router = build_router();
        let template = template_of(row);
        let described = create_and_settle(&router, row.case, row.stack_name, &template).await;
        let published = outputs_of(&described);
        let resources = stack_ok(
            &router,
            row.case,
            "DescribeStackResources",
            &[("StackName", row.stack_name)],
        )
        .await;
        let reported: BTreeMap<String, String> = physical_ids_of(&resources);

        for entity in row.entities {
            let case = format!("{} [{}]", row.case, entity.logical_id);
            let output = |suffix: &str| {
                let key = format!("{}{suffix}", entity.logical_id);
                published
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
            };
            let physical_id = reported
                .get(entity.logical_id)
                .cloned()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no PhysicalResourceId in {resources}")
                });

            // The identifier is the three parts joined, and nothing else: a client splits on
            // the separator and reads the three fields by position.
            let parts: Vec<&str> = physical_id.split(SEPARATOR).collect();
            assert!(
                parts.len() == 3,
                "case {case:?}: expected the PhysicalResourceId to be the three parts \
                 <Name>|<Id>|<Scope>, got {physical_id:?}"
            );
            let (name, id, scope) = (parts[0], parts[1], parts[2]);

            // Part one is the name the entity was created under: the declared one, or the
            // `<stack>-<logical id>-<suffix>` one CloudFormation generated.
            let declared = output("Name");
            assert!(
                name == declared,
                "case {case:?}: expected the first part to be the entity's name {declared:?}, \
                 got {name:?}"
            );
            match entity.declared_name {
                Some(stated) => assert!(
                    name == stated,
                    "case {case:?}: expected the first part to be the declared name \
                     {stated:?}, got {name:?}"
                ),
                None => assert!(
                    is_generated_name(name, row.stack_name, entity.logical_id),
                    "case {case:?}: expected the first part to be a generated <stack>-<logical \
                     id>-<suffix> name, got {name:?}"
                ),
            }

            // Part two is the id AWS WAF minted, which is also the type's published `Id` — not
            // the composite, and not the entity's name.
            assert!(
                is_waf_entity_id(id),
                "case {case:?}: expected the second part to be an EntityId AWS WAF minted, got \
                 {id:?}"
            );
            let published_id = output("Id");
            assert!(
                id == published_id,
                "case {case:?}: expected the second part to be the published Id \
                 {published_id:?}, got {id:?}"
            );

            // Part three is the scope the declaration states.
            assert!(
                scope == entity.scope,
                "case {case:?}: expected the third part to be the declared scope {:?}, got \
                 {scope:?}",
                entity.scope
            );

            // `Ref` resolves to that same identifier: it is what the stack knows the resource
            // by.
            let by_ref = output("Ref");
            assert!(
                by_ref == physical_id,
                "case {case:?}: expected Ref to resolve to the reported identifier \
                 {physical_id:?}, got {by_ref:?}"
            );

            // And the three parts, read off that identifier alone, address the entity in AWS
            // WAF: the read answers the ARN that service built from the same three values.
            let arn = expected_arn(scope, entity.kind, name, id);
            let (status, answer) = wafv2(
                &router,
                entity.get,
                &json!({"Name": name, "Scope": scope, "Id": id}),
            )
            .await;
            assert!(
                status == 200,
                "case {case:?}: expected the parts of {physical_id:?} to address the entity \
                 through {}, got {status} with {answer}",
                entity.get
            );
            let reported_arn = answer
                .pointer(&format!("/{}/ARN", entity_member(entity.get)))
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                reported_arn == arn,
                "case {case:?}: expected {} to report the ARN {arn:?}, got {answer}",
                entity.get
            );
            let published_arn = output("Arn");
            assert!(
                published_arn == arn,
                "case {case:?}: expected the published ARN to be {arn:?}, got \
                 {published_arn:?}"
            );
        }
    }
}

/// The member a `Get…` answers the entity under, which is the operation's name without its
/// verb (`GetWebACL` -> `WebACL`).
fn entity_member(get: &str) -> &str {
    get.strip_prefix("Get").unwrap_or(get)
}

/// The other side of the rule: the identifier the stack recorded is one the stack itself can
/// decompose again, so deleting the stack deletes the entities it created — AWS WAF then holds
/// none of them.
#[tokio::test]
async fn deleting_the_stack_deletes_every_entity_its_composite_identifier_names() {
    let row = &ROWS[0];
    let case = "the four REGIONAL entity kinds, deleted with their stack";
    let router = build_router();
    let template = template_of(row);
    let described = create_and_settle(&router, case, row.stack_name, &template).await;
    let resources = stack_ok(
        &router,
        case,
        "DescribeStackResources",
        &[("StackName", row.stack_name)],
    )
    .await;
    let reported = physical_ids_of(&resources);

    // Each entity is there to begin with, addressed by the parts of its reported identifier.
    let addressed: Vec<(&DeclaredEntity, Value)> = row
        .entities
        .iter()
        .map(|entity| {
            let physical_id = reported
                .get(entity.logical_id)
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: no id for {}", entity.logical_id));
            let parts: Vec<&str> = physical_id.split(SEPARATOR).collect();
            assert!(
                parts.len() == 3,
                "case {case:?}: expected a three-part identifier for {}, got {physical_id:?}",
                entity.logical_id
            );
            (
                entity,
                json!({"Name": parts[0], "Scope": parts[2], "Id": parts[1]}),
            )
        })
        .collect();
    for (entity, key) in &addressed {
        let (status, answer) = wafv2(&router, entity.get, key).await;
        assert!(
            status == 200,
            "case {case:?}: expected {} to read the entity the stack created, got {status} \
             with {answer} (stack {described})",
            entity.get
        );
    }

    stack_ok(
        &router,
        case,
        "DeleteStack",
        &[("StackName", row.stack_name)],
    )
    .await;
    // The deletion is a transition the clock owes: look again until the stack is gone rather
    // than waiting a span out. `DescribeStacks` on a deleted stack is a refusal, which is the
    // condition this waits for.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(&router, "DescribeStacks", &[("StackName", row.stack_name)]).await;
        let reported_status = opt_element(&body, "StackStatus").unwrap_or_default();
        if status != 200 || reported_status == "DELETE_COMPLETE" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stack to be deleted, last read {body}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }

    for (entity, key) in &addressed {
        let (status, answer) = wafv2(&router, entity.get, key).await;
        assert!(
            status == 400 && waf_error_code(&answer) == "WAFNonexistentItemException",
            "case {case:?}: expected {} on the deleted entity to answer 400 \
             WAFNonexistentItemException, got {status} with {answer}",
            entity.get
        );
    }
}
