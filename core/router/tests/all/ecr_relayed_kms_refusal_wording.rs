// SPDX-License-Identifier: CC0-1.0

//! What Amazon ECR tells a caller when the KMS key its `CreateRepository` names cannot be
//! granted on: this service's OWN sentence for a failure it relays, with KMS's answer behind
//! it.
//!
//! RULE: a service that refuses a request because ANOTHER service refused the call it made on
//! the caller's behalf reports that refusal as its own fixed opening for a relayed failure,
//! then the called service's own sentence verbatim, then the clause naming the answer it
//! relays — which service answered, the status and the error code that answer carried, that
//! answer's own request id and the proxy it came through — and nothing of the SDK layer that
//! caught it, in particular no exception CLASS name, because a client strips the opening to
//! recover the called service's diagnostic. The opening is fixed text; everything after it is
//! the called service's answer, so the variable parts of the sentence (here the Region named
//! in the key ARN the request supplied) come from the request and from that answer rather than
//! from anything this service writes, and the request id is per answer.
//!
//! The grant is taken through the in-process callback, so a single-service router has no KMS
//! to answer it: that is why this suite lives here rather than in the ECR crate.
//!
//! The rows span what the rule generalises over: the Region of the key ARN the request names
//! (three Regions other than the repository's, so the Region in the sentence is read from the
//! request and never a fixed one), the KMS encryption type that asked for the key, and — in
//! the second test — WHICH of the two calls this service makes to KMS was refused (resolving
//! the key, taking the grant on it) and therefore the error class and sentence the relayed
//! answer carried, neither of which is the class this service answers its own caller under. A
//! served create with a key of the repository's own Region is the control every refusal row
//! differs from in the key's Region alone.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every repository of this suite is created in, and therefore the Region its key
/// is resolved in: a KMS key is regional, and ECR resolves the key at the repository's own
/// Region's endpoint.
const REGION: &str = "eu-west-1";

/// The Regions a key that is NOT the repository's Region is made in. Three of them, so the
/// Region the refusal names is read from the key ARN the request carried rather than being one
/// fixed value that happens to match.
const FOREIGN_REGIONS: [&str; 3] = ["us-east-1", "ap-south-1", "eu-central-1"];

/// The account id every emulated ARN carries (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// The `X-Amz-Target` prefix ECR routes on.
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// The `X-Amz-Target` prefix KMS routes on (its service shape is `TrentService`).
const KMS_TARGET_PREFIX: &str = "TrentService";

/// The fixed sentence Amazon ECR opens a refusal it relays from KMS with, including the colon
/// and the space that separate it from KMS's own sentence. Written out here rather than
/// imported, so a service that words it differently — or that opens with the class of the
/// exception the call's SDK layer caught — fails this suite.
const RELAYED_OPENING: &str = "The Amazon ECR action failed due to a KMS exception: ";

/// The name the relayed answer's own service is reported under in the clause.
const KMS_SERVICE_NAME: &str = "AWSKMS";

/// The status Amazon ECR answers its OWN caller with for this refusal, whatever status the
/// relayed answer carried: `KmsException` is a client error of this service
/// (`smithy_query(query="operation",operation="CreateRepository")` reports `KmsException`,
/// errorKind client, httpError 400).
const ECR_REFUSAL_STATUS: u16 = 400;

/// ECR and KMS over ONE shared engine, as `router/src/lib.rs` mounts them, so ECR's in-process
/// callback reaches this very KMS.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(engine),
    );
    router
}

/// One awsJson1_1 call, signed for `region`, answered with the status and the JSON body.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    host: &str,
    signing_name: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", target)
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One ECR call, at the endpoint of [`REGION`].
async fn ecr(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    call(
        router,
        REGION,
        &format!("api.ecr.{REGION}.amazonaws.com"),
        "ecr",
        &format!("{ECR_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await
}

/// One KMS call at `region`'s endpoint: a KMS key is a regional resource, so a key of ANOTHER
/// Region is made there.
async fn kms_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    call(
        router,
        region,
        &format!("kms.{region}.amazonaws.com"),
        "kms",
        &format!("{KMS_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await
}

/// A KMS call that must be served, or a panic naming the case and what came back instead.
async fn kms_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answer) = kms_in(router, region, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from kms:{operation} in {region}, got {status} with \
         {answer}"
    );
    answer
}

/// An ECR call that must be served, or a panic naming the case and what came back instead.
async fn ecr_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = ecr(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ecr:{operation}, got {status} with {answer}"
    );
    answer
}

/// A member of `value` as a string, or `"<unset>"` when it is absent — so an assertion reports
/// the missing member rather than panicking on it.
fn member(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// The simple name of the error an awsJson answer reports itself as, whose `__type` may be
/// qualified by the shape's namespace.
fn error_code(answer: &Value) -> String {
    member(answer, "__type")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Whether `id` has the 8-4-4-4-12 lowercase-hex form of the request ids these services report.
fn is_request_id(id: &str) -> bool {
    let mut groups = id.split('-');
    for width in [8usize, 4, 4, 4, 12] {
        let Some(group) = groups.next() else {
            return false;
        };
        let hex = group.len() == width
            && group
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
        if !hex {
            return false;
        }
    }
    groups.next().is_none()
}

/// The clause a relayed refusal names the answer it came from in, up to the request id that
/// answer carried.
fn clause_head(status: u16, code: &str) -> String {
    format!(
        "(Service: {KMS_SERVICE_NAME}; Status Code: {status}; Error Code: {code}; Request ID: "
    )
}

/// Assert `message` is `sentence` RELAYED: this service's fixed opening, that sentence
/// verbatim, and the clause naming the answer it came from, ending in the proxy that answer
/// came through. Answers with the request id the message stated, so a caller can check two
/// refusals report two answers.
fn relayed_request_id(case: &str, message: &str, sentence: &str, status: u16, code: &str) -> String {
    let head = format!("{RELAYED_OPENING}{sentence} {}", clause_head(status, code));
    let tail = "; Proxy: null)";
    assert!(
        message.starts_with(&head),
        "case {case:?}: expected the refusal to open {head:?}, got {message:?}"
    );
    assert!(
        message.ends_with(tail),
        "case {case:?}: expected the refusal to end {tail:?}, got {message:?}"
    );
    // Nothing of the SDK layer that caught the answer: a client strips the opening to recover
    // KMS's own diagnostic, so no exception class may stand between the two.
    assert!(
        !message.contains("com.amazonaws"),
        "case {case:?}: expected no Java exception class in the relayed refusal, got {message:?}"
    );
    assert_eq!(
        message.matches(RELAYED_OPENING).count(),
        1,
        "case {case:?}: expected this service's opening exactly once, got {message:?}"
    );
    let id = message
        .strip_prefix(&head)
        .and_then(|rest| rest.strip_suffix(tail))
        .unwrap_or_default();
    assert!(
        is_request_id(id),
        "case {case:?}: expected the relayed answer's own request id, got {id:?} in {message:?}"
    );
    id.to_string()
}

/// Whether this registry holds a repository named `name`.
async fn repository_exists(router: &MultiServiceRouter, case: &str, name: &str) -> bool {
    let (status, read) = ecr(
        router,
        "DescribeRepositories",
        &json!({"repositoryNames": [name]}),
    )
    .await;
    if status == 200 {
        return true;
    }
    assert_eq!(
        error_code(&read),
        "RepositoryNotFoundException",
        "case {case:?}: expected the read of {name} to be served or to report the repository \
         absent, got {status} with {read}"
    );
    false
}

/// RULE: a key ARN naming a Region other than the repository's cannot be granted on at the
/// repository's Region's endpoint, so the create is refused with the relayed answer — this
/// service's fixed opening, KMS's own sentence (whose Region is the one the request's ARN
/// named) and the clause naming that answer — and no repository is created.
#[tokio::test]
async fn a_cross_region_key_is_refused_in_this_services_own_relayed_sentence() {
    let router = emulator();

    // The control: the same create with a key of the repository's OWN Region is served, so
    // every row below differs from a served request in the Region of the key it names alone.
    let own = kms_ok(&router, "setup", REGION, "CreateKey", &json!({})).await;
    let own_key = member(&own["KeyMetadata"], "Arn");
    let served = ecr_ok(
        &router,
        "a key of the repository's own Region",
        "CreateRepository",
        &json!({
            "repositoryName": "relayed-control",
            "encryptionConfiguration": {"encryptionType": "KMS", "kmsKey": own_key},
        }),
    )
    .await;
    assert_eq!(
        member(&served["repository"]["encryptionConfiguration"], "kmsKey"),
        own_key,
        "case \"a key of the repository's own Region\": expected the repository to report the \
         key it is encrypted with, got {served}"
    );

    // (the Region the key is made in, the KMS encryption type the create names)
    let rows: Vec<(&str, &str)> = FOREIGN_REGIONS
        .iter()
        .enumerate()
        .map(|(index, region)| (*region, ["KMS", "KMS_DSSE"][index % 2]))
        .collect();

    let mut stated_ids: BTreeSet<String> = BTreeSet::new();
    for (foreign, encryption_type) in rows {
        let case = format!("a {encryption_type} key of {foreign}, a Region other than {REGION}");
        let made = kms_ok(&router, &case, foreign, "CreateKey", &json!({})).await;
        let foreign_key = member(&made["KeyMetadata"], "Arn");

        // KMS's OWN answer to the very call ECR makes to resolve the key, read here rather
        // than written, so no wording of this suite's can pass for the dependency's. An ARN
        // pins the request to one Region, so the endpoint of another cannot resolve it, and
        // the answer names the ARN's own Region.
        let (kms_status, said) =
            kms_in(&router, REGION, "DescribeKey", &json!({"KeyId": foreign_key})).await;
        let sentence = member(&said, "message");
        let code = error_code(&said);
        let named_region = format!("Invalid arn {foreign}");
        assert_eq!(
            (kms_status, sentence.as_str(), code.as_str()),
            (400, named_region.as_str(), "NotFoundException"),
            "case {case:?}: expected KMS to answer about a key ARN of another Region by naming \
             that Region, got {kms_status} with {said}"
        );

        let name = format!("relayed-{foreign}");
        let (status, refused) = ecr(
            &router,
            "CreateRepository",
            &json!({
                "repositoryName": name,
                "encryptionConfiguration": {
                    "encryptionType": encryption_type,
                    "kmsKey": foreign_key,
                },
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused), member(&refused, "kmsError")),
            (ECR_REFUSAL_STATUS, "KmsException".to_string(), code.clone()),
            "case {case:?}: expected HTTP {ECR_REFUSAL_STATUS} KmsException carrying the code \
             KMS answered under, got {status} with {refused}"
        );
        let message = member(&refused, "message");
        let id = relayed_request_id(&case, &message, &sentence, kms_status, &code);
        assert!(
            stated_ids.insert(id.clone()),
            "case {case:?}: expected each relayed answer its own request id, got {id:?} twice"
        );
        // The Region in the sentence is the key ARN's, not the endpoint's: the two are told
        // apart only by a message that names neither in the other's place.
        assert!(
            !message.contains(REGION),
            "case {case:?}: expected the refusal to name the Region of the key ARN rather than \
             the endpoint's, got {message:?}"
        );
        assert!(
            !repository_exists(&router, &case, &name).await,
            "case {case:?}: expected no repository {name} after the refused create"
        );
    }
}

/// RULE: everything after this service's fixed opening is the relayed ANSWER's — its sentence,
/// its error code and the status it carried — so a refusal of the grant itself is reported
/// under the class KMS answered under rather than the one the key resolution answers, while
/// the create stays this service's own client error. And the request id is the answer's, so two
/// refusals of the same request report two ids.
#[tokio::test]
async fn the_relayed_clause_states_the_answer_kms_gave_to_the_grant() {
    let router = emulator();

    // A key of this Region whose deletion the owner has scheduled: KMS resolves it, and
    // refuses to grant on a key in that state.
    let made = kms_ok(&router, "setup", REGION, "CreateKey", &json!({})).await;
    let key_arn = member(&made["KeyMetadata"], "Arn");
    kms_ok(
        &router,
        "setup",
        REGION,
        "ScheduleKeyDeletion",
        &json!({"KeyId": key_arn, "PendingWindowInDays": 7}),
    )
    .await;

    let case = "a key of this Region whose deletion is scheduled";
    // KMS's own answer to the call ECR makes to take the grant, read through KMS's wire.
    let (kms_status, said) = kms_in(
        &router,
        REGION,
        "CreateGrant",
        &json!({
            "KeyId": key_arn,
            "GranteePrincipal": format!("arn:aws:iam::{ACCOUNT}:root"),
            "Operations": ["Decrypt"],
        }),
    )
    .await;
    let sentence = member(&said, "message");
    let code = error_code(&said);
    assert_eq!(
        (
            (400..500).contains(&kms_status),
            code.as_str(),
            sentence.contains(&key_arn)
        ),
        (true, "KMSInvalidStateException", true),
        "case {case:?}: expected KMS to refuse a grant on a key pending deletion by naming that \
         key and its state, got {kms_status} with {said}"
    );

    let name = "relayed-pending-deletion";
    let (status, refused) = ecr(
        &router,
        "CreateRepository",
        &json!({
            "repositoryName": name,
            "encryptionConfiguration": {"encryptionType": "KMS", "kmsKey": key_arn},
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused), member(&refused, "kmsError")),
        (ECR_REFUSAL_STATUS, "KmsException".to_string(), code.clone()),
        "case {case:?}: expected HTTP {ECR_REFUSAL_STATUS} KmsException carrying the code KMS \
         answered under, got {status} with {refused}"
    );
    let message = member(&refused, "message");
    let first = relayed_request_id(case, &message, &sentence, kms_status, &code);

    // The same request refused again reports the SAME wording about another answer: the
    // request id belongs to the answer relayed, so two refusals never report one id.
    let (repeated_status, repeated) = ecr(
        &router,
        "CreateRepository",
        &json!({
            "repositoryName": name,
            "encryptionConfiguration": {"encryptionType": "KMS", "kmsKey": key_arn},
        }),
    )
    .await;
    assert_eq!(
        repeated_status, ECR_REFUSAL_STATUS,
        "case {case:?}: expected the repeated create to be refused the same way, got \
         {repeated_status} with {repeated}"
    );
    let second = relayed_request_id(
        case,
        &member(&repeated, "message"),
        &sentence,
        kms_status,
        &code,
    );
    assert_ne!(
        first, second,
        "case {case:?}: expected each relayed answer its own request id, got {first:?} twice"
    );

    assert!(
        !repository_exists(&router, case, name).await,
        "case {case:?}: expected no repository {name} after the refused create"
    );
}
