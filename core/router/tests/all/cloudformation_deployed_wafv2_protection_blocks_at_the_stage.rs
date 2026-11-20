// SPDX-License-Identifier: CC0-1.0

//! An IaC-deployed AWS WAF protection WORKS: a CloudFormation stack of `AWS::WAFv2::IPSet`,
//! `AWS::WAFv2::WebACL` and `AWS::WAFv2::WebACLAssociation` reaches `CREATE_COMPLETE`, and
//! the web ACL it deployed decides requests at the REST API stage it was put in front of.
//!
//! The rule this suite pins: **a stack whose resources each stand for a modelled control-plane
//! call of the owning service deploys when every one of those calls is accepted, and what it
//! deployed is that service's own live state** — so the ARN `Fn::GetAtt <IPSet>.Arn` resolves
//! to is the one AWS WAF minted (`CreateIPSet` answers `Summary.ARN`), it reaches
//! `CreateWebACL` under the member `IPSetReferenceStatement$ARN` requires, `AssociateWebACL`
//! binds the named resource, and the rule tree the stack saved is what the protected
//! resource's requests are then decided by. A declaration none of those calls refuses must
//! not roll the stack back: a rollback deletes the protection it just made and leaves the
//! client no modelled WAF error to act on.
//!
//! Three crates have to agree for that to hold — CloudFormation makes the calls, AWS WAF
//! serves them and holds the binding, and API Gateway's data plane asks AWS WAF about each
//! request to a protected stage — so the suite lives here: a crate-local test can only ever
//! see one side of it.
//!
//! The rows differ along what the rule generalises over: the IP address family the set
//! declares (`IPAddressVersion` is `IPV4` | `IPV6`), how wide a block an entry names (a
//! single host, a sub-prefix), and how many entries the set lists. Each row's probes are the
//! addresses on both sides of what the declaration covers: every bound of a block, and one
//! address past either end. The address entries are the ones `IPSet$Addresses` itself
//! documents — "for requests that originated from the IP address 192.0.2.44, specify
//! `192.0.2.44/32`", "from IP addresses from 192.0.2.0 to 192.0.2.255, specify
//! `192.0.2.0/24`", and the two IPv6 examples beside them.
//!
//! Every value asserted is computed from what the row declares plus the identifiers AWS WAF
//! minted, never read back out of the answer being checked, and one stage carries no stack of
//! its own at all — so a `403` is the deployed protection's doing rather than the emulator
//! refusing that address everywhere.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery wire helpers this test crate publishes ONCE, in `query_wire`: a suite that
// writes its own percent-encoder, element reader or output pairing drifts from the others.
use crate::query_wire::{element, elements, form_encode, is_waf_entity_id, outputs_of};

/// The region every request here is signed for, which is the region the stack's resources
/// belong to and the one the stage's invoke host names.
const REGION: &str = "us-west-2";

/// The namespace every entity here lives in: a REST API stage is a regional resource, and
/// `Scope`'s `CLOUDFRONT` value is "for a global resource type, such as a Amazon CloudFront
/// distribution".
const SCOPE: &str = "REGIONAL";

/// How long a stack is given to leave its in-progress statuses, and how often it is looked at
/// again meanwhile. The wait is for the CONDITION, so a slower host only takes more looks; the
/// deadline is a safety net so a wait nothing will satisfy fails instead of hanging.
const SETTLE_DEADLINE: Duration = Duration::from_secs(240);
const SETTLE_INTERVAL: Duration = Duration::from_millis(50);

/// The terminal status a deployment that provisioned everything it declared rests in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The body a refused request is answered with at the edge: API Gateway's own `WAF_FILTERED`
/// gateway response carries `{"message":"Forbidden"}`.
const FORBIDDEN: &str = "Forbidden";

/// The path part of the one resource the api serves, and the body its `MOCK` integration
/// answers — so an answer says whether the request reached the origin at all.
const ORIGIN_PATH: &str = "orders";
const ORIGIN_BODY: &str = "{\"at\":\"orders\"}";

/// One emulator with CloudFormation, AWS WAF and API Gateway mounted on ONE engine, exactly
/// as `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callbacks CloudFormation makes sign their own requests either
/// way (the credential scope is what routes them).
struct Emulator {
    router: MultiServiceRouter,
}

/// One answer, as this suite reads it: the status and the body as text and as JSON.
struct Answer {
    status: u16,
    text: String,
    body: Value,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_wafv2_awswaf_20190729::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// Drive one signed request at `signing_name`'s wire.
    async fn send(
        &self,
        signing_name: &str,
        method: &str,
        uri: &str,
        content_type: &str,
        body: String,
        target: Option<String>,
    ) -> Answer {
        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                     SignedHeaders=host, Signature=0"
                ),
            )
            .header("content-type", content_type);
        if let Some(target) = target {
            request = request.header("x-amz-target", target);
        }
        let request = request
            .body(Bytes::from(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(&response.into_body()).into_owned();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Answer { status, text, body }
    }

    /// One CloudFormation call over its awsQuery wire.
    async fn cloudformation(&self, action: &str, members: &[(&str, &str)]) -> Answer {
        let mut body = format!("Action={action}&Version=2010-05-15");
        for (key, value) in members {
            body.push('&');
            body.push_str(&form_encode(key));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        self.send(
            "cloudformation",
            "POST",
            "/",
            "application/x-www-form-urlencoded",
            body,
            None,
        )
        .await
    }

    /// One CloudFormation call that must succeed, answered as the XML text.
    async fn stack_ok(&self, case: &str, action: &str, members: &[(&str, &str)]) -> String {
        let answer = self.cloudformation(action, members).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from {action}, got {} with {}",
            answer.status, answer.text
        );
        answer.text
    }

    /// One AWS WAF call over its awsJson1_1 wire that must succeed.
    async fn waf_ok(&self, case: &str, operation: &str, request: Value) -> Value {
        let answer = self
            .send(
                "wafv2",
                "POST",
                "/",
                "application/x-amz-json-1.1",
                serde_json::to_string(&request).expect("the request serializes"),
                Some(format!("AWSWAF_20190729.{operation}")),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from {operation}, got {} with {}",
            answer.status, answer.text
        );
        answer.body
    }

    /// One API Gateway control-plane call that must be accepted with `expected`.
    async fn api_ok(&self, method: &str, uri: &str, body: Option<Value>, expected: u16) -> Value {
        let payload = body.map(|value| value.to_string()).unwrap_or_default();
        let answer = self
            .send(
                "apigateway",
                method,
                uri,
                "application/json",
                payload,
                None,
            )
            .await;
        assert_eq!(
            answer.status, expected,
            "case {method} {uri}: expected {expected}, got {} with {}",
            answer.status, answer.text
        );
        answer.body
    }

    /// One unsigned GET to a deployed stage's invoke URL, stating the address it came from in
    /// the forwarded-for header every protected resource's service reads the client address
    /// off.
    async fn invoke(&self, api: &str, stage: &str, from: &str) -> Answer {
        let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method("GET")
            .uri(format!("https://{host}/{stage}/{ORIGIN_PATH}"))
            .header("host", &host)
            .header("x-forwarded-for", from)
            .body(Bytes::new())
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(&response.into_body()).into_owned();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Answer { status, text, body }
    }
}

/// The ARN `AssociateWebACL` binds a web ACL to a REST API stage by: "for an Amazon API
/// Gateway REST API: `arn:partition:apigateway:region::/restapis/api-id/stages/stage-name`"
/// (`AssociateWebACLRequest$ResourceArn`), written out from that documented format.
fn stage_arn(api: &str, stage: &str) -> String {
    format!("arn:aws:apigateway:{REGION}::/restapis/{api}/stages/{stage}")
}

/// The `VisibilityConfig` every web ACL and every rule of one requires.
fn visibility(metric: &str) -> Value {
    json!({
        "SampledRequestsEnabled": false,
        "CloudWatchMetricsEnabled": false,
        "MetricName": metric,
    })
}

/// One row: an IP set a stack declares, the addresses the web ACL it deploys must refuse, and
/// the addresses it must let through.
struct Row {
    case: &'static str,
    /// The stage this row's deployed web ACL is put in front of, and the stem its stack and
    /// its two entities are named after.
    stage: &'static str,
    /// `IPSet$IPAddressVersion`: the family the entries belong to.
    version: &'static str,
    /// `IPSet$Addresses`, in the notation that member documents.
    addresses: &'static [&'static str],
    /// Source addresses the deployed rule must refuse: every bound of what the entries cover.
    refused: &'static [&'static str],
    /// Source addresses the deployed rule must not refuse: one past either end of it.
    admitted: &'static [&'static str],
}

/// The rows, spanning the family, the block width and the entry count. Every entry is one
/// `IPSet$Addresses` documents as an example of its form, and the probes are that entry's own
/// bounds and the addresses just outside them.
const ROWS: &[Row] = &[
    Row {
        // The reported shape: a list of single-host entries.
        case: "one IPv4 host address",
        stage: "host",
        version: "IPV4",
        addresses: &["192.0.2.44/32"],
        refused: &["192.0.2.44"],
        admitted: &["192.0.2.43", "192.0.2.45"],
    },
    Row {
        case: "an IPv4 /24 block",
        stage: "block",
        version: "IPV4",
        addresses: &["192.0.2.0/24"],
        refused: &["192.0.2.0", "192.0.2.128", "192.0.2.255"],
        admitted: &["192.0.1.255", "192.0.3.0"],
    },
    Row {
        // The three-entry array `IPSet$Addresses` publishes as its own example, verbatim:
        // `"Addresses": ["192.0.2.44/32", "192.0.2.0/24", "192.0.0.0/16"]`.
        case: "several IPv4 entries of different widths",
        stage: "several",
        version: "IPV4",
        addresses: &["192.0.2.44/32", "192.0.2.0/24", "192.0.0.0/16"],
        // 192.0.1.255 sits one below the /24's own start and INSIDE the /16, so the wider
        // entry is what has to cover it; 191.255.255.255 and 192.1.0.0 sit one past either
        // end of that /16, so no entry of the set covers them.
        refused: &[
            "192.0.2.44",
            "192.0.2.7",
            "192.0.1.255",
            "192.0.0.0",
            "192.0.255.255",
        ],
        admitted: &["191.255.255.255", "192.1.0.0"],
    },
    Row {
        case: "an IPv6 host address",
        stage: "sixhost",
        version: "IPV6",
        addresses: &["1111:0000:0000:0000:0000:0000:0000:0111/128"],
        refused: &["1111:0000:0000:0000:0000:0000:0000:0111"],
        admitted: &[
            "1111:0000:0000:0000:0000:0000:0000:0110",
            "1111:0000:0000:0000:0000:0000:0000:0112",
        ],
    },
    Row {
        case: "an IPv6 /64 block",
        stage: "sixblock",
        version: "IPV6",
        addresses: &["1111:0000:0000:0000:0000:0000:0000:0000/64"],
        refused: &[
            "1111:0000:0000:0000:0000:0000:0000:0000",
            "1111:0000:0000:0000:ffff:ffff:ffff:ffff",
        ],
        admitted: &[
            "1110:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            "1111:0000:0000:0001:0000:0000:0000:0000",
        ],
    },
];

/// The stage that carries NO stack of its own, so nothing is in front of it: the baseline a
/// refusal at any other stage is read against.
const UNPROTECTED_STAGE: &str = "bare";

impl Row {
    /// The name this row's IP set is declared under.
    fn ip_set_name(&self) -> String {
        format!("{}-listed", self.stage)
    }

    /// The name this row's web ACL is declared under.
    fn web_acl_name(&self) -> String {
        format!("{}-acl", self.stage)
    }

    /// The stack this row deploys.
    fn stack_name(&self) -> String {
        format!("waf-protection-{}", self.stage)
    }

    /// The template: the IP set, a web ACL whose one rule BLOCKS what that set lists — naming
    /// it by `Fn::GetAtt <IPSet>.Arn`, so the ARN reaching `CreateWebACL` is the one AWS WAF
    /// minted — and the binding that puts the ACL in front of the stage. The outputs publish
    /// each entity's ARN and the id its own `Get…` takes.
    fn template(&self, api: &str) -> String {
        json!({
            "Resources": {
                "Listed": {
                    "Type": "AWS::WAFv2::IPSet",
                    "Properties": {
                        "Name": self.ip_set_name(),
                        "Scope": SCOPE,
                        "IPAddressVersion": self.version,
                        "Addresses": self.addresses,
                    },
                },
                "Acl": {
                    "Type": "AWS::WAFv2::WebACL",
                    "Properties": {
                        "Name": self.web_acl_name(),
                        "Scope": SCOPE,
                        "DefaultAction": {"Allow": {}},
                        "Rules": [{
                            "Name": "block-listed",
                            "Priority": 0,
                            "Action": {"Block": {}},
                            "Statement": {
                                "IPSetReferenceStatement": {
                                    "Arn": {"Fn::GetAtt": ["Listed", "Arn"]},
                                },
                            },
                            "VisibilityConfig": visibility("block-listed"),
                        }],
                        "VisibilityConfig": visibility(&self.web_acl_name()),
                    },
                },
                "Protection": {
                    "Type": "AWS::WAFv2::WebACLAssociation",
                    "Properties": {
                        "ResourceArn": stage_arn(api, self.stage),
                        "WebACLArn": {"Fn::GetAtt": ["Acl", "Arn"]},
                    },
                },
            },
            "Outputs": {
                "WebACLArn": {"Value": {"Fn::GetAtt": ["Acl", "Arn"]}},
                "WebACLId": {"Value": {"Ref": "Acl"}},
                "IPSetArn": {"Value": {"Fn::GetAtt": ["Listed", "Arn"]}},
                "IPSetId": {"Value": {"Ref": "Listed"}},
            },
        })
        .to_string()
    }
}

/// Build the api every row's stage belongs to: one `GET` resource on a `MOCK` integration that
/// answers `200` with [`ORIGIN_BODY`], deployed into each row's stage and into the
/// unprotected one.
async fn build_api(emulator: &Emulator) -> String {
    let created = emulator
        .api_ok("POST", "/restapis", Some(json!({"name": "waf-protected"})), 201)
        .await;
    let api = created["id"].as_str().expect("the api id").to_string();
    let root = created["rootResourceId"]
        .as_str()
        .expect("the root resource id")
        .to_string();

    let resource = emulator
        .api_ok(
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({"pathPart": ORIGIN_PATH})),
            201,
        )
        .await;
    let resource = resource["id"].as_str().expect("the resource id").to_string();
    emulator
        .api_ok(
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET"),
            Some(json!({"authorizationType": "NONE"})),
            201,
        )
        .await;
    emulator
        .api_ok(
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET/integration"),
            Some(json!({"type": "MOCK"})),
            201,
        )
        .await;
    emulator
        .api_ok(
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET/integration/responses/200"),
            Some(json!({"responseTemplates": {"application/json": ORIGIN_BODY}})),
            201,
        )
        .await;

    for stage in ROWS
        .iter()
        .map(|row| row.stage)
        .chain([UNPROTECTED_STAGE])
    {
        emulator
            .api_ok(
                "POST",
                &format!("/restapis/{api}/deployments"),
                Some(json!({"stageName": stage})),
                201,
            )
            .await;
    }
    api
}

/// The `DescribeStacks` answer read once `stack` has left its in-progress statuses, by LOOKING
/// again until that holds or [`SETTLE_DEADLINE`] passes — never by sleeping for the span a
/// deployment takes.
async fn settled_stack(emulator: &Emulator, case: &str, stack: &str) -> String {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = emulator
            .stack_ok(case, "DescribeStacks", &[("StackName", stack)])
            .await;
        let status = element(&described, "StackStatus");
        if !status.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {stack} was still {status:?} after {SETTLE_DEADLINE:?}, so nothing \
             settled it"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// Every output the settled stack published, with the status it settled in asserted to be
/// `CREATE_COMPLETE` — quoting the resource statuses and their reasons when it is not, since a
/// deployment any resource's create was refused for rolls the whole stack back and those
/// reasons are what the failure is reported with.
async fn published_outputs(
    emulator: &Emulator,
    case: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let described = settled_stack(emulator, case, stack).await;
    let status = element(&described, "StackStatus");
    if status != CREATE_COMPLETE {
        let events = emulator
            .stack_ok(case, "DescribeStackEvents", &[("StackName", stack)])
            .await;
        panic!(
            "case {case:?}: expected {stack} to reach {CREATE_COMPLETE}, got {status:?} with \
             resource statuses {:?} because {:?}",
            elements(&events, "ResourceStatus"),
            elements(&events, "ResourceStatusReason"),
        );
    }
    outputs_of(&described)
}

/// A stack of an IP set, a web ACL that blocks it and the binding to a REST API stage deploys,
/// publishes the identifiers AWS WAF minted, and the protection it deployed decides the
/// requests that stage then answers.
#[tokio::test]
async fn an_iac_deployed_web_acl_blocks_the_addresses_its_ip_set_lists() {
    let emulator = Emulator::new();
    let api = build_api(&emulator).await;

    // Every stack is created first and settled afterwards: each deployment's own clock-owed
    // spans then run alongside the others' rather than one after another.
    for row in ROWS {
        let stack = row.stack_name();
        let template = row.template(&api);
        emulator
            .stack_ok(
                row.case,
                "CreateStack",
                &[("StackName", &stack), ("TemplateBody", &template)],
            )
            .await;
    }

    for row in ROWS {
        let case = row.case;
        let published = published_outputs(&emulator, case, &row.stack_name()).await;
        for key in ["WebACLArn", "WebACLId", "IPSetArn", "IPSetId"] {
            assert!(
                published.get(key).is_some_and(|value| !value.is_empty()),
                "case {case:?}: expected the stack to publish {key}, got {published:?}"
            );
        }
        let ip_set_arn = published["IPSetArn"].clone();
        let ip_set_id = published["IPSetId"].clone();
        let web_acl_arn = published["WebACLArn"].clone();
        let web_acl_id = published["WebACLId"].clone();

        // Each published id is one AWS WAF assigned, and each published ARN carries the triple
        // that service addresses an entity by — so both can be handed straight back to it.
        for (member, id) in [("IPSetId", &ip_set_id), ("WebACLId", &web_acl_id)] {
            assert!(
                is_waf_entity_id(id),
                "case {case:?}: expected the published {member} to be an EntityId, got {id:?}"
            );
        }
        for (member, arn, tail) in [
            (
                "IPSetArn",
                &ip_set_arn,
                format!("/ipset/{}/{ip_set_id}", row.ip_set_name()),
            ),
            (
                "WebACLArn",
                &web_acl_arn,
                format!("/webacl/{}/{web_acl_id}", row.web_acl_name()),
            ),
        ] {
            assert!(
                arn.ends_with(&tail),
                "case {case:?}: expected the published {member} to end in {tail:?}, got {arn:?}"
            );
        }

        // The IP set the stack deployed is one AWS WAF holds, under the id the stack
        // published, with the membership and the family the template declared.
        let read = emulator
            .waf_ok(
                case,
                "GetIPSet",
                json!({"Name": row.ip_set_name(), "Scope": SCOPE, "Id": ip_set_id}),
            )
            .await;
        assert_eq!(
            read.pointer("/IPSet/ARN").and_then(Value::as_str),
            Some(ip_set_arn.as_str()),
            "case {case:?}: expected GetIPSet to report the published ARN, got {read}"
        );
        assert_eq!(
            read.pointer("/IPSet/IPAddressVersion").and_then(Value::as_str),
            Some(row.version),
            "case {case:?}: expected GetIPSet to report IPAddressVersion {:?}, got {read}",
            row.version
        );
        // `IPSet$Addresses` is the SET those strings name, and AWS WAF reports it in its own
        // order rather than the order the array was written in, so what the deployment has to
        // get right is the MEMBERSHIP that reached that service — compared here order-free.
        let mut reported: Vec<&str> = read
            .pointer("/IPSet/Addresses")
            .and_then(Value::as_array)
            .map(|listed| listed.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        reported.sort_unstable();
        let mut declared: Vec<&str> = row.addresses.to_vec();
        declared.sort_unstable();
        assert_eq!(
            reported, declared,
            "case {case:?}: expected GetIPSet to report the declared addresses, got {read}"
        );

        // And the binding the stack made is one AWS WAF holds for that stage.
        let in_front = emulator
            .waf_ok(
                case,
                "GetWebACLForResource",
                json!({"ResourceArn": stage_arn(&api, row.stage)}),
            )
            .await;
        assert_eq!(
            in_front.pointer("/WebACL/ARN").and_then(Value::as_str),
            Some(web_acl_arn.as_str()),
            "case {case:?}: expected the protected stage to report the deployed web ACL, got \
             {in_front}"
        );

        // The protection is LIVE: a request from an address the set lists is refused at the
        // edge, one from an address outside it reaches the origin, and the same listed address
        // reaches the origin at the stage no stack put a web ACL in front of.
        for from in row.refused {
            let answered = emulator.invoke(&api, row.stage, from).await;
            assert_eq!(
                answered.status, 403,
                "case {case:?}: expected a request from the listed {from} to be refused 403, \
                 got {} with {:?}",
                answered.status, answered.text
            );
            assert_eq!(
                answered.body.get("message").and_then(Value::as_str),
                Some(FORBIDDEN),
                "case {case:?}: expected the refusal of {from} to say {FORBIDDEN:?}, got {:?}",
                answered.text
            );

            let unprotected = emulator.invoke(&api, UNPROTECTED_STAGE, from).await;
            assert_eq!(
                unprotected.status, 200,
                "case {case:?}: expected {from} to reach the stage no web ACL is in front of, \
                 got {} with {:?}",
                unprotected.status, unprotected.text
            );
        }
        for from in row.admitted {
            let answered = emulator.invoke(&api, row.stage, from).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected a request from the unlisted {from} to reach the \
                 origin, got {} with {:?}",
                answered.status, answered.text
            );
            assert_eq!(
                answered.body.get("at").and_then(Value::as_str),
                Some(ORIGIN_PATH),
                "case {case:?}: expected {from} to be answered by the origin, got {:?}",
                answered.text
            );
        }
    }
}
