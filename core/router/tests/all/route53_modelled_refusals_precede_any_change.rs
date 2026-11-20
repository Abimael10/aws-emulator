// SPDX-License-Identifier: CC0-1.0

//! Route 53 refuses a request its model carries an error for with THAT error, before it
//! changes anything.
//!
//! The rule under test: where an operation's model declares an error for the condition a
//! request violates, the service answers that error — its own wire code, the HTTP status
//! the model gives it, and a message naming the request values the refusal is about — and
//! the resources the request addressed report exactly what they reported before it, so a
//! refused call is observably a no-op. The generic scaffold answer (HTTP 501
//! `NotImplemented`) is never what such a request gets.
//!
//! Three of the five conditions below can only be decided with another service's state, so
//! only an assembled emulator can pin them, and each service's own suite sees one half:
//!
//! * whether the calling account CREATED the VPC an association names is a fact of its
//!   Amazon EC2 inventory, the one `DescribeVpcs` answers from;
//! * whether the customer managed key a key-signing key names is the sign-and-verify
//!   `ECC_NIST_P256` key DNSSEC signs a zone with is a fact of the Key Management Service
//!   key store;
//! * the principal an unauthorized association is refused in the name of is the identity
//!   `sts:GetCallerIdentity` answers for the SAME credential the request was signed with.
//!
//! The rows differ along what the rule generalises over: which operation refuses, and the
//! axis each condition lives on — the VPC id form and the Region named, the key spec and
//! key usage of the customer managed key, where in a batch the unresolvable id sits and how
//! long the batch is, which structural constraint of the traffic policy document format the
//! document breaks, and which private zone the feature is asked for. Each refusal is paired
//! with a positive control: the same operation, with the condition satisfied, is served and
//! round-trips.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_route53_awsdnsv20130401::state::{
    ACCELERATED_RECOVERY_ENABLED, ACCELERATED_RECOVERY_ENABLING,
};

use crate::query_wire::{
    block_of, blocks_of, element, elements, form_encode, opt_element, xml_escape,
};

/// The Region every request here is signed for, and the Region the VPCs this suite creates
/// therefore belong to.
const REGION: &str = "us-east-1";

/// The endpoint host a Route 53 request addresses — Route 53 is global — which is also the
/// routing signal the router resolves that service from.
const ROUTE53_HOST: &str = "route53.amazonaws.com";

/// The API version prefix every Route 53 path carries.
const API: &str = "/2013-04-01";

/// The namespace a Route 53 request document declares.
const XMLNS: &str = "https://route53.amazonaws.com/doc/2013-04-01/";

/// The `ResourceType` a hosted zone is tagged under (`TagResourceType$hostedzone`).
const HOSTED_ZONE: &str = "hostedzone";

/// The key spec and key usage a key-signing key's customer managed key must have
/// (`KeySigningKey$KmsArn`: "Key spec ECC_NIST_P256", "Key usage Sign and verify").
const SIGNING_KEY_SPEC: &str = "ECC_NIST_P256";
const SIGNING_KEY_USAGE: &str = "SIGN_VERIFY";

/// The scaffold's answer, which no row here may receive: an operation that models a
/// refusal for the condition under test must answer that refusal instead.
const SCAFFOLD_CODE: &str = "NotImplemented";
const SCAFFOLD_STATUS: u16 = 501;

/// The four services this suite exercises, over ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These rows exercise handler behaviour, not SigV4: the access key id is what resolves
    // the caller's identity, and the signature itself is never checked.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` — the signal
/// the router resolves a routing key from, and whose access key id is the credential every
/// request here presents.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0"
    )
}

/// One answered request: its status and its body as text.
struct Answer {
    status: u16,
    text: String,
}

impl Answer {
    /// The wire error code of a refusal (`<ErrorResponse><Error><Code>`), or the empty
    /// string for an answer that carries none.
    fn code(&self) -> String {
        opt_element(&self.text, "Code").unwrap_or_default()
    }

    /// The refusal's message, or the empty string.
    fn message(&self) -> String {
        elements(&self.text, "Message")
            .into_iter()
            .next()
            .unwrap_or_default()
    }
}

/// Send one Route 53 request over its restXml wire.
async fn route53(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{ROUTE53_HOST}{path}"))
        .header(http::header::HOST, ROUTE53_HOST)
        .header(http::header::AUTHORIZATION, authorization("route53"))
        .header(http::header::CONTENT_TYPE, "application/xml")
        .body(bytes::Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        text: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// Send one Route 53 request that must be SERVED, and answer its body.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: &str,
    expected: u16,
) -> String {
    let answer = route53(router, method, path, body).await;
    assert_eq!(
        answer.status, expected,
        "case {case:?}: {method} {path} expected HTTP {expected}, got {} with body {}",
        answer.status, answer.text
    );
    answer.text
}

/// Send one Amazon EC2 request over its ec2Query wire, which must be served.
async fn ec2_query(router: &MultiServiceRouter, case: &str, body: &str) -> String {
    let host = format!("ec2.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header(http::header::HOST, host)
        .header(http::header::AUTHORIZATION, authorization("ec2"))
        .header(
            http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(bytes::Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: the Amazon EC2 call {body} must be served, got {status} with {text}"
    );
    text
}

/// Send one Key Management Service request over its awsJson1_1 wire, which must be served.
async fn kms(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let host = format!("kms.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header(http::header::HOST, host)
        .header(http::header::AUTHORIZATION, authorization("kms"))
        .header(http::header::CONTENT_TYPE, "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: KMS {operation} must be served, got {status} with {payload}"
    );
    payload
}

/// The `Arn` `sts:GetCallerIdentity` answers for the credential this suite signs with, over
/// the token service's own awsQuery wire.
async fn caller_identity_arn(router: &MultiServiceRouter) -> String {
    let host = "sts.amazonaws.com";
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header(http::header::HOST, host)
        .header(http::header::AUTHORIZATION, authorization("sts"))
        .header(
            http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(bytes::Bytes::from(
            "Action=GetCallerIdentity&Version=2011-06-15",
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "GetCallerIdentity must be served, got {status} with {text}"
    );
    element(&text, "Arn")
}

// ---------------------------------------------------------------------------
// Setup through the services' own APIs
// ---------------------------------------------------------------------------

/// Create a VPC the calling account really holds, through Amazon EC2's own wire, and answer
/// its minted id. A VPC of ANOTHER account is simply an id this inventory does not carry.
async fn owned_vpc(router: &MultiServiceRouter, case: &str, cidr_block: &str) -> String {
    let answer = ec2_query(
        router,
        case,
        &format!(
            "Action=CreateVpc&Version=2016-11-15&CidrBlock={}",
            form_encode(cidr_block)
        ),
    )
    .await;
    opt_element(&answer, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc must answer a vpcId, got {answer}"))
}

/// `CreateHostedZone`, answering the BARE (`Z…`) id of the zone it created.
async fn create_zone(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    caller_reference: &str,
    config: &str,
) -> String {
    let body = format!(
        "<CreateHostedZoneRequest xmlns=\"{XMLNS}\"><Name>{name}</Name>\
         <CallerReference>{caller_reference}</CallerReference>{config}</CreateHostedZoneRequest>"
    );
    let text = served(router, case, "POST", &format!("{API}/hostedzone"), &body, 201).await;
    let zone = block_of(&text, "HostedZone")
        .unwrap_or_else(|| panic!("case {case:?}: the create must answer a HostedZone: {text}"));
    element(zone, "Id")
        .trim_start_matches("/hostedzone/")
        .to_string()
}

/// The `HostedZoneConfig` and `VPC` blocks that make a create a PRIVATE zone's.
fn private_config(vpc_id: &str) -> String {
    format!(
        "<HostedZoneConfig><PrivateZone>true</PrivateZone></HostedZoneConfig>\
         <VPC><VPCRegion>{REGION}</VPCRegion><VPCId>{vpc_id}</VPCId></VPC>"
    )
}

/// `GetHostedZone`, which is where an association list and a features setting are read back.
async fn get_zone(router: &MultiServiceRouter, zone_id: &str) -> Answer {
    route53(router, "GET", &format!("{API}/hostedzone/{zone_id}"), "").await
}

/// `GetDNSSEC`, which is where the key-signing keys a zone holds are read back.
async fn get_dnssec(router: &MultiServiceRouter, zone_id: &str) -> Answer {
    route53(
        router,
        "GET",
        &format!("{API}/hostedzone/{zone_id}/dnssec"),
        "",
    )
    .await
}

/// `ListTrafficPolicies`, which is where the policies the account holds are read back.
async fn list_policies(router: &MultiServiceRouter) -> Answer {
    route53(router, "GET", &format!("{API}/trafficpolicies"), "").await
}

/// `AssociateVPCWithHostedZone` naming one VPC.
async fn associate(
    router: &MultiServiceRouter,
    zone_id: &str,
    vpc_id: &str,
    vpc_region: &str,
) -> Answer {
    route53(
        router,
        "POST",
        &format!("{API}/hostedzone/{zone_id}/associatevpc"),
        &format!(
            "<AssociateVPCWithHostedZoneRequest xmlns=\"{XMLNS}\">\
             <VPC><VPCRegion>{vpc_region}</VPCRegion><VPCId>{vpc_id}</VPCId></VPC>\
             </AssociateVPCWithHostedZoneRequest>"
        ),
    )
    .await
}

/// `CreateKeySigningKey` over the customer managed key `kms_arn`.
async fn create_ksk(
    router: &MultiServiceRouter,
    zone_id: &str,
    name: &str,
    status: &str,
    kms_arn: &str,
) -> Answer {
    route53(
        router,
        "POST",
        &format!("{API}/keysigningkey"),
        &format!(
            "<CreateKeySigningKeyRequest xmlns=\"{XMLNS}\">\
             <CallerReference>ksk-{name}</CallerReference><HostedZoneId>{zone_id}</HostedZoneId>\
             <KeyManagementServiceArn>{}</KeyManagementServiceArn><Name>{name}</Name>\
             <Status>{status}</Status></CreateKeySigningKeyRequest>",
            xml_escape(kms_arn)
        ),
    )
    .await
}

/// `ListTagsForResources` over a batch of hosted-zone ids.
async fn list_tags_batch(router: &MultiServiceRouter, ids: &[String]) -> Answer {
    let named: String = ids
        .iter()
        .map(|id| format!("<ResourceId>{}</ResourceId>", xml_escape(id)))
        .collect();
    route53(
        router,
        "POST",
        &format!("{API}/tags/{HOSTED_ZONE}"),
        &format!(
            "<ListTagsForResourcesRequest xmlns=\"{XMLNS}\"><ResourceIds>{named}</ResourceIds>\
             </ListTagsForResourcesRequest>"
        ),
    )
    .await
}

/// `ChangeTagsForResource`, adding one tag to a hosted zone.
async fn tag_zone(router: &MultiServiceRouter, case: &str, zone_id: &str, key: &str, value: &str) {
    served(
        router,
        case,
        "POST",
        &format!("{API}/tags/{HOSTED_ZONE}/{zone_id}"),
        &format!(
            "<ChangeTagsForResourceRequest xmlns=\"{XMLNS}\"><AddTags>\
             <Tag><Key>{key}</Key><Value>{value}</Value></Tag></AddTags>\
             </ChangeTagsForResourceRequest>"
        ),
        200,
    )
    .await;
}

/// `CreateTrafficPolicy` carrying one document.
async fn create_policy(router: &MultiServiceRouter, name: &str, document: &str) -> Answer {
    route53(
        router,
        "POST",
        &format!("{API}/trafficpolicy"),
        &format!(
            "<CreateTrafficPolicyRequest xmlns=\"{XMLNS}\"><Name>{name}</Name>\
             <Document>{}</Document></CreateTrafficPolicyRequest>",
            xml_escape(document)
        ),
    )
    .await
}

/// `UpdateHostedZoneFeatures`, asking for one setting of the one feature it configures.
async fn update_features(router: &MultiServiceRouter, zone_id: &str, enable: bool) -> Answer {
    route53(
        router,
        "POST",
        &format!("{API}/hostedzone/{zone_id}/features"),
        &format!(
            "<UpdateHostedZoneFeaturesRequest xmlns=\"{XMLNS}\">\
             <EnableAcceleratedRecovery>{enable}</EnableAcceleratedRecovery>\
             </UpdateHostedZoneFeaturesRequest>"
        ),
    )
    .await
}

/// A traffic policy document the format DOES define: a version, a record type, the
/// endpoints it can route to and exactly one entry point into them.
fn well_formed_document(record_type: &str, value: &str) -> String {
    format!(
        "{{\"AWSPolicyFormatVersion\":\"2015-10-01\",\"RecordType\":\"{record_type}\",\
         \"StartEndpoint\":\"only\",\
         \"Endpoints\":{{\"only\":{{\"Type\":\"value\",\"Value\":\"{value}\"}}}}}}"
    )
}

// ---------------------------------------------------------------------------
// The refusal table
// ---------------------------------------------------------------------------

/// The condition a row's request violates, and the request that violates it.
#[derive(Debug, Clone, Copy)]
enum Violation {
    /// `AssociateVPCWithHostedZone` naming a VPC the calling account did not create and
    /// the zone holds no authorization for.
    UnauthorizedVpc {
        vpc_id: &'static str,
        vpc_region: &'static str,
    },
    /// `CreateKeySigningKey` naming a customer managed key that is not a usable
    /// sign-and-verify `ECC_NIST_P256` key.
    UnusableSigningKey {
        key_spec: &'static str,
        key_usage: &'static str,
    },
    /// `ListTagsForResources` over a batch one of whose ids names no hosted zone.
    /// `ids` spells the created zone's id as `{known}`.
    BatchNamingAnUnknownZone { ids: &'static [&'static str] },
    /// `CreateTrafficPolicy` with a document the traffic policy document format does not
    /// define.
    MalformedPolicyDocument { document: &'static str },
    /// `UpdateHostedZoneFeatures` turning accelerated recovery on for a private zone.
    AcceleratedRecoveryOnAPrivateZone { zone_name: &'static str },
}

/// One row: the violation, the error class the model declares for it, and the status the
/// model gives that class.
struct RefusalRow {
    case: &'static str,
    operation: &'static str,
    violation: Violation,
    code: &'static str,
    status: u16,
    /// The WHOLE message, where every part of it is fixed text. `None` where the message
    /// carries a value only the run knows — a minted ARN, a minted id, the caller's own
    /// identity — which [`assert_request_derived_message`] asserts instead.
    message: Option<&'static str>,
}

/// The rows, spanning the five conditions and the axis each one lives on.
///
/// Every `code`/`status` pair is the model's own
/// (`smithy_query(query="operation",operation="<the operation>")`): `NotAuthorizedException`
/// httpError 401, `InvalidKMSArn` 400, `NoSuchHostedZone` 404,
/// `InvalidTrafficPolicyDocument` 400 and `InvalidInput` 400.
const REFUSAL_ROWS: &[RefusalRow] = &[
    RefusalRow {
        case: "a foreign VPC in the long id form, in the zone's own Region",
        operation: "AssociateVPCWithHostedZone",
        violation: Violation::UnauthorizedVpc {
            vpc_id: "vpc-0a1b2c3d4e5f60718",
            vpc_region: REGION,
        },
        code: "NotAuthorizedException",
        status: 401,
        message: None,
    },
    RefusalRow {
        case: "a foreign VPC in the 8-character id form, in another Region",
        operation: "AssociateVPCWithHostedZone",
        violation: Violation::UnauthorizedVpc {
            vpc_id: "vpc-deadbeef",
            vpc_region: "eu-west-2",
        },
        code: "NotAuthorizedException",
        status: 401,
        message: None,
    },
    RefusalRow {
        // A third Region of the `aws` partition, kept to that partition deliberately: how a
        // VPC of another partition is addressed is a question about the ARN the shared engine
        // builds, which this suite is not about.
        case: "a foreign VPC in a third Region of the same partition",
        operation: "AssociateVPCWithHostedZone",
        violation: Violation::UnauthorizedVpc {
            vpc_id: "vpc-00112233445566778",
            vpc_region: "ap-southeast-2",
        },
        code: "NotAuthorizedException",
        status: 401,
        message: None,
    },
    RefusalRow {
        case: "a sign-and-verify RSA key, which is not the curve DNSSEC signs with",
        operation: "CreateKeySigningKey",
        violation: Violation::UnusableSigningKey {
            key_spec: "RSA_2048",
            key_usage: SIGNING_KEY_USAGE,
        },
        code: "InvalidKMSArn",
        status: 400,
        message: None,
    },
    RefusalRow {
        // The same 256-bit point width as the required curve, so what is checked has to
        // be the key's SPEC rather than the shape of the public key it publishes.
        case: "a sign-and-verify key on another 256-bit curve",
        operation: "CreateKeySigningKey",
        violation: Violation::UnusableSigningKey {
            key_spec: "ECC_SECG_P256K1",
            key_usage: SIGNING_KEY_USAGE,
        },
        code: "InvalidKMSArn",
        status: 400,
        message: None,
    },
    RefusalRow {
        case: "a P-256 key whose usage is key agreement rather than sign and verify",
        operation: "CreateKeySigningKey",
        violation: Violation::UnusableSigningKey {
            key_spec: SIGNING_KEY_SPEC,
            key_usage: "KEY_AGREEMENT",
        },
        code: "InvalidKMSArn",
        status: 400,
        message: None,
    },
    RefusalRow {
        case: "a two-id batch whose second id names no zone",
        operation: "ListTagsForResources",
        violation: Violation::BatchNamingAnUnknownZone {
            ids: &["{known}", "Z00000000000000000000"],
        },
        code: "NoSuchHostedZone",
        status: 404,
        message: Some("No hosted zone found with ID: Z00000000000000000000"),
    },
    RefusalRow {
        case: "a two-id batch whose FIRST id names no zone",
        operation: "ListTagsForResources",
        violation: Violation::BatchNamingAnUnknownZone {
            ids: &["ZNOSUCHZONE0000001", "{known}"],
        },
        code: "NoSuchHostedZone",
        status: 404,
        message: Some("No hosted zone found with ID: ZNOSUCHZONE0000001"),
    },
    RefusalRow {
        case: "a four-id batch whose unresolvable id sits in the middle",
        operation: "ListTagsForResources",
        violation: Violation::BatchNamingAnUnknownZone {
            ids: &["{known}", "{known}", "ZNOSUCHZONE0000002", "{known}"],
        },
        code: "NoSuchHostedZone",
        status: 404,
        message: Some("No hosted zone found with ID: ZNOSUCHZONE0000002"),
    },
    RefusalRow {
        case: "a document declaring neither endpoints nor an entry point",
        operation: "CreateTrafficPolicy",
        violation: Violation::MalformedPolicyDocument {
            document: "{\"AWSPolicyFormatVersion\":\"2015-10-01\",\"RecordType\":\"A\"}",
        },
        code: "InvalidTrafficPolicyDocument",
        status: 400,
        message: Some(
            "At least one endpoint must be declared.;Either StartRule or StartEndpoint must \
             be declared, but not both",
        ),
    },
    RefusalRow {
        case: "a document declaring BOTH entry points",
        operation: "CreateTrafficPolicy",
        violation: Violation::MalformedPolicyDocument {
            document: "{\"AWSPolicyFormatVersion\":\"2015-10-01\",\"RecordType\":\"A\",\
                       \"StartEndpoint\":\"only\",\"StartRule\":\"rule\",\
                       \"Endpoints\":{\"only\":{\"Type\":\"value\",\"Value\":\"192.0.2.1\"}},\
                       \"Rules\":{\"rule\":{\"RuleType\":\"failover\"}}}",
        },
        code: "InvalidTrafficPolicyDocument",
        status: 400,
        message: Some("Either StartRule or StartEndpoint must be declared, but not both"),
    },
    RefusalRow {
        case: "a document that is not JSON at all",
        operation: "CreateTrafficPolicy",
        violation: Violation::MalformedPolicyDocument {
            document: "not a document",
        },
        code: "InvalidTrafficPolicyDocument",
        status: 400,
        message: Some("The traffic policy document must be declared as valid JSON."),
    },
    RefusalRow {
        case: "accelerated recovery asked for on a two-label private zone",
        operation: "UpdateHostedZoneFeatures",
        violation: Violation::AcceleratedRecoveryOnAPrivateZone {
            zone_name: "internal.example.com",
        },
        code: "InvalidInput",
        status: 400,
        message: Some("Private hosted zones cannot enable accelerated recovery"),
    },
    RefusalRow {
        case: "accelerated recovery asked for on a four-label private zone",
        operation: "UpdateHostedZoneFeatures",
        violation: Violation::AcceleratedRecoveryOnAPrivateZone {
            zone_name: "zone.sub.internal.example.net",
        },
        code: "InvalidInput",
        status: 400,
        message: Some("Private hosted zones cannot enable accelerated recovery"),
    },
];

/// What one row produced: the refusal, and the SAME read taken before and after it.
struct Outcome {
    refusal: Answer,
    /// The body of the read the rule is about, before the violating request.
    before: String,
    /// The same read, after it.
    after: String,
    /// The value the refusal's message is derived from, where only the run knows it: the
    /// ARN of the customer managed key the row created.
    derived_from: Option<String>,
}

/// Drive one row: stand up the emulator, put the resource in place through the services'
/// own APIs, read what the rule is about, send the violating request, read it again.
async fn drive(router: &MultiServiceRouter, row: &RefusalRow, reference: &str) -> Outcome {
    let case = row.case;
    match row.violation {
        Violation::UnauthorizedVpc { vpc_id, vpc_region } => {
            let held = owned_vpc(router, case, "10.21.0.0/16").await;
            let zone = create_zone(
                router,
                case,
                "private.example.com",
                reference,
                &private_config(&held),
            )
            .await;
            let before = get_zone(router, &zone).await.text;
            let refusal = associate(router, &zone, vpc_id, vpc_region).await;
            let after = get_zone(router, &zone).await.text;
            Outcome {
                refusal,
                before,
                after,
                derived_from: None,
            }
        }
        Violation::UnusableSigningKey {
            key_spec,
            key_usage,
        } => {
            let zone = create_zone(router, case, "signed.example.com", reference, "").await;
            let created = kms(
                router,
                case,
                "CreateKey",
                &json!({ "KeySpec": key_spec, "KeyUsage": key_usage }),
            )
            .await;
            let kms_arn = created["KeyMetadata"]["Arn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: CreateKey must answer an ARN: {created}"))
                .to_string();
            let before = get_dnssec(router, &zone).await.text;
            let refusal = create_ksk(router, &zone, "refused_key", "INACTIVE", &kms_arn).await;
            let after = get_dnssec(router, &zone).await.text;
            assert!(
                !after.contains("refused_key"),
                "case {case:?}: a refused create must leave no key-signing key behind, got {after}"
            );
            Outcome {
                refusal,
                before,
                after,
                derived_from: Some(kms_arn),
            }
        }
        Violation::BatchNamingAnUnknownZone { ids } => {
            let zone = create_zone(router, case, "tagged.example.com", reference, "").await;
            tag_zone(router, case, &zone, "owner", "dns-team").await;
            let batch: Vec<String> = ids.iter().map(|id| id.replace("{known}", &zone)).collect();
            let known = vec![zone.clone()];
            let before = list_tags_batch(router, &known).await.text;
            let refusal = list_tags_batch(router, &batch).await;
            let after = list_tags_batch(router, &known).await.text;
            assert!(
                blocks_of(&refusal.text, "ResourceTagSet").is_empty(),
                "case {case:?}: a refused batch must answer no partial tag set, got {}",
                refusal.text
            );
            Outcome {
                refusal,
                before,
                after,
                derived_from: None,
            }
        }
        Violation::MalformedPolicyDocument { document } => {
            // One policy the format DOES define is already there, so "nothing changed" is
            // about this create rather than about an empty store.
            let kept = create_policy(router, "kept", &well_formed_document("A", "192.0.2.10")).await;
            assert_eq!(
                kept.status, 201,
                "case {case:?}: a well-formed document must be created, got {} with {}",
                kept.status, kept.text
            );
            let before = list_policies(router).await.text;
            let refusal = create_policy(router, "refused", document).await;
            let after = list_policies(router).await.text;
            Outcome {
                refusal,
                before,
                after,
                derived_from: None,
            }
        }
        Violation::AcceleratedRecoveryOnAPrivateZone { zone_name } => {
            let held = owned_vpc(router, case, "10.22.0.0/16").await;
            let zone = create_zone(
                router,
                case,
                zone_name,
                reference,
                &private_config(&held),
            )
            .await;
            let before = get_zone(router, &zone).await.text;
            let refusal = update_features(router, &zone, true).await;
            let after = get_zone(router, &zone).await.text;
            Outcome {
                refusal,
                before,
                after,
                derived_from: None,
            }
        }
    }
}

/// Assert the part of a refusal's message that is derived from the run: the ARN the request
/// named, or the principal the token service answers for this credential.
async fn assert_request_derived_message(
    router: &MultiServiceRouter,
    row: &RefusalRow,
    outcome: &Outcome,
) {
    let case = row.case;
    let message = outcome.refusal.message();
    match row.violation {
        Violation::UnauthorizedVpc { vpc_id, vpc_region } => {
            // The denial names the principal that signed the request, the IAM action it
            // performs and the VPC it named, as that VPC's own Amazon EC2 resource ARN in
            // the Region the request said it was created in. The principal is read back
            // from the token service rather than spelled out here, so the two services
            // must agree on one identity for one credential.
            let caller = caller_identity_arn(router).await;
            let account = ::engine::emulator_account_id();
            let expected = format!(
                "User: {caller} is not authorized to perform: route53:AssociateVPCWithHostedZone \
                 on resource: arn:aws:ec2:{vpc_region}:{account}:vpc/{vpc_id}"
            );
            assert_eq!(
                message, expected,
                "case {case:?}: expected the message {expected:?}, got {message:?}"
            );
        }
        Violation::UnusableSigningKey { .. } => {
            let arn = outcome
                .derived_from
                .as_deref()
                .expect("a key-signing-key row carries the ARN it named");
            let expected = format!(
                "The customer managed key with ARN '{arn}' could not be used by Route 53 DNSSEC. \
                 Check to make sure that the customer managed key has not been deleted. \
                 Otherwise, review its configurations and key policy, to make sure it is in \
                 compliance with Route 53 DNSSEC requirements."
            );
            assert_eq!(
                message, expected,
                "case {case:?}: expected the message {expected:?}, got {message:?}"
            );
        }
        _ => {
            let expected = row
                .message
                .expect("every other row states the whole message it owes");
            assert_eq!(
                message, expected,
                "case {case:?}: expected the message {expected:?}, got {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_modelled_refusal_is_answered_and_nothing_is_changed_by_it() {
    for (index, row) in REFUSAL_ROWS.iter().enumerate() {
        let case = row.case;
        let router = build_router();
        let outcome = drive(&router, row, &format!("refusal-{index}")).await;
        let refusal = &outcome.refusal;

        // The scaffold's answer is never one of these: the operation models a refusal for
        // exactly this condition.
        assert_ne!(
            refusal.status, SCAFFOLD_STATUS,
            "case {case:?}: {} must not answer the scaffold's status, got body {}",
            row.operation, refusal.text
        );
        assert_ne!(
            refusal.code(),
            SCAFFOLD_CODE,
            "case {case:?}: {} must not answer {SCAFFOLD_CODE}, got body {}",
            row.operation,
            refusal.text
        );

        assert_eq!(
            refusal.status, row.status,
            "case {case:?}: {} expected HTTP {}, got {} with body {}",
            row.operation, row.status, refusal.status, refusal.text
        );
        assert_eq!(
            refusal.code(),
            row.code,
            "case {case:?}: {} expected the code {}, got {:?} with body {}",
            row.operation,
            row.code,
            refusal.code(),
            refusal.text
        );
        assert_request_derived_message(&router, row, &outcome).await;

        // And the refusal changed nothing: the read the rule is about answers exactly what
        // it answered before the request.
        assert_eq!(
            outcome.after, outcome.before,
            "case {case:?}: the refused {} must leave the read it addressed byte-identical, \
             got {:?} after {:?}",
            row.operation, outcome.after, outcome.before
        );
    }
}

// ---------------------------------------------------------------------------
// The positive controls: the same operation, with the condition satisfied
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_association_of_a_vpc_the_account_created_is_applied() {
    let case = "a second VPC of the account";
    let router = build_router();
    let first = owned_vpc(&router, case, "10.31.0.0/16").await;
    let second = owned_vpc(&router, case, "10.32.0.0/16").await;
    let zone = create_zone(
        &router,
        case,
        "private.example.com",
        "control-associate",
        &private_config(&first),
    )
    .await;

    let applied = associate(&router, &zone, &second, REGION).await;
    assert_eq!(
        applied.status, 200,
        "case {case:?}: expected HTTP 200, got {} with body {}",
        applied.status, applied.text
    );
    let read = get_zone(&router, &zone).await;
    let reported = elements(&read.text, "VPCId");
    assert_eq!(
        reported,
        vec![first.clone(), second.clone()],
        "case {case:?}: the zone must report both VPCs it holds, got {reported:?} with body {}",
        read.text
    );
}

#[tokio::test]
async fn a_key_signing_key_over_a_usable_customer_managed_key_is_created_and_read_back() {
    let case = "the sign-and-verify P-256 key DNSSEC signs with";
    let router = build_router();
    let zone = create_zone(&router, case, "signed.example.com", "control-ksk", "").await;
    let created = kms(
        &router,
        case,
        "CreateKey",
        &json!({ "KeySpec": SIGNING_KEY_SPEC, "KeyUsage": SIGNING_KEY_USAGE }),
    )
    .await;
    let kms_arn = created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateKey must answer an ARN: {created}"))
        .to_string();

    let made = create_ksk(&router, &zone, "control_key", "INACTIVE", &kms_arn).await;
    assert_eq!(
        made.status, 201,
        "case {case:?}: expected HTTP 201, got {} with body {}",
        made.status, made.text
    );
    let key = block_of(&made.text, "KeySigningKey")
        .unwrap_or_else(|| panic!("case {case:?}: the create must answer a key: {}", made.text));
    // The create echoes the Name and the initial Status the request asked for.
    for (member, expected) in [("Name", "control_key"), ("Status", "INACTIVE")] {
        let got = element(key, member);
        assert_eq!(
            got, expected,
            "case {case:?}: the created key's {member} must be {expected:?}, got {got:?}"
        );
    }

    // And the key the create answered is the key the zone PERSISTED.
    let read = get_dnssec(&router, &zone).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected HTTP 200 from GetDNSSEC, got {} with body {}",
        read.status, read.text
    );
    let reported = elements(&read.text, "Name");
    assert_eq!(
        reported,
        vec!["control_key".to_string()],
        "case {case:?}: GetDNSSEC must report the created key, got {reported:?} with body {}",
        read.text
    );
    let reported_arn = elements(&read.text, "KmsArn");
    assert_eq!(
        reported_arn,
        vec![kms_arn.clone()],
        "case {case:?}: the reported key must name the customer managed key it publishes, got \
         {reported_arn:?} with body {}",
        read.text
    );
}

#[tokio::test]
async fn a_batch_of_ids_that_all_resolve_reports_one_tag_set_each() {
    // The rows span the batch sizes the model allows: `ResourceIds` targets
    // `TagResourceIdList`, which is `len 1..10`
    // (smithy_query(query="shape",shape="TagResourceIdList")), so one id is its low bound.
    let router = build_router();
    let mut ids = Vec::new();
    for index in 0..3 {
        let case = format!("zone {index}");
        let zone = create_zone(
            &router,
            &case,
            &format!("tagged-{index}.example.com"),
            &format!("control-batch-{index}"),
            "",
        )
        .await;
        tag_zone(&router, &case, &zone, "owner", &format!("team-{index}")).await;
        ids.push(zone);
    }

    for size in [1_usize, 2, 3] {
        let case = format!("a batch of {size} id(s)");
        let batch = ids[..size].to_vec();
        let read = list_tags_batch(&router, &batch).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected HTTP 200, got {} with body {}",
            read.status, read.text
        );
        let sets = blocks_of(&read.text, "ResourceTagSet");
        assert_eq!(
            sets.len(),
            size,
            "case {case:?}: expected {size} tag set(s), got {} with body {}",
            sets.len(),
            read.text
        );
        for (index, set) in sets.iter().enumerate() {
            let reported = element(set, "ResourceId");
            assert_eq!(
                reported, batch[index],
                "case {case:?}: the tag sets must come back in request order, got {reported:?} \
                 at position {index}"
            );
            let value = element(set, "Value");
            assert_eq!(
                value,
                format!("team-{index}"),
                "case {case:?}: the tag set must carry the zone's own tag, got {value:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_traffic_policy_document_the_format_defines_round_trips_byte_identical() {
    // The rows differ along the record type the document declares and the value its one
    // endpoint routes to, so what round-trips is the document rather than one string.
    let router = build_router();
    let rows = [
        ("an A record document", "A", "192.0.2.25"),
        ("an AAAA record document", "AAAA", "2001:db8::1"),
        ("a CNAME record document", "CNAME", "target.example.com"),
    ];

    for (case, record_type, value) in rows {
        let document = well_formed_document(record_type, value);
        let created = create_policy(&router, &format!("control-{record_type}"), &document).await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected HTTP 201, got {} with body {}",
            created.status, created.text
        );
        let policy = block_of(&created.text, "TrafficPolicy").unwrap_or_else(|| {
            panic!(
                "case {case:?}: the create must answer a TrafficPolicy: {}",
                created.text
            )
        });
        let id = element(policy, "Id");
        let version = element(policy, "Version");
        let reported_type = element(policy, "Type");
        assert_eq!(
            reported_type, record_type,
            "case {case:?}: the policy's type is the one its document declares, got \
             {reported_type:?}"
        );

        let read = route53(
            &router,
            "GET",
            &format!("{API}/trafficpolicy/{id}/{version}"),
            "",
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected HTTP 200 from GetTrafficPolicy, got {} with body {}",
            read.status, read.text
        );
        let round_tripped = elements(&read.text, "Document")
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the read must answer the Document: {}",
                    read.text
                )
            });
        assert_eq!(
            round_tripped, document,
            "case {case:?}: the stored document must come back byte-identical, got \
             {round_tripped:?}"
        );
    }
}

#[tokio::test]
async fn accelerated_recovery_asked_for_on_a_public_zone_is_served() {
    // The feature is public-zone-only, so the SAME request the private rows are refused is
    // served here, and the zone starts moving to the setting it asked for. The rows differ
    // along the zone, the one axis the refusal is keyed on.
    let router = build_router();
    let rows = [
        ("a two-label public zone", "public.example.com"),
        ("a four-label public zone", "zone.sub.public.example.net"),
    ];

    for (case, name) in rows {
        let zone = create_zone(&router, case, name, &format!("control-features-{name}"), "").await;
        let answer = update_features(&router, &zone, true).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} with body {}",
            answer.status, answer.text
        );
        let read = get_zone(&router, &zone).await;
        let status = opt_element(&read.text, "AcceleratedRecoveryStatus").unwrap_or_default();
        // Either the moving status the update left, or the settled one the clock has since
        // made due: both are the setting the request asked for, and which of the two it is
        // depends on nothing this test controls.
        assert!(
            status == ACCELERATED_RECOVERY_ENABLING || status == ACCELERATED_RECOVERY_ENABLED,
            "case {case:?}: expected {ACCELERATED_RECOVERY_ENABLING} or \
             {ACCELERATED_RECOVERY_ENABLED}, got {status:?} with body {}",
            read.text
        );
    }
}
