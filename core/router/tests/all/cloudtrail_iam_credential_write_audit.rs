// SPDX-License-Identifier: CC0-1.0

//! The CloudTrail record of an IAM credential write, read back through `LookupEvents`.
//!
//! RULE: a service that serves a control-plane call records it in the account's event
//! history, where the record's `requestParameters` / `responseElements` carry the members of
//! that call's own modelled request and response EXCEPT the ones holding credential
//! material, which are left OUT of the document entirely — no key for them anywhere in it,
//! not a key carrying a placeholder — and the record is filed in the Region the serving
//! service reports its events from: the partition's leader Region for a GLOBAL service
//! (IAM), the Region the call was served in for a regional one (CloudTrail itself). So a
//! client auditing credential creation finds the call under its own `eventName` with
//! everything but the secret, and finds it in one Region whatever Region it signed the write
//! for.
//!
//! Only a cross-service suite can pin this: the records are written by the service that
//! SERVES the call (IAM, awsQuery) and read back through `cloudtrail:LookupEvents`
//! (awsJson1_1) — two services over ONE engine, as `core/router/src/lib.rs` mounts them.
//! `iam_management_events.rs` covers the presence of those records and the absence of the
//! two secrets; what this suite adds is the MEMBER SET a record carries at each of those
//! documents (so a withheld member cannot come back as a placeholder, and a member the call
//! did carry cannot go missing), that no key of a withheld member appears anywhere in the
//! document, and the Region pairing read through a lookup in each Region — including
//! CloudTrail's own regional call, which is filed in the Region it was served in.
//!
//! The rows span what the rule generalises over rather than the one call that motivated it:
//! which SIDE of the call the credential material is on (a password a request carries, a
//! secret a response answers with, a seed and a QR image of one create), the FORM of the
//! material (a console password, a long-term secret, a service-specific password, a shared
//! MFA seed), the call kind (two creates, an update), and — for the Region — a global
//! service's write, a global service's read, and a regional service's read in two different
//! Regions.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudtrail_cloudtrail_20131101::service::management_events::EVENT_SOURCE as CLOUDTRAIL_EVENT_SOURCE;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::service::management_events::{
    EVENT_REGION as IAM_EVENT_REGION, EVENT_SOURCE as IAM_EVENT_SOURCE, OMITTED_MEMBERS,
};

use crate::query_wire::{element, form_encode};

/// IAM's API version, which every awsQuery form body of it carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// A Region that is NOT the one a global service's records are filed in, used both for a
/// write signed elsewhere and for CloudTrail's own regional call.
const OTHER_REGION: &str = "eu-west-1";

/// A third Region, so the regional rows below differ from each other as well as from the
/// leader Region.
const THIRD_REGION: &str = "us-west-2";

/// The literal CloudTrail stores in place of a REDACTED member's value. An IAM record
/// carries it nowhere — the member is omitted instead — so it is named here to be asserted
/// ABSENT.
const REDACTED: &str = "HIDDEN_DUE_TO_SECURITY_REASONS";

/// The user every credential write below is made for.
const USER_NAME: &str = "credential-write-audit";

/// The console password the login-profile create carries, and the one its update replaces it
/// with. Values that appear nowhere else, so finding either in a record is unambiguous.
const CREATED_PASSWORD: &str = "cr3ated-not-in-the-trail";
const UPDATED_PASSWORD: &str = "upd4ted-not-in-the-trail";

/// The service a service-specific credential is issued for: one of the services IAM issues
/// them for (`crate::state::service_with_credentials` in the IAM crate).
const CREDENTIAL_SERVICE: &str = "codecommit.amazonaws.com";

/// How wide a window the lookups below ask for, in seconds either side of the call. Generous
/// on purpose: the window is here to exercise `StartTime` / `EndTime`, not to time anything,
/// so nothing about the suite depends on how fast the host runs.
const LOOKUP_WINDOW_SECONDS: i64 = 3_600;

/// What one record must carry, and what it must not.
struct Recorded {
    case: &'static str,
    /// The operation's modelled name, which is the record's `eventName`.
    event_name: &'static str,
    /// Each document of the record whose member set is pinned EXACTLY, as a JSON pointer and
    /// the member names a client must see there — the recorded spelling of the modelled
    /// members the call carried, minus the withheld ones.
    exactly: Vec<(&'static str, Vec<&'static str>)>,
    /// Pointers the record must not resolve at all.
    absent: Vec<&'static str>,
    /// Pointers the record must resolve: what the call carried and the record keeps.
    present: Vec<&'static str>,
    /// The credential material of this call, which no record may carry anywhere.
    secret: String,
}

/// The two services this record travels through, over ONE shared engine: IAM serves and
/// records the calls, CloudTrail reads the records back.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential scope
    // is still carried, because it is what names the Region a call is served in.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` and `region`, which is
/// what routes the request and what names the Region it was served in. The signature is
/// never checked here.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDAUDITROUTER/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, \
         Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One IAM awsQuery call made in `region`, as the status and the XML body it answered.
async fn iam(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={IAM_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("iam.{region}.amazonaws.com"))
        .header("authorization", authorization("iam", region))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One IAM call that must be served, as the XML body it answered.
async fn iam_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = iam(router, region, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: IAM {action} in {region} must be served, got {status} with {body:?}"
    );
    body
}

/// One CloudTrail `awsJson1_1` call made in `region`, as the status and the parsed body.
async fn cloudtrail(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudtrail.{region}.amazonaws.com"))
        .header("authorization", authorization("cloudtrail", region))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CloudTrail_20131101.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `cloudtrail:LookupEvents` in `region`, filtered on one attribute over a window holding
/// every call this suite makes, as each matching event's parsed `CloudTrailEvent` document.
async fn looked_up(
    router: &MultiServiceRouter,
    region: &str,
    attribute_key: &str,
    attribute_value: &str,
) -> Vec<Value> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock at or after the epoch")
        .as_secs() as i64;
    let (status, answered) = cloudtrail(
        router,
        region,
        "LookupEvents",
        &json!({
            "LookupAttributes": [{
                "AttributeKey": attribute_key,
                "AttributeValue": attribute_value,
            }],
            "StartTime": now - LOOKUP_WINDOW_SECONDS,
            "EndTime": now + LOOKUP_WINDOW_SECONDS,
            "MaxResults": 50,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "LookupEvents({attribute_key}={attribute_value}) in {region} must be served, got \
         {status} with {answered}"
    );
    answered["Events"]
        .as_array()
        .unwrap_or_else(|| panic!("LookupEvents reports an Events list, got {answered}"))
        .iter()
        .map(|event| {
            let document = event["CloudTrailEvent"]
                .as_str()
                .unwrap_or_else(|| panic!("an event carries its CloudTrailEvent: {event}"));
            serde_json::from_str(document)
                .unwrap_or_else(|error| panic!("CloudTrailEvent is JSON ({error}): {document}"))
        })
        .collect()
}

/// The one record of `event_name` a lookup in `region` reports.
async fn record_of(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    event_name: &str,
) -> Value {
    let found = looked_up(router, region, "EventName", event_name).await;
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one {event_name} record in {region}, got {found:?}"
    );
    found.into_iter().next().expect("the one record")
}

/// The member names a record carries at `pointer`, sorted — the SET a client reading that
/// document sees.
fn member_names(record: &Value, pointer: &str) -> Vec<String> {
    let mut names: Vec<String> = record
        .pointer(pointer)
        .and_then(Value::as_object)
        .map(|members| members.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Every key that appears anywhere in `value`, however deeply nested.
fn keys_anywhere(value: &Value, into: &mut BTreeSet<String>) {
    match value {
        Value::Object(members) => {
            for (name, held) in members {
                into.insert(name.clone());
                keys_anywhere(held, into);
            }
        }
        Value::Array(elements) => {
            for element in elements {
                keys_anywhere(element, into);
            }
        }
        _ => {}
    }
}

/// The name a record spells a modelled member with: the modelled name with a lower-case
/// first letter and the rest unchanged.
///
/// The same reading as the recorder's own (`service_common::management_events`'s
/// `recorded_member_name`), written here because this test crate cannot depend on
/// `services/common`: `core/router/Cargo.toml` is a pinned path, as `query_wire.rs` says of
/// its own encoder.
fn recorded_member_name(modelled: &str) -> String {
    let mut characters = modelled.chars();
    match characters.next() {
        None => String::new(),
        Some(first) => first.to_lowercase().collect::<String>() + characters.as_str(),
    }
}

#[tokio::test]
async fn an_iam_credential_write_is_recorded_without_its_credential_material() {
    let router = router();

    // The user every write below is made for. Created in the leader Region; the key create
    // is made in ANOTHER Region, so the records' Region is the serving service's own.
    iam_ok(
        &router,
        "setup",
        IAM_EVENT_REGION,
        "CreateUser",
        &[("UserName", USER_NAME)],
    )
    .await;

    let mut rows: Vec<Recorded> = Vec::new();

    // The long-term secret a key create ANSWERS with: the withholding is on the response
    // side, and the recorded member set of the key is the modelled `AccessKey` minus that
    // one member (`smithy_query(query="shape", shape="AccessKey")` -> UserName, AccessKeyId,
    // Status, SecretAccessKey, CreateDate).
    let minted = iam_ok(
        &router,
        "the secret an access-key create answers with",
        OTHER_REGION,
        "CreateAccessKey",
        &[("UserName", USER_NAME)],
    )
    .await;
    let minted_secret = element(&minted, "SecretAccessKey");
    let minted_key_id = element(&minted, "AccessKeyId");
    assert!(
        !minted_secret.is_empty() && !minted_key_id.is_empty(),
        "case {:?}: the create answers the caller with a key and a secret, got {minted:?}",
        "the secret an access-key create answers with"
    );
    rows.push(Recorded {
        case: "the secret an access-key create answers with",
        event_name: "CreateAccessKey",
        exactly: vec![
            ("/requestParameters", vec!["userName"]),
            (
                "/responseElements/accessKey",
                vec!["accessKeyId", "createDate", "status", "userName"],
            ),
        ],
        absent: vec!["/responseElements/accessKey/secretAccessKey"],
        present: vec![
            "/responseElements/accessKey/accessKeyId",
            "/responseElements/accessKey/createDate",
        ],
        secret: minted_secret.clone(),
    });

    // A console password a REQUEST carries, on the create that sets it: the recorded
    // parameters are the modelled `CreateLoginProfileRequest` minus `Password`
    // (`smithy_query(query="shape", shape="CreateLoginProfileRequest")` -> UserName,
    // Password, PasswordResetRequired), and the profile the create answers with carries no
    // password member at all.
    iam_ok(
        &router,
        "a console password on the create that sets it",
        IAM_EVENT_REGION,
        "CreateLoginProfile",
        &[
            ("UserName", USER_NAME),
            ("Password", CREATED_PASSWORD),
            ("PasswordResetRequired", "false"),
        ],
    )
    .await;
    rows.push(Recorded {
        case: "a console password on the create that sets it",
        event_name: "CreateLoginProfile",
        exactly: vec![
            ("/requestParameters", vec!["passwordResetRequired", "userName"]),
            (
                "/responseElements/loginProfile",
                vec!["createDate", "passwordResetRequired", "userName"],
            ),
        ],
        absent: vec!["/requestParameters/password"],
        present: vec![
            "/requestParameters/userName",
            "/responseElements/loginProfile/userName",
        ],
        secret: CREATED_PASSWORD.to_string(),
    });

    // The same password member on the UPDATE that replaces it: the member set is the one the
    // request carried, which here is the user alone.
    iam_ok(
        &router,
        "the same password member on the update that replaces it",
        THIRD_REGION,
        "UpdateLoginProfile",
        &[("UserName", USER_NAME), ("Password", UPDATED_PASSWORD)],
    )
    .await;
    rows.push(Recorded {
        case: "the same password member on the update that replaces it",
        event_name: "UpdateLoginProfile",
        exactly: vec![("/requestParameters", vec!["userName"])],
        absent: vec!["/requestParameters/password"],
        present: vec!["/requestParameters/userName"],
        secret: UPDATED_PASSWORD.to_string(),
    });

    // The generated password of a service-specific credential: credential material on the
    // response side again, in a different member of a different shape.
    let issued = iam_ok(
        &router,
        "the generated password of a service-specific credential",
        IAM_EVENT_REGION,
        "CreateServiceSpecificCredential",
        &[("UserName", USER_NAME), ("ServiceName", CREDENTIAL_SERVICE)],
    )
    .await;
    let issued_password = element(&issued, "ServicePassword");
    assert!(
        !issued_password.is_empty(),
        "case {:?}: the create answers the caller with a password, got {issued:?}",
        "the generated password of a service-specific credential"
    );
    rows.push(Recorded {
        case: "the generated password of a service-specific credential",
        event_name: "CreateServiceSpecificCredential",
        exactly: Vec::new(),
        absent: vec![
            "/responseElements/serviceSpecificCredential/servicePassword",
            "/responseElements/serviceSpecificCredential/serviceCredentialSecret",
        ],
        present: vec![
            "/responseElements/serviceSpecificCredential/serviceUserName",
            "/responseElements/serviceSpecificCredential/serviceSpecificCredentialId",
            "/responseElements/serviceSpecificCredential/userName",
            "/responseElements/serviceSpecificCredential/status",
        ],
        secret: issued_password.clone(),
    });

    // A virtual MFA device's shared seed and the QR image encoding it: TWO withheld members
    // of one response, where what remains is the device's own identifier.
    let device = iam_ok(
        &router,
        "the shared seed of a virtual MFA device",
        IAM_EVENT_REGION,
        "CreateVirtualMFADevice",
        &[("VirtualMFADeviceName", "credential-write-audit-device")],
    )
    .await;
    let seed = element(&device, "Base32StringSeed");
    assert!(
        !seed.is_empty(),
        "case {:?}: the create answers the caller with a seed, got {device:?}",
        "the shared seed of a virtual MFA device"
    );
    rows.push(Recorded {
        case: "the shared seed of a virtual MFA device",
        event_name: "CreateVirtualMFADevice",
        exactly: Vec::new(),
        absent: vec![
            "/responseElements/virtualMFADevice/base32StringSeed",
            "/responseElements/virtualMFADevice/qRCodePNG",
        ],
        present: vec!["/responseElements/virtualMFADevice/serialNumber"],
        secret: seed.clone(),
    });

    // Every row's record, read back the way a client auditing credential creation reads it:
    // a lookup in the Region a global service's event history lives in, by the operation's
    // own name.
    let withheld_keys: BTreeSet<String> = OMITTED_MEMBERS
        .iter()
        .map(|member| recorded_member_name(member))
        .collect();
    for row in &rows {
        let record = record_of(&router, row.case, IAM_EVENT_REGION, row.event_name).await;

        for (pointer, expected) in &row.exactly {
            assert_eq!(
                member_names(&record, pointer),
                expected
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<String>>(),
                "case {:?}: expected {} of the {} record to carry exactly {expected:?}, got {record}",
                row.case,
                pointer,
                row.event_name
            );
        }
        for pointer in &row.absent {
            assert_eq!(
                record.pointer(pointer),
                None,
                "case {:?}: the {} record must carry no {pointer} at all, got {record}",
                row.case,
                row.event_name
            );
        }
        for pointer in &row.present {
            assert!(
                record.pointer(pointer).is_some(),
                "case {:?}: the {} record keeps {pointer}, got {record}",
                row.case,
                row.event_name
            );
        }

        // The withholding is by OMISSION: no key of a withheld member stands anywhere in the
        // document, at any depth, so nothing can come back carrying a placeholder.
        let mut keys = BTreeSet::new();
        keys_anywhere(&record, &mut keys);
        let withheld_present: Vec<&String> = keys.intersection(&withheld_keys).collect();
        assert!(
            withheld_present.is_empty(),
            "case {:?}: the {} record names withheld members {withheld_present:?}, got {record}",
            row.case,
            row.event_name
        );

        // What the record says about the call: the serving service, the Region its history
        // lives in, and that the call changed the account.
        for (member, expected) in [
            ("eventSource", json!(IAM_EVENT_SOURCE)),
            ("awsRegion", json!(IAM_EVENT_REGION)),
            ("readOnly", json!(false)),
            ("eventName", json!(row.event_name)),
        ] {
            assert_eq!(
                record.get(member),
                Some(&expected),
                "case {:?}: expected {member} {expected} on the {} record, got {record}",
                row.case,
                row.event_name
            );
        }

        // And the material itself is nowhere in the record, nor is a placeholder standing in
        // for it.
        let serialised = record.to_string();
        for (what, forbidden) in [
            ("the credential material", row.secret.as_str()),
            ("a placeholder in place of it", REDACTED),
        ] {
            assert!(
                !serialised.contains(forbidden),
                "case {:?}: the {} record must not carry {what}, got {serialised}",
                row.case,
                row.event_name
            );
        }
    }

    // Nothing in the WHOLE of the account's event history carries any of the material
    // either: a lookup over every record this service wrote, not just the one per row.
    let whole_history: String = looked_up(&router, IAM_EVENT_REGION, "EventSource", IAM_EVENT_SOURCE)
        .await
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<String>>()
        .join("\n");
    for row in &rows {
        assert!(
            !whole_history.contains(row.secret.as_str()),
            "case {:?}: no record of {IAM_EVENT_SOURCE} may carry its credential material, got \
             {whole_history}",
            row.case
        );
    }
    assert!(
        !whole_history.contains(REDACTED),
        "no record of {IAM_EVENT_SOURCE} stands a placeholder in for a withheld member, got \
         {whole_history}"
    );
}

#[tokio::test]
async fn a_record_is_filed_in_the_region_its_serving_service_reports_from() {
    let router = router();

    // A global service's calls, each signed for a DIFFERENT Region: a write, a second write,
    // and a read.
    for (case, region, action, params) in [
        (
            "a global service's write in the leader Region",
            IAM_EVENT_REGION,
            "CreateUser",
            vec![("UserName", "region-filing-leader")],
        ),
        (
            "the same write signed for another Region",
            OTHER_REGION,
            "CreateUser",
            vec![("UserName", "region-filing-elsewhere")],
        ),
        (
            "a global service's read signed for a third Region",
            THIRD_REGION,
            "GetUser",
            vec![("UserName", "region-filing-leader")],
        ),
    ] {
        iam_ok(&router, case, region, action, &params).await;
    }

    // CloudTrail's own calls are REGIONAL: each one is filed in the Region it was served in.
    // The trail name list each carries names no trail, which selects nothing and refuses
    // nothing — what matters is the parameters the record keeps.
    for (case, region, token) in [
        (
            "a regional service's read in one Region",
            OTHER_REGION,
            "region-filing-control-one",
        ),
        (
            "the same regional read in another Region",
            THIRD_REGION,
            "region-filing-control-two",
        ),
    ] {
        let (status, answered) = cloudtrail(
            &router,
            region,
            "DescribeTrails",
            &json!({ "trailNameList": [token] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeTrails in {region} must be served, got {status} with \
             {answered}"
        );
    }

    // Each global call is in the leader Region's history, under the serving service's source,
    // whatever Region it was signed for.
    for (case, event_name, read_only) in [
        (
            "a global service's write in the leader Region",
            "CreateUser",
            false,
        ),
        (
            "a global service's read signed for a third Region",
            "GetUser",
            true,
        ),
    ] {
        let found = looked_up(&router, IAM_EVENT_REGION, "EventName", event_name).await;
        assert!(
            !found.is_empty(),
            "case {case:?}: expected a {event_name} record in {IAM_EVENT_REGION}, got none"
        );
        for record in &found {
            assert_eq!(
                record.get("awsRegion"),
                Some(&json!(IAM_EVENT_REGION)),
                "case {case:?}: every {event_name} record is filed in {IAM_EVENT_REGION}, got \
                 {record}"
            );
            assert_eq!(
                record.get("eventSource"),
                Some(&json!(IAM_EVENT_SOURCE)),
                "case {case:?}: expected {IAM_EVENT_SOURCE} on the {event_name} record, got \
                 {record}"
            );
            assert_eq!(
                record.get("readOnly"),
                Some(&json!(read_only)),
                "case {case:?}: expected readOnly {read_only} on the {event_name} record, got \
                 {record}"
            );
        }
    }
    assert_eq!(
        looked_up(&router, IAM_EVENT_REGION, "EventName", "CreateUser")
            .await
            .len(),
        2,
        "both writes are filed in {IAM_EVENT_REGION}, whichever Region they were signed for"
    );
    assert!(
        looked_up(&router, OTHER_REGION, "EventSource", IAM_EVENT_SOURCE)
            .await
            .is_empty(),
        "a lookup in {OTHER_REGION} reports no record of {IAM_EVENT_SOURCE}"
    );

    // The regional calls are each in their OWN Region's history, with the parameters the
    // request carried, and in no other Region's.
    for (case, region, token, elsewhere) in [
        (
            "a regional service's read in one Region",
            OTHER_REGION,
            "region-filing-control-one",
            THIRD_REGION,
        ),
        (
            "the same regional read in another Region",
            THIRD_REGION,
            "region-filing-control-two",
            OTHER_REGION,
        ),
    ] {
        let here = looked_up(&router, region, "EventName", "DescribeTrails").await;
        let matching: Vec<&Value> = here
            .iter()
            .filter(|record| {
                record.pointer("/requestParameters/trailNameList") == Some(&json!([token]))
            })
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "case {case:?}: expected exactly one DescribeTrails record in {region} carrying \
             {token:?}, got {here:?}"
        );
        assert_eq!(
            matching[0].get("awsRegion"),
            Some(&json!(region)),
            "case {case:?}: a regional call is filed in the Region it was served in, got {}",
            matching[0]
        );
        assert_eq!(
            matching[0].get("eventSource"),
            Some(&json!(CLOUDTRAIL_EVENT_SOURCE)),
            "case {case:?}: expected {CLOUDTRAIL_EVENT_SOURCE} on the record, got {}",
            matching[0]
        );
        assert_eq!(
            member_names(matching[0], "/requestParameters"),
            vec!["trailNameList".to_string()],
            "case {case:?}: the record carries exactly the parameters the request did, got {}",
            matching[0]
        );

        let there = looked_up(&router, elsewhere, "EventName", "DescribeTrails").await;
        assert!(
            there.iter().all(|record| {
                record.pointer("/requestParameters/trailNameList") != Some(&json!([token]))
            }),
            "case {case:?}: the record belongs to {region} alone, yet {elsewhere} reports it: \
             {there:?}"
        );
    }
}
