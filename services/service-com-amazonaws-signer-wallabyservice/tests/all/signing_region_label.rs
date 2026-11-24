// SPDX-License-Identifier: CC0-1.0

//! The region a request is READ with is a region LABEL, so every ARN this service
//! mints is one its own ARN reading resolves again.
//!
//! Every assertion goes through the router, so each case exercises routing, the
//! credential scope the region is read from, the handler, the persisted record and
//! serialization — the same path a client takes. The rule under test:
//!
//! * the region a request is served for is the SigV4 credential scope's region
//!   when that value is a well-formed AWS region label, and this service's default
//!   region otherwise — the same fallback a request carrying no scope at all gets.
//!   An ARN's fields are `:`-separated and its region field is published as
//!   `[a-z0-9-]{1,20}`, so a value outside that grammar names no region an AWS
//!   endpoint has and must never be spliced into an identifier: an ARN carrying it
//!   would shift the later fields and name a resource no operation of this service
//!   could resolve again.
//! * what follows is that an ARN this service MINTS is always an ARN it SERVES:
//!   the `arn` a create reports is the value the three tagging operations resolve
//!   to that same profile, and the `profileVersionArn` it reports is the value the
//!   revocation reader accepts, whatever the scope the create was signed with.
//!
//! The cases are a table whose rows differ along what the rule generalises over:
//! the FORM of the scope's region field — a plain label, a label at the published
//! length bound, one character past it, and a value carrying each kind of
//! character the grammar excludes (the ARN field separator among them).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
// The longest region label the published ARN grammar admits, stated once by the grammar's
// own reader: every AWS model's `*Arn` pattern bounds the region field at `[a-z0-9-]{1,20}`,
// which is what `service_common::arn::region_label` reads.
use service_common::arn::MAX_REGION_LENGTH;
use service_com_amazonaws_signer_wallabyservice::state::{
    profile_region_and_name_in_arn, DEFAULT_REGION,
};
use service_com_amazonaws_signer_wallabyservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every generated emulator presents, spelled out here rather than
/// imported so a wrong account in the handlers fails this suite.
const ACCOUNT_ID: &str = "000000000000";

/// A platform from this service's published catalog whose profiles need no
/// `signingMaterial`.
const PLATFORM_ID: &str = "AWSLambda-SHA384-ECDSA";

/// The status an admitted `TagResource` / `UntagResource` answers with: both
/// response shapes declare no members, so the reply carries no document.
const NO_CONTENT_STATUS: u16 = 204;

/// One credential scope, and the region this service must serve it for.
struct Case {
    case: &'static str,
    /// The region field of the request's SigV4 credential scope.
    scope_region: String,
    /// The region every ARN the request mints has to carry.
    expected_region: String,
}

/// A region label of exactly `length` characters, inside the published grammar.
fn label_of_length(length: usize) -> String {
    let prefix = "ap-southeast-";
    let mut label = String::from(prefix);
    while label.len() < length {
        label.push('7');
    }
    label.truncate(length);
    label
}

/// The table: a scope region per FORM the rule distinguishes.
///
/// The labelled rows are kept verbatim; every other row names a value the region
/// grammar excludes, which this service must serve under its default region
/// instead. The excluded characters are the kinds a value outside `[a-z0-9-]`
/// carries — the ARN field separator, an uppercase letter, a dot, an underscore, a
/// space, an authority separator, a query and a fragment delimiter.
fn cases() -> Vec<Case> {
    let kept = |case: &'static str, region: &str| Case {
        case,
        scope_region: region.to_string(),
        expected_region: region.to_string(),
    };
    let fell_back = |case: &'static str, region: &str| Case {
        case,
        scope_region: region.to_string(),
        expected_region: DEFAULT_REGION.to_string(),
    };
    vec![
        kept("a plain region label", "us-west-2"),
        kept("a label in another geography", "ap-southeast-1"),
        kept("a label of a government partition", "us-gov-west-1"),
        kept(
            "the longest label the grammar admits",
            &label_of_length(MAX_REGION_LENGTH),
        ),
        fell_back(
            "one character past the longest label",
            &label_of_length(MAX_REGION_LENGTH + 1),
        ),
        fell_back("a region carrying the ARN field separator", "us:east-1"),
        fell_back("a region carrying an uppercase letter", "US-WEST-2"),
        fell_back("a region carrying a dot", "us.west.2"),
        fell_back("a region carrying an underscore", "us_west_2"),
        fell_back("a region carrying a space", "us west 2"),
        fell_back("a region carrying an authority separator", "us@west-2"),
        fell_back("a region carrying a query delimiter", "us-west-2?x"),
        fell_back("a region carrying a fragment delimiter", "us-west-2#x"),
    ]
}

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: status and JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    fn str_member(&self, member: &str) -> String {
        self.body[member].as_str().unwrap_or_default().to_string()
    }

    /// The modelled `message` member, or the empty string when there is none.
    fn message(&self) -> &str {
        self.body["message"].as_str().unwrap_or_default()
    }
}

/// A SigV4 `Authorization` header whose credential scope names `region` and the
/// `signer` signing name: what routes the request to this service and what the
/// region of the endpoint is read from.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/signer/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// `value` percent-encoded for ONE URI segment or query value — what an SDK does
/// to an ARN travelling as a non-greedy `@httpLabel`.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

async fn call(
    router: &MultiServiceRouter,
    scope_region: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "signer.us-east-1.amazonaws.com")
        .header("authorization", authorization(scope_region));
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body().to_vec();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} signed for {scope_region:?} answered a body that is not JSON \
                 ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

async fn put_profile(
    router: &MultiServiceRouter,
    scope_region: &str,
    name: &str,
    body: Value,
) -> Answer {
    call(
        router,
        scope_region,
        "PUT",
        &format!("/signing-profiles/{name}"),
        Some(body),
    )
    .await
}

async fn get_profile(router: &MultiServiceRouter, scope_region: &str, name: &str) -> Answer {
    call(
        router,
        scope_region,
        "GET",
        &format!("/signing-profiles/{name}"),
        None,
    )
    .await
}

/// The profile name row `index` of the table creates — one per row, so a row that
/// falls back to the default region never collides with another's profile.
fn profile_name(index: usize) -> String {
    format!("Region_Form_{index}")
}

/// Create the row's profile and answer what the service reported.
async fn create(router: &MultiServiceRouter, case: &Case, name: &str) -> Answer {
    let created = put_profile(
        router,
        &case.scope_region,
        name,
        json!({ "platformId": PLATFORM_ID }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {:?}: expected PutSigningProfile 200, got {} {:?}",
        case.case, created.status, created.body
    );
    created
}

#[tokio::test]
async fn a_minted_profile_arn_carries_the_scope_region_only_when_it_is_a_label() {
    let router = emulator();

    for (index, case) in cases().into_iter().enumerate() {
        let name = profile_name(index);
        let created = create(&router, &case, &name).await;
        let arn = created.str_member("arn");

        let expected_arn = format!(
            "arn:aws:signer:{}:{ACCOUNT_ID}:/signing-profiles/{name}",
            case.expected_region
        );
        assert_eq!(
            arn, expected_arn,
            "case {:?}: expected the minted ARN {expected_arn:?}, got {arn:?}",
            case.case
        );

        // The mint and the one parse that resolves an ARN back to a profile agree:
        // the ARN names the region the profile is keyed under, and the name it was
        // created with.
        assert_eq!(
            profile_region_and_name_in_arn(&arn),
            Some((case.expected_region.as_str(), name.as_str())),
            "case {:?}: the minted ARN {arn:?} must read back as the profile it names",
            case.case
        );

        // The version ARN is the profile's ARN with the version after it, so it
        // carries the same region.
        let version = created.str_member("profileVersion");
        let version_arn = created.str_member("profileVersionArn");
        assert_eq!(
            version_arn,
            format!("{arn}/{version}"),
            "case {:?}: expected the version ARN to extend the profile ARN, got {version_arn:?}",
            case.case
        );

        // A read reports the same ARN, and the profile is reachable through the
        // endpoint of the region the ARN names.
        let read = get_profile(&router, &case.expected_region, &name).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: expected GetSigningProfile 200 in {}, got {} {:?}",
            case.case, case.expected_region, read.status, read.body
        );
        assert_eq!(
            read.str_member("arn"),
            arn,
            "case {:?}: expected GetSigningProfile to report the stored ARN {arn:?}",
            case.case
        );
    }
}

#[tokio::test]
async fn a_minted_profile_arn_round_trips_through_the_tagging_operations() {
    let router = emulator();

    for (index, case) in cases().into_iter().enumerate() {
        let name = profile_name(index);
        let arn = create(&router, &case, &name).await.str_member("arn");
        let encoded = encode(&arn);

        let tagged = call(
            &router,
            &case.scope_region,
            "POST",
            &format!("/tags/{encoded}"),
            Some(json!({ "tags": { "team": "signing" } })),
        )
        .await;
        assert_eq!(
            tagged.status, NO_CONTENT_STATUS,
            "case {:?}: expected TagResource {NO_CONTENT_STATUS} for the service's own ARN \
             {arn:?}, got {} {:?} ({})",
            case.case,
            tagged.status,
            tagged.body,
            tagged.message()
        );

        let listed = call(
            &router,
            &case.scope_region,
            "GET",
            &format!("/tags/{encoded}"),
            None,
        )
        .await;
        assert_eq!(
            listed.status, 200,
            "case {:?}: expected ListTagsForResource 200 for {arn:?}, got {} {:?}",
            case.case, listed.status, listed.body
        );
        assert_eq!(
            listed.body["tags"]["team"].as_str(),
            Some("signing"),
            "case {:?}: expected the written tag to be reported, got {:?}",
            case.case,
            listed.body
        );

        let untagged = call(
            &router,
            &case.scope_region,
            "DELETE",
            &format!("/tags/{encoded}?tagKeys=team"),
            None,
        )
        .await;
        assert_eq!(
            untagged.status, NO_CONTENT_STATUS,
            "case {:?}: expected UntagResource {NO_CONTENT_STATUS} for {arn:?}, got {} {:?}",
            case.case, untagged.status, untagged.body
        );

        let after = call(
            &router,
            &case.scope_region,
            "GET",
            &format!("/tags/{encoded}"),
            None,
        )
        .await;
        assert_eq!(
            after.body["tags"]["team"].as_str(),
            None,
            "case {:?}: expected the removed key to be gone, got {:?}",
            case.case,
            after.body
        );
    }
}

#[tokio::test]
async fn a_minted_profile_version_arn_is_served_by_the_revocation_reader() {
    let router = emulator();

    for (index, case) in cases().into_iter().enumerate() {
        let name = profile_name(index);
        let created = create(&router, &case, &name).await;
        let version_arn = created.str_member("profileVersionArn");

        // `jobArn` is required and is judged on its SHAPE alone; this operation
        // models no not-found, so a well-formed job ARN naming no job is answered
        // as a resource that is simply not revoked. It is built for the region the
        // service minted the profile ARN for, which is the region whose endpoint
        // answers.
        let job_arn = format!(
            "arn:aws:signer:{}:{ACCOUNT_ID}:/signing-jobs/{}",
            case.expected_region,
            format_args!("job-{index}")
        );
        // A `signatureTimestamp` in the past (a signature is made before it is
        // checked) and a certificate hash of the documented shape: two hex
        // digests end to end.
        let uri = format!(
            "/revocations?signatureTimestamp=2023-11-14T22%3A13%3A20Z&platformId={}\
             &profileVersionArn={}&jobArn={}&certificateHashes={}",
            encode(PLATFORM_ID),
            encode(&version_arn),
            encode(&job_arn),
            "ab".repeat(96),
        );
        let answer = call(&router, &case.scope_region, "GET", &uri, None).await;
        assert_eq!(
            answer.status, 200,
            "case {:?}: expected GetRevocationStatus 200 for the service's own version ARN \
             {version_arn:?}, got {} {:?} ({})",
            case.case,
            answer.status,
            answer.body,
            answer.message()
        );
        assert!(
            answer.body["revokedEntities"].is_array(),
            "case {:?}: expected a revokedEntities list, got {:?}",
            case.case,
            answer.body
        );
    }
}
