// SPDX-License-Identifier: CC0-1.0

//! The hosted zone an `AWS::CertificateManager::Certificate` names for a covered name really
//! receives that name's DNS validation record, so the certificate CloudFormation requested is
//! proven and issued without anybody publishing a record by hand.
//!
//! The rule under test, which three crates have to agree on (CloudFormation makes the calls,
//! ACM mints the certificate and the challenge it hands out, Route 53 holds the zone the
//! challenge is published in), so the suite lives here rather than in any of them:
//!
//! **A `DomainValidationOptions` entry's `HostedZoneId` is no member of the request ACM
//! serves — that shape carries the domain and the mail suffix — it is CloudFormation's own
//! instruction: the provider publishes the challenge record ACM handed out for that covered
//! name, as ACM worded it, in THAT zone and in no other, while the request is still waiting
//! on proof of domain control. A zone the record cannot be written in fails the resource,
//! which keeps the identifier ACM minted — the certificate exists, and the stack's row for it
//! is the only handle a client has on it.**
//!
//! The rows of the first table differ along what the rule generalises over: how many covered
//! names the certificate has, how many zones they are spread over, whether two names share one
//! zone, the FORM of the covered name (an apex name, a subdomain, a wildcard), whether the
//! zone is named literally or by a `Ref` to a zone of the same template, and the Region the
//! stack deploys in. The second table's rows differ along WHY the named zone cannot hold the
//! record: a sibling zone, a zone below the name, and a zone id Route 53 holds no zone for.
//! The `Ref` case goes on to take the stack DOWN, since the record the provider published is
//! the provider's to withdraw.
//!
//! Every expected value is computed from what the row declares, except the certificate's ARN
//! and its challenge record, which are ACM's to mint: those are read from ACM and then
//! required of the zone and of the stack's row.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{element, elements, form_encode, opt_element};

/// How long the suite goes on polling for an outcome that is already due, and how often: the
/// spans behind these outcomes are the stack's own (`STATUS_TRANSITION_DELAY`) and the one ACM
/// owes a request whose evidence it has observed (`VALIDATION_SETTLE_DELAY`), and a loaded
/// machine can be far slower than either without the rule being any different.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLLS: usize = 400;

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The record type ACM's DNS challenge is published as ("the CNAME record that you add to your
/// DNS database for domain validation", `DomainValidation$ResourceRecord`).
const CNAME: &str = "CNAME";

/// The `TTL` the provider publishes the challenge record with, in seconds.
const VALIDATION_RECORD_TTL: &str = "300";

/// The `CertificateStatus` values the rows expect, from the model's own enum.
const PENDING_VALIDATION: &str = "PENDING_VALIDATION";
const ISSUED: &str = "ISSUED";

/// The `ResourceStatus` values the rule is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The `HandlerErrorCode` the provider reports a refusal of the change batch under: the
/// plugin's class for an exception of the called service it has no mapping for.
const GENERAL_SERVICE_EXCEPTION: &str = "GeneralServiceException";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
///
/// Request authentication is off, as in every other suite here; the in-process calls
/// CloudFormation makes sign themselves either way (the credential scope is what routes them).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_acm_certificatemanager::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
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
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one ACM operation over its awsJson1_1 wire, signed for `region`.
async fn acm(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("acm.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CertificateManager.{operation}"))
        .header("authorization", authorization("acm", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Call one Route 53 operation over its restXml wire, signed for `region`.
async fn route53(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: &str,
) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("route53.{region}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", authorization("route53", region))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Create one PUBLIC hosted zone for `name` and answer the bare id Route 53 minted, which is
/// what a template states as a `HostedZoneId` and what every call that takes a zone id takes.
async fn create_zone(router: &MultiServiceRouter, region: &str, name: &str, case: &str) -> String {
    let (status, body) = route53(
        router,
        region,
        "POST",
        "/2013-04-01/hostedzone",
        &format!(
            "<CreateHostedZoneRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
             <Name>{name}</Name><CallerReference>{case}-{name}</CallerReference>\
             </CreateHostedZoneRequest>"
        ),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from CreateHostedZone for {name:?}, got {status} with \
         body {body}"
    );
    element(&body, "Id")
        .trim_start_matches("/hostedzone/")
        .to_string()
}

/// The `(ResourceStatus, PhysicalResourceId, ResourceStatusReason, body)` the stack reports for
/// one of its resources, read through `DescribeStackResource` so the answer carries that
/// resource alone.
async fn stack_resource(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
    logical_id: &str,
) -> (String, String, String, String) {
    let (_, body) = cloudformation(
        router,
        region,
        "DescribeStackResource",
        &[("StackName", stack), ("LogicalResourceId", logical_id)],
    )
    .await;
    (
        opt_element(&body, "ResourceStatus").unwrap_or_default(),
        opt_element(&body, "PhysicalResourceId").unwrap_or_default(),
        opt_element(&body, "ResourceStatusReason").unwrap_or_default(),
        body,
    )
}

/// Poll one resource of the stack until it reports `wanted`, or answer the last reading.
///
/// Waits for the CONDITION rather than for a span: the outcome is owed as soon as the services
/// behind it have answered, so a slower machine takes more polls and not a different answer.
async fn wait_for_resource(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
    logical_id: &str,
    wanted: &str,
) -> (String, String, String, String) {
    let mut last = stack_resource(router, region, stack, logical_id).await;
    for _ in 0..POLLS {
        if last.0 == wanted {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        last = stack_resource(router, region, stack, logical_id).await;
    }
    last
}

/// Poll a resource until it reports a physical id at all, or answer the last reading.
async fn wait_for_physical_id(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
    logical_id: &str,
) -> (String, String, String, String) {
    let mut last = stack_resource(router, region, stack, logical_id).await;
    for _ in 0..POLLS {
        if !last.1.is_empty() {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        last = stack_resource(router, region, stack, logical_id).await;
    }
    last
}

/// Whether `arn` satisfies `DescribeCertificate`'s `CertificateArn` modelled `@pattern`
/// (`^arn:[\w+=/,.@-]+:acm:[\w+=/,.@-]*:[0-9]+:[\w+=,.@-]+(/[\w+=,.@-]+)*$`) and its modelled
/// length (20..2048) for `region` and the account this emulator mints.
fn is_well_formed_certificate_arn(arn: &str, region: &str) -> bool {
    let prefix = format!("arn:aws:acm:{region}:{ACCOUNT}:certificate/");
    let Some(id) = arn.strip_prefix(&prefix) else {
        return false;
    };
    (20..=2048).contains(&arn.len())
        && !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+=,.@-".contains(c))
}

/// The challenge record ACM reports for each covered name of the certificate `arn`, as the
/// `(name, value)` a client is to publish — read from ACM, never composed here.
async fn challenge_records(
    router: &MultiServiceRouter,
    region: &str,
    arn: &str,
    case: &str,
) -> BTreeMap<String, (String, String)> {
    let (status, answer) = acm(
        router,
        region,
        "DescribeCertificate",
        &json!({ "CertificateArn": arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeCertificate on the ARN the stack \
         reported, got {status} with body {answer}"
    );
    let validations = answer["Certificate"]["DomainValidationOptions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut records = BTreeMap::new();
    for validation in validations {
        let Some(domain) = validation["DomainName"].as_str() else {
            continue;
        };
        let record = &validation["ResourceRecord"];
        let (Some(name), Some(record_type), Some(value)) = (
            record["Name"].as_str(),
            record["Type"].as_str(),
            record["Value"].as_str(),
        ) else {
            continue;
        };
        assert_eq!(
            record_type, CNAME,
            "case {case:?}: expected ACM's DNS challenge to be a {CNAME}, got {record_type:?}"
        );
        records.insert(
            domain.to_string(),
            (name.to_string(), value.to_string()),
        );
    }
    records
}

/// The record set of `name` and type `CNAME` the zone `zone` holds, as the
/// `(name, values, ttl)` a client reading the zone sees, or `None` where the zone holds no such
/// record set.
async fn held_record(
    router: &MultiServiceRouter,
    region: &str,
    zone: &str,
    name: &str,
    case: &str,
) -> Option<(String, Vec<String>, String)> {
    let (status, body) = route53(
        router,
        region,
        "GET",
        &format!(
            "/2013-04-01/hostedzone/{zone}/rrset?name={}&type={CNAME}&maxitems=1",
            form_encode(name)
        ),
        "",
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListResourceRecordSets in {zone}, got {status} \
         with body {body}"
    );
    let held = opt_element(&body, "Name")?;
    if held != name {
        return None;
    }
    Some((
        held,
        elements(&body, "Value"),
        opt_element(&body, "TTL").unwrap_or_default(),
    ))
}

// ---------------------------------------------------------------------------
// The named zone takes the challenge record, and no other zone does
// ---------------------------------------------------------------------------

/// One covered name of a row's certificate: the name, and the index into the row's zones of the
/// zone its `DomainValidationOptions` entry names.
type CoveredName = (&'static str, usize);

/// One row of the publication table.
struct Row {
    case: &'static str,
    region: &'static str,
    /// The zones created before the stack, by DNS name. Every one of them is read after the
    /// deployment, so a record in a zone the declaration did not name for that name is caught.
    zones: &'static [&'static str],
    /// The certificate's `DomainName`, with the zone its entry names.
    domain: CoveredName,
    /// Its `SubjectAlternativeNames`, each with the zone its own entry names.
    alternatives: &'static [CoveredName],
}

const ROWS: &[Row] = &[
    Row {
        case: "one covered name, in its own zone",
        region: "us-east-1",
        // A reserved top-level label (RFC 2606), which the public DNS delegates to nobody, so
        // the account's own public zone is the authority for it — which is what lets ACM
        // resolve the challenge it handed out.
        zones: &["cfn-acm-one.example"],
        domain: ("cfn-acm-one.example", 0),
        alternatives: &[],
    },
    Row {
        case: "two covered names, one zone each",
        region: "eu-west-1",
        zones: &["cfn-acm-two-a.example", "cfn-acm-two-b.example"],
        domain: ("cfn-acm-two-a.example", 0),
        alternatives: &[("shop.cfn-acm-two-b.example", 1)],
    },
    Row {
        case: "an apex name and a subdomain of it, sharing one zone",
        region: "ap-southeast-2",
        zones: &["cfn-acm-three.example"],
        domain: ("cfn-acm-three.example", 0),
        alternatives: &[("api.cfn-acm-three.example", 0)],
    },
    Row {
        case: "a wildcard name beside the apex it covers, in the apex's zone",
        region: "us-west-2",
        zones: &["cfn-acm-four.example"],
        domain: ("cfn-acm-four.example", 0),
        alternatives: &[("*.cfn-acm-four.example", 0)],
    },
];

/// Every row: the zone each covered name's entry names holds exactly the challenge record ACM
/// handed out for that name, no other zone of the row holds it, and the certificate ACM then
/// proves is the one the stack reports created under the ARN ACM minted.
#[tokio::test]
async fn the_named_hosted_zone_takes_the_certificates_validation_record() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("acm-validation-zones-{index}");
        let mut zone_ids = Vec::new();
        for zone in row.zones {
            zone_ids.push(create_zone(&router, region, zone, case).await);
        }
        let covered: Vec<CoveredName> = ::std::iter::once(row.domain)
            .chain(row.alternatives.iter().copied())
            .collect();
        let options: Vec<Value> = covered
            .iter()
            .map(|(name, zone)| {
                json!({ "DomainName": name, "HostedZoneId": zone_ids[*zone] })
            })
            .collect();
        let mut properties = json!({
            "DomainName": row.domain.0,
            "ValidationMethod": "DNS",
            "DomainValidationOptions": options,
        });
        if !row.alternatives.is_empty() {
            properties["SubjectAlternativeNames"] = json!(row
                .alternatives
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<&str>>());
        }
        let template = json!({
            "Resources": {
                "Certificate": {
                    "Type": "AWS::CertificateManager::Certificate",
                    "Properties": properties
                }
            }
        })
        .to_string();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        // The handle the stack publishes is the ARN ACM minted, from the first reading onwards.
        let (_, certificate_arn, _, read) =
            wait_for_physical_id(&router, region, &stack_name, "Certificate").await;
        assert!(
            is_well_formed_certificate_arn(&certificate_arn, region),
            "case {case:?}: expected an ACM certificate ARN in {region}, got \
             {certificate_arn:?} in {read}"
        );

        // What ACM asks a client to publish for each covered name — read from ACM, so the zone
        // is judged against the challenge that service really handed out.
        let challenges = challenge_records(&router, region, &certificate_arn, case).await;
        let expected_names: Vec<&str> = covered.iter().map(|(name, _)| *name).collect();
        let reported: Vec<&str> = challenges.keys().map(String::as_str).collect();
        let mut sorted = expected_names.clone();
        sorted.sort_unstable();
        assert_eq!(
            reported, sorted,
            "case {case:?}: expected ACM to report a DNS challenge for every covered name \
             {sorted:?}, got {reported:?}"
        );

        for (name, zone) in &covered {
            let (challenge_name, challenge_value) = challenges
                .get(*name)
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: no challenge for {name:?}"));
            // The zone the entry named holds it, with the value ACM published and a TTL.
            let held = held_record(
                &router,
                region,
                &zone_ids[*zone],
                &challenge_name,
                case,
            )
            .await;
            let (held_name, held_values, held_ttl) = held.unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the zone {:?} to hold the challenge record \
                     {challenge_name:?} for {name:?}, it holds no such record set",
                    row.zones[*zone]
                )
            });
            assert_eq!(
                held_name, challenge_name,
                "case {case:?}: expected the record set's name to be ACM's challenge name \
                 {challenge_name:?}, got {held_name:?}"
            );
            assert_eq!(
                held_values,
                vec![challenge_value.clone()],
                "case {case:?}: expected the zone {:?} to hold ACM's challenge value \
                 {challenge_value:?} for {name:?}, got {held_values:?}",
                row.zones[*zone]
            );
            assert_eq!(
                held_ttl, VALIDATION_RECORD_TTL,
                "case {case:?}: expected the challenge record to carry TTL \
                 {VALIDATION_RECORD_TTL}, got {held_ttl:?}"
            );
            // ...and no zone the entry did NOT name holds it.
            for (other, other_id) in row.zones.iter().zip(&zone_ids) {
                if other_id == &zone_ids[*zone] {
                    continue;
                }
                let elsewhere =
                    held_record(&router, region, other_id, &challenge_name, case).await;
                assert!(
                    elsewhere.is_none(),
                    "case {case:?}: expected only the zone {:?} to hold the challenge record \
                     for {name:?}, the zone {other:?} holds {elsewhere:?}",
                    row.zones[*zone]
                );
            }
        }

        // The published records are the proof ACM was waiting for, so the certificate reaches
        // the state it is usable in and the resource is reported created — under the same ARN.
        let (resource_status, reported_id, _, read) =
            wait_for_resource(&router, region, &stack_name, "Certificate", CREATE_COMPLETE).await;
        assert_eq!(
            resource_status, CREATE_COMPLETE,
            "case {case:?}: expected the certificate resource to reach {CREATE_COMPLETE} once \
             its challenge records are published, got {read}"
        );
        assert_eq!(
            reported_id, certificate_arn,
            "case {case:?}: expected the resource to keep reporting the ARN ACM minted, got \
             {reported_id:?}"
        );
        let (status, answer) = acm(
            &router,
            region,
            "DescribeCertificate",
            &json!({ "CertificateArn": certificate_arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeCertificate, got {status} with body \
             {answer}"
        );
        assert_eq!(
            answer["Certificate"]["Status"].as_str(),
            Some(ISSUED),
            "case {case:?}: expected ACM to report the certificate {ISSUED} once every \
             challenge is published, got {answer}"
        );
    }
}

/// A zone the SAME template declares is named by a `Ref`, which resolves to the id Route 53
/// minted: the certificate's challenge record lands in that zone, so a template can bring the
/// zone and the certificate up together.
#[tokio::test]
async fn a_zone_of_the_same_template_takes_the_challenge_record() {
    let router = build_router();
    let case = "the hosted zone declared beside the certificate";
    let region = "eu-central-1";
    let domain = "cfn-acm-same-template.example";
    let template = json!({
        "Resources": {
            "Zone": {
                "Type": "AWS::Route53::HostedZone",
                "Properties": { "Name": domain }
            },
            "Certificate": {
                "Type": "AWS::CertificateManager::Certificate",
                "Properties": {
                    "DomainName": domain,
                    "ValidationMethod": "DNS",
                    "DomainValidationOptions": [
                        { "DomainName": domain, "HostedZoneId": { "Ref": "Zone" } }
                    ]
                }
            }
        },
        "Outputs": { "ZoneId": { "Value": { "Ref": "Zone" } } }
    })
    .to_string();

    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[
            ("StackName", "acm-validation-same-template"),
            ("TemplateBody", &template),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );

    let (_, zone_id, _, read) =
        wait_for_physical_id(&router, region, "acm-validation-same-template", "Zone").await;
    assert!(
        !zone_id.is_empty(),
        "case {case:?}: expected the hosted zone to report the id Route 53 minted, got {read}"
    );
    let (_, certificate_arn, _, read) = wait_for_physical_id(
        &router,
        region,
        "acm-validation-same-template",
        "Certificate",
    )
    .await;
    assert!(
        is_well_formed_certificate_arn(&certificate_arn, region),
        "case {case:?}: expected an ACM certificate ARN in {region}, got {certificate_arn:?} \
         in {read}"
    );

    let challenges = challenge_records(&router, region, &certificate_arn, case).await;
    let (challenge_name, challenge_value) = challenges
        .get(domain)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no challenge for {domain:?}"));
    let held = held_record(&router, region, &zone_id, &challenge_name, case).await;
    let (_, held_values, _) = held.unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the zone the template declared to hold the challenge \
             record {challenge_name:?}, it holds no such record set"
        )
    });
    assert_eq!(
        held_values,
        vec![challenge_value.clone()],
        "case {case:?}: expected the declared zone to hold ACM's challenge value \
         {challenge_value:?}, got {held_values:?}"
    );

    let (resource_status, _, _, read) = wait_for_resource(
        &router,
        region,
        "acm-validation-same-template",
        "Certificate",
        CREATE_COMPLETE,
    )
    .await;
    assert_eq!(
        resource_status, CREATE_COMPLETE,
        "case {case:?}: expected the certificate resource to reach {CREATE_COMPLETE}, got {read}"
    );

    // Taking the stack down withdraws what the provider published: the certificate's removal
    // takes the challenge record out of the zone and then deletes the certificate, and only
    // then can the zone itself go — Route 53 deletes a hosted zone "only if it contains no
    // resource record sets other than the default SOA record and NS resource record sets", so
    // a zone that is gone is the proof the record was withdrawn.
    let (status, body) = cloudformation(
        &router,
        region,
        "DeleteStack",
        &[("StackName", "acm-validation-same-template")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
    );
    let mut certificate_gone = false;
    let mut zone_gone = false;
    let mut last = String::new();
    for _ in 0..POLLS {
        let (certificate_status, answer) = acm(
            &router,
            region,
            "DescribeCertificate",
            &json!({ "CertificateArn": certificate_arn }),
        )
        .await;
        let (zone_status, zone) = route53(
            &router,
            region,
            "GET",
            &format!("/2013-04-01/hostedzone/{zone_id}"),
            "",
        )
        .await;
        certificate_gone = certificate_status != 200;
        zone_gone = zone_status != 200;
        last = format!(
            "certificate: {certificate_status} {answer}; zone: {zone_status} {zone}"
        );
        if certificate_gone && zone_gone {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert!(
        certificate_gone,
        "case {case:?}: expected ACM to hold no certificate for {certificate_arn:?} after the \
         stack was deleted, got {last}"
    );
    assert!(
        zone_gone,
        "case {case:?}: expected Route 53 to hold no zone {zone_id:?} after the stack was \
         deleted, which it cannot delete while the challenge record is still published, got \
         {last}"
    );
    let (_, described) = cloudformation(
        &router,
        region,
        "DescribeStacks",
        &[("StackName", "acm-validation-same-template")],
    )
    .await;
    assert!(
        !described.contains("DELETE_FAILED"),
        "case {case:?}: expected the stack's teardown to go through, got {described}"
    );
}

// ---------------------------------------------------------------------------
// A zone that cannot hold the record fails the resource, ARN and all
// ---------------------------------------------------------------------------

/// One row of the refusal table: why the zone the declaration names cannot hold the covered
/// name's challenge record.
struct RefusedRow {
    case: &'static str,
    region: &'static str,
    /// The zone the certificate's own name lives in, which is what makes the request one ACM
    /// carries into domain validation at all.
    own_zone: &'static str,
    /// The further zone the row creates, if any, and whether the declaration names THAT zone
    /// rather than a zone id Route 53 holds nothing for.
    other_zone: Option<&'static str>,
    /// The `HandlerErrorCode` the provider reports the refusal under.
    handler_code: &'static str,
}

const REFUSED_ROWS: &[RefusedRow] = &[
    RefusedRow {
        case: "a sibling zone, which is not authoritative for the domain",
        region: "us-east-1",
        own_zone: "cfn-acm-neg-one.example",
        other_zone: Some("cfn-acm-other-one.example"),
        // Route 53 refuses the change batch, which the provider has no mapping for.
        handler_code: GENERAL_SERVICE_EXCEPTION,
    },
    RefusedRow {
        case: "a zone BELOW the domain, which the record's name sits above",
        region: "eu-west-1",
        own_zone: "cfn-acm-neg-two.example",
        other_zone: Some("under.cfn-acm-neg-two.example"),
        handler_code: GENERAL_SERVICE_EXCEPTION,
    },
    RefusedRow {
        case: "a zone id Route 53 holds no zone for",
        region: "ap-southeast-2",
        own_zone: "cfn-acm-neg-three.example",
        other_zone: None,
        // A declaration naming a zone that is not there is a fault in the request, not the
        // certificate this handler manages going missing.
        handler_code: "InvalidRequest",
    },
];

/// A zone id Route 53 holds no zone for, in the form the service's `ResourceId` admits.
const ABSENT_ZONE_ID: &str = "Z0ABSENTZONE00000000";

/// Every row: the resource fails, and the identifier on its failed row is the ARN ACM minted —
/// which ACM still resolves to the certificate, the only handle a client has on it — while
/// neither zone holds the record the refused batch would have written.
#[tokio::test]
async fn a_zone_that_cannot_hold_the_record_fails_the_resource_under_the_minted_arn() {
    let router = build_router();

    for (index, row) in REFUSED_ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("acm-validation-refused-{index}");
        let own_zone_id = create_zone(&router, region, row.own_zone, case).await;
        let named_zone_id = match row.other_zone {
            Some(other) => create_zone(&router, region, other, case).await,
            None => ABSENT_ZONE_ID.to_string(),
        };
        let template = json!({
            "Resources": {
                "Certificate": {
                    "Type": "AWS::CertificateManager::Certificate",
                    "Properties": {
                        "DomainName": row.own_zone,
                        "ValidationMethod": "DNS",
                        "DomainValidationOptions": [
                            { "DomainName": row.own_zone, "HostedZoneId": named_zone_id }
                        ]
                    }
                }
            }
        })
        .to_string();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &template),
                // The failed resource is left in place rather than rolled back, which is what
                // keeps its row readable — the row being what this rule is about.
                ("OnFailure", "DO_NOTHING"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let (resource_status, certificate_arn, reason, read) =
            wait_for_resource(&router, region, &stack_name, "Certificate", CREATE_FAILED).await;
        assert_eq!(
            resource_status, CREATE_FAILED,
            "case {case:?}: expected the certificate resource to reach {CREATE_FAILED}, got \
             {read}"
        );
        assert!(
            is_well_formed_certificate_arn(&certificate_arn, region),
            "case {case:?}: expected the failed row to keep the ACM certificate ARN the \
             create minted, got {certificate_arn:?} in {read}"
        );
        assert!(
            reason.contains(&format!("HandlerErrorCode: {}", row.handler_code)),
            "case {case:?}: expected the failure to be reported under HandlerErrorCode {}, \
             got {reason:?}",
            row.handler_code
        );

        // The certificate ACM minted is still there, under that ARN: the resource failed after
        // the certificate existed, so the identifier on the row resolves.
        let (status, answer) = acm(
            &router,
            region,
            "DescribeCertificate",
            &json!({ "CertificateArn": certificate_arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeCertificate on the id the failed \
             row carries, got {status} with body {answer}"
        );
        assert_eq!(
            answer["Certificate"]["Status"].as_str(),
            Some(PENDING_VALIDATION),
            "case {case:?}: expected the certificate to be left waiting on proof of domain \
             control, got {answer}"
        );
        let challenge_name = answer["Certificate"]["DomainValidationOptions"][0]["ResourceRecord"]
            ["Name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !challenge_name.is_empty(),
            "case {case:?}: expected ACM to report the challenge record it handed out, got \
             {answer}"
        );

        // Nothing was published: the refused batch wrote no record set in the zone it named,
        // and none in the zone the certificate's own name lives in either.
        for (zone_name, zone_id) in [
            (row.own_zone, own_zone_id.as_str()),
            (row.other_zone.unwrap_or(ABSENT_ZONE_ID), named_zone_id.as_str()),
        ] {
            if zone_id == ABSENT_ZONE_ID {
                continue;
            }
            let held = held_record(&router, region, zone_id, &challenge_name, case).await;
            assert!(
                held.is_none(),
                "case {case:?}: expected no challenge record in the zone {zone_name:?} after \
                 the refused change batch, it holds {held:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The failed row keeps the minted identifier, and says whose refusal failed it
// ---------------------------------------------------------------------------

/// One row of the kept-identifier table: a declaration whose named zone cannot hold the
/// covered name's challenge record, read through the LIST form of the resource read and taken
/// down afterwards.
///
/// The rows differ along what the rule generalises over: why the named zone cannot hold the
/// record, the FORM of the covered name (a zone apex, a name inside the zone, a wildcard over
/// it), the Region, and which `CreateStack` member leaves the failed resource in place — the
/// one that disables the rollback and the one that asks for nothing to be done on failure,
/// since what the row carries is the same either way.
struct KeptRow {
    case: &'static str,
    region: &'static str,
    /// The zone the covered name itself lives in, which is what lets ACM hand out a challenge
    /// for it at all.
    own_zone: &'static str,
    /// The name the certificate covers.
    domain: &'static str,
    /// The zone the declaration names for that covered name, which the challenge record's own
    /// name does not sit in.
    named_zone: &'static str,
    /// The `CreateStack` member that leaves the failed resource in place rather than rolling
    /// it back, so its row stays readable.
    kept_by: (&'static str, &'static str),
}

const KEPT_ROWS: &[KeptRow] = &[
    KeptRow {
        case: "a sibling zone, the failed create left in place by DisableRollback",
        region: "us-east-2",
        own_zone: "cfn-acm-kept-one.example",
        domain: "cfn-acm-kept-one.example",
        named_zone: "cfn-acm-kept-other.example",
        kept_by: ("DisableRollback", "true"),
    },
    KeptRow {
        case: "a zone below the covered name, the failed create left in place by OnFailure",
        region: "eu-west-2",
        own_zone: "cfn-acm-kept-two.example",
        domain: "api.cfn-acm-kept-two.example",
        named_zone: "under.cfn-acm-kept-two.example",
        kept_by: ("OnFailure", "DO_NOTHING"),
    },
    KeptRow {
        case: "a wildcard covered name whose challenge the named sibling zone cannot hold",
        region: "ap-northeast-1",
        own_zone: "cfn-acm-kept-three.example",
        domain: "*.cfn-acm-kept-three.example",
        named_zone: "cfn-acm-kept-three-other.example",
        kept_by: ("DisableRollback", "true"),
    },
];

/// The name of the hosted zone `zone_id`, as Route 53 reports it — fully qualified, which is
/// how a refusal about that zone names it.
async fn hosted_zone_name(
    router: &MultiServiceRouter,
    region: &str,
    zone_id: &str,
    case: &str,
) -> String {
    let (status, body) = route53(
        router,
        region,
        "GET",
        &format!("/2013-04-01/hostedzone/{zone_id}"),
        "",
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetHostedZone for {zone_id}, got {status} with \
         body {body}"
    );
    element(&body, "Name").to_string()
}

/// Every row: the failed resource's row carries the ARN ACM minted — in the LIST read a client
/// makes of it as well as in the single read — its `ResourceStatusReason` is the refusal of the
/// service that would not take the record, worded with ACM's own challenge name and the named
/// zone's own name, and the certificate behind that identifier is still there, waiting on proof
/// of domain control, until the stack is taken down — after which ACM resolves the identifier
/// to nothing.
#[tokio::test]
async fn a_failed_certificate_row_keeps_the_minted_arn_and_quotes_the_refusal() {
    let router = build_router();

    for (index, row) in KEPT_ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("acm-validation-kept-{index}");
        let _own_zone_id = create_zone(&router, region, row.own_zone, case).await;
        let named_zone_id = create_zone(&router, region, row.named_zone, case).await;
        let template = json!({
            "Resources": {
                "Certificate": {
                    "Type": "AWS::CertificateManager::Certificate",
                    "Properties": {
                        "DomainName": row.domain,
                        "ValidationMethod": "DNS",
                        "DomainValidationOptions": [
                            { "DomainName": row.domain, "HostedZoneId": named_zone_id }
                        ]
                    }
                }
            }
        })
        .to_string();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &template),
                (row.kept_by.0, row.kept_by.1),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let (resource_status, certificate_arn, _, read) =
            wait_for_resource(&router, region, &stack_name, "Certificate", CREATE_FAILED).await;
        assert_eq!(
            resource_status, CREATE_FAILED,
            "case {case:?}: expected the certificate resource to reach {CREATE_FAILED}, got \
             {read}"
        );
        assert!(
            is_well_formed_certificate_arn(&certificate_arn, region),
            "case {case:?}: expected the failed row to keep the ACM certificate ARN the create \
             minted, got {certificate_arn:?} in {read}"
        );
        // The reason is read through the DECODING reader: it quotes another service's message
        // inside its own, so the answer carries that message's quotes as entities.
        let reason = elements(&read, "ResourceStatusReason")
            .first()
            .cloned()
            .unwrap_or_default();

        // The LIST read of the stack's resources carries the same identifier: the handle on the
        // certificate does not depend on which read a client makes.
        let (status, listed) = cloudformation(
            &router,
            region,
            "DescribeStackResources",
            &[
                ("StackName", &stack_name),
                ("LogicalResourceId", "Certificate"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             body {listed}"
        );
        assert_eq!(
            opt_element(&listed, "ResourceStatus").as_deref(),
            Some(CREATE_FAILED),
            "case {case:?}: expected the listed row to report {CREATE_FAILED}, got {listed}"
        );
        assert_eq!(
            opt_element(&listed, "PhysicalResourceId").as_deref(),
            Some(certificate_arn.as_str()),
            "case {case:?}: expected the listed row to carry the minted ARN \
             {certificate_arn:?}, got {listed}"
        );

        // That identifier is a usable ACM handle: the certificate exists and is still waiting
        // on proof of domain control, because what failed was the publication of the proof.
        let (status, answer) = acm(
            &router,
            region,
            "DescribeCertificate",
            &json!({ "CertificateArn": certificate_arn.clone() }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeCertificate on the id the failed row \
             carries, got {status} with body {answer}"
        );
        assert_eq!(
            answer["Certificate"]["CertificateArn"].as_str(),
            Some(certificate_arn.as_str()),
            "case {case:?}: expected ACM to resolve the row's identifier to that certificate, \
             got {answer}"
        );
        assert_eq!(
            answer["Certificate"]["Status"].as_str(),
            Some(PENDING_VALIDATION),
            "case {case:?}: expected the certificate to be left in {PENDING_VALIDATION}, got \
             {answer}"
        );

        // The reason is the refusal of the service that would not take the record, worded with
        // the challenge name ACM handed out and the name of the zone that refused it — both
        // read back here rather than composed. Everything past the call's own identifier is
        // per-run (the request id, the attempt count, the resource request's token).
        let challenges = challenge_records(&router, region, &certificate_arn, case).await;
        let (challenge_name, _) = challenges
            .get(row.domain)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no challenge for {:?}", row.domain));
        let refusing_zone = hosted_zone_name(&router, region, &named_zone_id, case).await;
        let opening = format!(
            "Resource handler returned message: \"[RRSet with DNS name {challenge_name} is not \
             permitted in zone {refusing_zone}] (Service: Route53, Status Code: 400, Request \
             ID: "
        );
        assert!(
            reason.starts_with(&opening),
            "case {case:?}: expected the failed row's reason to open {opening:?}, got {reason:?}"
        );
        assert!(
            reason.contains("(SDK Attempt Count: "),
            "case {case:?}: expected the reason to report the attempts the call was made in, \
             got {reason:?}"
        );
        assert!(
            reason.ends_with(&format!(", HandlerErrorCode: {GENERAL_SERVICE_EXCEPTION})")),
            "case {case:?}: expected the reason to close under HandlerErrorCode \
             {GENERAL_SERVICE_EXCEPTION}, got {reason:?}"
        );

        // Taking the stack down deletes the certificate it left behind, so the identifier the
        // failed row published resolves to nothing afterwards.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        let mut last = (200, Value::Null);
        for _ in 0..POLLS {
            last = acm(
                &router,
                region,
                "DescribeCertificate",
                &json!({ "CertificateArn": certificate_arn.clone() }),
            )
            .await;
            if last.0 != 200 {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        let (status, gone) = last;
        assert_eq!(
            status, 400,
            "case {case:?}: expected ACM to hold no certificate for {certificate_arn:?} once \
             the stack was deleted, got {status} with body {gone}"
        );
        assert_eq!(
            gone["__type"]
                .as_str()
                .unwrap_or_default()
                .split('#')
                .next_back(),
            Some("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException for the deleted certificate, got \
             {gone}"
        );
        let expected_message = format!("Could not find certificate {certificate_arn}.");
        assert_eq!(
            gone["message"].as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the refusal to name the certificate it could not find, got \
             {gone}"
        );
    }
}
