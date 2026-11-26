// SPDX-License-Identifier: CC0-1.0

//! The KMS calls Amazon ECR makes for a caller, in the account's event history: a repository
//! encrypted with a customer managed key is audited as ECR acting on the caller's behalf.
//!
//! RULE: a call one service makes to ANOTHER service's API while serving a client's call is
//! recorded in the account's event history as a call of the service that was CALLED — its
//! `eventSource` and its operation's `eventName` — carrying the CLIENT's own `userIdentity`
//! with `userIdentity.invokedBy` naming the service that made it, which is how an auditor
//! tells such a call from one the client made itself; a call the client DID make itself is
//! recorded under the serving service's own `eventSource` with no `invokedBy` at all. So a
//! repository whose `encryptionConfiguration` names a key of the account's leaves two legs in
//! event history — the caller's own `ecr.amazonaws.com` `CreateRepository`, and the
//! `kms.amazonaws.com` calls ECR made on that key for it — and a repository that needs no
//! authorization on a key of the account's leaves only the first.
//!
//! Only a cross-service suite can pin this: the grant leg runs through ECR's in-process
//! callback into KMS, and the records are read back through `cloudtrail:LookupEvents` — three
//! services over ONE engine, as `core/router/src/lib.rs` mounts them.
//!
//! The rows span what the rule generalises over rather than the one call that motivated it:
//! the ECR side covers a MUTATING call, a REPORTING call and a REFUSED one; the KMS side
//! covers the read that resolves the key, the write that takes a grant and the write that
//! retires one; and the encryption configuration spans both KMS encryption types against the
//! S3-managed control, which authorizes nothing on a key of the account's and so is audited
//! as nothing but its own `CreateRepository`.
//!
//! No row writes a grant id, a key id, an event id or a request id: every one of those is
//! generated per run, so each is read from what the service answered.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for: a registry is one account's in one Region, and
/// ECR requires the key to be in the repository's own Region, so it is where the key lives
/// and where every record is filed.
const REGION: &str = "eu-central-1";

/// The account id every emulated ARN carries (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// The access key id every CLIENT call below is signed with — the credential whose identity
/// the records must name, ECR's own records and the records of the KMS calls ECR made for it
/// alike.
const CALLER_ACCESS_KEY_ID: &str = "AKIDECRKMSAUDIT";

/// The `X-Amz-Target` prefix ECR routes on.
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// The `X-Amz-Target` prefix KMS routes on (its service shape is `TrentService`).
const KMS_TARGET_PREFIX: &str = "TrentService";

/// The `X-Amz-Target` prefix CloudTrail routes on.
const CLOUDTRAIL_TARGET_PREFIX: &str = "CloudTrail_20131101";

/// The `eventSource` a record of an Amazon ECR call carries, and — as `invokedBy` — the
/// service that made a KMS call for one of its callers. Spelled out here rather than
/// imported, so a service that words either differently fails this suite.
const ECR_EVENT_SOURCE: &str = "ecr.amazonaws.com";

/// The `eventSource` a record of a KMS call carries, whoever made the call.
const KMS_EVENT_SOURCE: &str = "kms.amazonaws.com";

/// The KMS operations the grant ECR takes must permit, as a record of the `CreateGrant` that
/// took it reports them: the data-key operations encrypting a repository's contents under the
/// key needs (`smithy_query(query="shape",shape="GrantOperation")` in the KMS model).
const GRANT_OPERATIONS: [&str; 2] = ["Decrypt", "GenerateDataKey"];

/// The encryption-context key the encrypted repository's ARN is named under, which is what
/// confines the grant — and the record of the call that took it — to one repository.
const REPOSITORY_ENCRYPTION_CONTEXT_KEY: &str = "aws:ecr:arn";

/// How wide a window the lookups below ask for, in seconds either side of the call. Generous
/// on purpose: the window exercises `StartTime` / `EndTime` and times nothing, so nothing
/// here depends on how fast the host runs.
const LOOKUP_WINDOW_SECONDS: i64 = 3_600;

/// How long a poll keeps looking before it gives up. Generous on purpose: event history is
/// eventually consistent, and this only bounds a wait that would otherwise never end, so
/// neither a slow nor a loaded host decides the outcome.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// ECR, KMS and CloudTrail over ONE shared engine, as `router/src/lib.rs` mounts them, so
/// ECR's in-process callback reaches this very KMS and CloudTrail reads the one log both
/// services record into.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential scope
    // is still carried, because it is what routes the request, names the Region it was served
    // in and names the identity its record carries.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(engine),
    );
    router
}

/// What one call answered: its status, its body, and the request its response named.
struct Answered {
    status: u16,
    body: Value,
    /// The `x-amzn-RequestId` the response carried — the id the record of this call names it
    /// by, so a client holding a response finds the record of that very call.
    request_id: String,
}

/// One awsJson1_1 call to `signing_name`, signed for [`REGION`] with [`CALLER_ACCESS_KEY_ID`].
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> Answered {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential={CALLER_ACCESS_KEY_ID}/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let request_id = response
        .headers()
        .get("x-amzn-requestid")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let status = response.status().as_u16();
    let body = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answered {
        status,
        body,
        request_id,
    }
}

/// One Amazon ECR call.
async fn ecr(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answered {
    call(router, "ecr", ECR_TARGET_PREFIX, operation, body).await
}

/// One Amazon ECR call that must be served.
async fn ecr_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Answered {
    let answered = ecr(router, operation, body).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: Amazon ECR {operation} must be served, got {} with {}",
        answered.status, answered.body
    );
    assert!(
        !answered.request_id.is_empty(),
        "case {case:?}: Amazon ECR {operation} must name the request it answered, got no \
         x-amzn-RequestId"
    );
    answered
}

/// One KMS call.
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answered {
    call(router, "kms", KMS_TARGET_PREFIX, operation, body).await
}

/// One KMS call that must be served.
async fn kms_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Answered {
    let answered = kms(router, operation, body).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: KMS {operation} must be served, got {} with {}",
        answered.status, answered.body
    );
    answered
}

/// A member of `value` as a string, or `"<unset>"` when it is absent — so an assertion
/// reports the missing member rather than panicking on it.
fn member(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// `cloudtrail:LookupEvents` filtered on one attribute over a window holding every call this
/// suite makes, as each matching event's `CloudTrailEvent` document.
async fn looked_up(
    router: &MultiServiceRouter,
    attribute_key: &str,
    attribute_value: &str,
) -> Vec<Value> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock at or after the epoch")
        .as_secs() as i64;
    let answered = call(
        router,
        "cloudtrail",
        CLOUDTRAIL_TARGET_PREFIX,
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
        answered.status, 200,
        "LookupEvents({attribute_key}={attribute_value}) must be served, got {} with {}",
        answered.status, answered.body
    );
    answered.body["Events"]
        .as_array()
        .unwrap_or_else(|| panic!("LookupEvents reports an Events list, got {}", answered.body))
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

/// Look up the records of `event_name` again and again until one of them satisfies `wanted`,
/// and answer that record.
///
/// Event history is eventually consistent, so the CONDITION is waited for rather than a span
/// of wall clock: the deadline is generous and only bounds a wait that would never end.
async fn record_until(
    router: &MultiServiceRouter,
    case: &str,
    event_name: &str,
    expectation: &str,
    wanted: impl Fn(&Value) -> bool,
) -> Value {
    let since = Instant::now();
    loop {
        let found = looked_up(router, "EventName", event_name).await;
        if let Some(record) = found.iter().find(|record| wanted(record)) {
            return record.clone();
        }
        assert!(
            since.elapsed() < POLL_DEADLINE,
            "case {case:?}: no {event_name} record {expectation}; the lookup last reported \
             {found:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The records of `event_name` that name no call of this suite's caller at all — used to
/// assert that a record this rule forbids was never written, after everything that would
/// write one has been served.
async fn records_of(router: &MultiServiceRouter, event_name: &str) -> Vec<Value> {
    looked_up(router, "EventName", event_name).await
}

/// The ONE record naming the request answered under `request_id` — how a client holding its
/// own response finds the record of that very call.
async fn record_of_request(
    router: &MultiServiceRouter,
    case: &str,
    event_name: &str,
    request_id: &str,
) -> Value {
    record_until(
        router,
        case,
        event_name,
        &format!("naming the request {request_id:?}"),
        |record| record.get("requestID") == Some(&json!(request_id)),
    )
    .await
}

/// Whether `record` is a call `invoked_by` made for this suite's caller: the called service's
/// own record, carrying the CALLER's credential and `invokedBy` naming the calling service.
fn made_for_the_caller_by(record: &Value, invoked_by: &str) -> bool {
    record.get("eventSource") == Some(&json!(KMS_EVENT_SOURCE))
        && record["userIdentity"].get("invokedBy") == Some(&json!(invoked_by))
        && record["userIdentity"].get("accessKeyId") == Some(&json!(CALLER_ACCESS_KEY_ID))
}

/// The ARN of the repository `name` in this account's registry.
fn repository_arn(name: &str) -> String {
    format!("arn:aws:ecr:{REGION}:{ACCOUNT}:repository/{name}")
}

/// Every grant `kms:ListGrants` reports on the key `key` names for `repository_arn`, once
/// there are `wanted` of them.
async fn grants_for(
    router: &MultiServiceRouter,
    case: &str,
    key: &str,
    repository_arn: &str,
    wanted: usize,
) -> Vec<Value> {
    let since = Instant::now();
    loop {
        let listed = kms_ok(router, case, "ListGrants", &json!({"KeyId": key})).await;
        let found: Vec<Value> = listed.body["Grants"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|grant| member(&grant["Constraints"], "SourceArn") == repository_arn)
            .collect();
        if found.len() == wanted {
            return found;
        }
        assert!(
            since.elapsed() < POLL_DEADLINE,
            "case {case:?}: kms:ListGrants never reported {wanted} grant(s) for \
             {repository_arn}; it last reported {found:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the ECR side: a call a CLIENT makes to Amazon ECR, and what its own record in
/// event history says.
struct EcrCall {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// The operation the client called.
    operation: &'static str,
    /// Whether the call is one that only REPORTS the account, which is the record's
    /// `readOnly`.
    read_only: bool,
    /// The fault shape the caller was answered with, for a row that is REFUSED.
    error_code: Option<&'static str>,
}

/// RULE (the ECR leg): every call a client makes to Amazon ECR is one management event of
/// `ecr.amazonaws.com` named by the id the caller was answered under, carrying that client's
/// own identity and NO `invokedBy` — because the client made the call itself — with the
/// read/write class of the operation, the parameters it carried, what a mutating call
/// answered, and the fault a refused one was answered with.
#[tokio::test]
async fn an_ecr_call_is_recorded_as_the_callers_own_with_no_invoked_by() {
    let router = emulator();
    let name = "audited-plain-repository";

    let created = ecr_ok(
        &router,
        "setup",
        "CreateRepository",
        &json!({"repositoryName": name}),
    )
    .await;
    let described = ecr_ok(
        &router,
        "setup",
        "DescribeRepositories",
        &json!({"repositoryNames": [name]}),
    )
    .await;
    // A repository no registry holds: the refused row, answered with the operation's own
    // modelled fault.
    let absent = "audited-repository-that-is-not-there";
    let refused = ecr(
        &router,
        "DeleteRepository",
        &json!({"repositoryName": absent, "force": true}),
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "setup: deleting a repository no registry holds is refused, got {} with {}",
        refused.status, refused.body
    );

    let rows = [
        (
            EcrCall {
                case: "a mutating call",
                operation: "CreateRepository",
                read_only: false,
                error_code: None,
            },
            created,
        ),
        (
            EcrCall {
                case: "a reporting call",
                operation: "DescribeRepositories",
                read_only: true,
                error_code: None,
            },
            described,
        ),
        (
            EcrCall {
                case: "a refused call",
                operation: "DeleteRepository",
                read_only: false,
                error_code: Some("RepositoryNotFoundException"),
            },
            refused,
        ),
    ];

    for (row, answered) in rows {
        let case = row.case;
        let record =
            record_of_request(&router, case, row.operation, &answered.request_id).await;
        for (what, actual, expected) in [
            ("eventSource", record["eventSource"].clone(), json!(ECR_EVENT_SOURCE)),
            ("eventName", record["eventName"].clone(), json!(row.operation)),
            ("awsRegion", record["awsRegion"].clone(), json!(REGION)),
            ("readOnly", record["readOnly"].clone(), json!(row.read_only)),
            (
                "userIdentity.accessKeyId",
                record["userIdentity"]["accessKeyId"].clone(),
                json!(CALLER_ACCESS_KEY_ID),
            ),
        ] {
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {what} {expected}, got {actual} in {record}"
            );
        }
        assert!(
            record["userIdentity"].get("invokedBy").is_none(),
            "case {case:?}: a call the client made itself names no service that made it for \
             it, got {record}"
        );
        let error_code = record.get("errorCode").cloned();
        assert_eq!(
            error_code,
            row.error_code.map(|code| json!(code)),
            "case {case:?}: expected errorCode {:?}, got {error_code:?} in {record}",
            row.error_code
        );
        if let Some(code) = row.error_code {
            // The record carries the sentence the caller saw, character for character.
            assert_eq!(
                record.get("errorMessage"),
                refused_message(&answered.body).as_ref(),
                "case {case:?}: the record of a call refused as {code} carries the message \
                 the caller saw, got {record}"
            );
        }
        // A reporting call answers nothing a record carries; a mutating one is recorded with
        // what it answered.
        assert_eq!(
            record["responseElements"].is_null(),
            row.read_only || row.error_code.is_some(),
            "case {case:?}: expected responseElements to be recorded only for a served \
             mutating call, got {record}"
        );
    }
}

/// The message a refused answer carried, as the record of that call must carry it.
fn refused_message(body: &Value) -> Option<Value> {
    body.get("message").or_else(|| body.get("Message")).cloned()
}

/// One row of the KMS side: a repository's encryption configuration, and whether a key of the
/// account's is authorized for it.
struct EncryptedRepository {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// The repository created by the row.
    name: &'static str,
    /// Its `encryptionConfiguration.encryptionType`.
    encryption_type: &'static str,
    /// Whether the configuration names the account's key, and so authorizes this service on
    /// it — the one fact that decides whether the KMS leg is audited at all.
    grants_on_the_key: bool,
}

/// RULE (the KMS leg): the KMS calls Amazon ECR makes on a caller's key while serving that
/// caller's `CreateRepository` and `DeleteRepository` are recorded as calls of
/// `kms.amazonaws.com` — the read that resolves the key, the writes that take the grants and
/// the writes that retire them — each carrying the CALLER's own identity and `invokedBy`
/// naming Amazon ECR, and each stating the terms the call was made under. A repository that
/// authorizes nothing on a key of the account's has no such leg at all.
#[tokio::test]
async fn the_kms_calls_an_encrypted_repository_needs_are_recorded_as_ecrs_calls_for_the_caller()
{
    let router = emulator();

    // The key the rows name, made through KMS's own wire so every row names a key that
    // service really holds rather than an ARN this suite wrote.
    let created = kms_ok(&router, "setup", "CreateKey", &json!({})).await;
    let key_arn = member(&created.body["KeyMetadata"], "Arn");
    let key_id = member(&created.body["KeyMetadata"], "KeyId");

    // The CONTROL for the whole rule: the caller's OWN call on the same key, which is
    // recorded under the same event source and names no service that made it.
    let read_by_the_client = kms_ok(
        &router,
        "the caller's own call on the key",
        "DescribeKey",
        &json!({"KeyId": key_id}),
    )
    .await;

    let rows = [
        EncryptedRepository {
            case: "KMS encryption naming the account's key",
            name: "audited-kms-repository",
            encryption_type: "KMS",
            grants_on_the_key: true,
        },
        EncryptedRepository {
            case: "dual-layer KMS encryption naming the account's key",
            name: "audited-kms-dsse-repository",
            encryption_type: "KMS_DSSE",
            grants_on_the_key: true,
        },
        EncryptedRepository {
            case: "S3-managed encryption, which takes no key of the account's",
            name: "audited-aes256-repository",
            encryption_type: "AES256",
            grants_on_the_key: false,
        },
    ];

    for row in &rows {
        let case = row.case;
        let arn = repository_arn(row.name);
        let mut configuration = json!({"encryptionType": row.encryption_type});
        if row.grants_on_the_key {
            configuration["kmsKey"] = json!(key_arn);
        }
        let created = ecr_ok(
            &router,
            case,
            "CreateRepository",
            &json!({"repositoryName": row.name, "encryptionConfiguration": configuration}),
        )
        .await;

        // The caller's own leg: the record of the call the client made, which names the key
        // the request asked the repository to be encrypted with and no service that made it.
        let own = record_of_request(&router, case, "CreateRepository", &created.request_id).await;
        assert_eq!(
            own["requestParameters"]["encryptionConfiguration"],
            configuration,
            "case {case:?}: the caller's own record states the encryption configuration the \
             request carried, got {own}"
        );
        assert!(
            own["userIdentity"].get("invokedBy").is_none(),
            "case {case:?}: the client made its own CreateRepository, so its record names no \
             service that made it, got {own}"
        );

        if !row.grants_on_the_key {
            // Nothing was authorized on anybody's key, so no call was made on one for this
            // repository — there is no record of one to find.
            let took = records_of(&router, "CreateGrant").await;
            let taken_for_this_one: Vec<&Value> = took
                .iter()
                .filter(|record| {
                    record["requestParameters"]["constraints"]["sourceArn"] == json!(arn)
                })
                .collect();
            assert!(
                taken_for_this_one.is_empty(),
                "case {case:?}: a repository that authorizes no key of the account's leaves no \
                 record of a grant taken for it, got {taken_for_this_one:?}"
            );
            continue;
        }

        // The grants KMS holds for this repository, read from KMS itself: the record of each
        // call that took one must name the very grant the key reports.
        let held = grants_for(&router, case, &key_arn, &arn, 2).await;
        let grant_ids: Vec<String> = held
            .iter()
            .map(|grant| member(grant, "GrantId"))
            .collect();

        // The read that RESOLVED the key, as a call ECR made for the caller.
        let resolved = record_until(
            &router,
            case,
            "DescribeKey",
            "that Amazon ECR made for the caller",
            |record| {
                made_for_the_caller_by(record, ECR_EVENT_SOURCE)
                    && record["requestParameters"]["keyId"] == json!(key_arn)
            },
        )
        .await;
        assert_eq!(
            resolved["readOnly"],
            json!(true),
            "case {case:?}: resolving the key reports it and changes nothing, got {resolved}"
        );
        assert!(
            resolved["responseElements"].is_null(),
            "case {case:?}: a read answers nothing a record carries, got {resolved}"
        );

        // The WRITE that took each grant, named by the grant KMS minted for it.
        for grant_id in &grant_ids {
            let took = record_until(
                &router,
                case,
                "CreateGrant",
                &format!("naming the grant {grant_id:?} it took for the caller"),
                |record| {
                    made_for_the_caller_by(record, ECR_EVENT_SOURCE)
                        && record["responseElements"]["grantId"] == json!(grant_id)
                },
            )
            .await;
            let parameters = took["requestParameters"].clone();
            for (what, actual, expected) in [
                ("keyId", parameters["keyId"].clone(), json!(key_arn)),
                (
                    "granteePrincipal",
                    parameters["granteePrincipal"].clone(),
                    json!(format!("ecr.{REGION}.amazonaws.com")),
                ),
                (
                    "constraints.sourceArn",
                    parameters["constraints"]["sourceArn"].clone(),
                    json!(arn),
                ),
                (
                    "constraints.encryptionContextSubset",
                    parameters["constraints"]["encryptionContextSubset"].clone(),
                    json!({REPOSITORY_ENCRYPTION_CONTEXT_KEY: arn}),
                ),
                (
                    "operations",
                    parameters["operations"].clone(),
                    json!(GRANT_OPERATIONS),
                ),
            ] {
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected the record of the grant to state {what} \
                     {expected}, got {actual} in {took}"
                );
            }
            assert_eq!(
                took["readOnly"],
                json!(false),
                "case {case:?}: taking a grant changes which authorizations the key carries, \
                 got {took}"
            );
            assert_eq!(
                took["awsRegion"],
                json!(REGION),
                "case {case:?}: the record is filed in the Region the call was made in, got \
                 {took}"
            );
        }

        // Deleting the repository retires those authorizations, and each retirement is a call
        // ECR made for the caller too.
        ecr_ok(
            &router,
            case,
            "DeleteRepository",
            &json!({"repositoryName": row.name, "force": true}),
        )
        .await;
        for grant_id in &grant_ids {
            let retired = record_until(
                &router,
                case,
                "RetireGrant",
                &format!("naming the grant {grant_id:?} it retired for the caller"),
                |record| {
                    made_for_the_caller_by(record, ECR_EVENT_SOURCE)
                        && record["requestParameters"]["grantId"] == json!(grant_id)
                },
            )
            .await;
            assert_eq!(
                retired["requestParameters"]["keyId"],
                json!(key_arn),
                "case {case:?}: the retirement names the key the grant was on, got {retired}"
            );
            assert_eq!(
                retired["readOnly"],
                json!(false),
                "case {case:?}: retiring a grant changes which authorizations the key carries, \
                 got {retired}"
            );
        }
        let left = grants_for(&router, case, &key_arn, &arn, 0).await;
        assert!(
            left.is_empty(),
            "case {case:?}: the grants the repository held are retired with it, got {left:?}"
        );
    }

    // The control, judged after every leg above has been served: the call the CLIENT made on
    // the key is recorded under the same event source with no `invokedBy` at all, which is
    // what tells the two kinds of record apart.
    let own_read = record_of_request(
        &router,
        "the caller's own call on the key",
        "DescribeKey",
        &read_by_the_client.request_id,
    )
    .await;
    assert_eq!(
        own_read["eventSource"],
        json!(KMS_EVENT_SOURCE),
        "the caller's own KMS call belongs to the service it called, got {own_read}"
    );
    assert!(
        own_read["userIdentity"].get("invokedBy").is_none(),
        "the caller made this KMS call itself, so its record names no service that made it \
         for it, got {own_read}"
    );
}
