// SPDX-License-Identifier: CC0-1.0

//! Which REGION's Secrets Manager answers a Linux subscription provider registration, with
//! Secrets Manager mounted beside License Manager Linux Subscriptions on the one shared
//! engine — the way the assembled emulator runs them, and the only arrangement in which the
//! in-process hop `RegisterSubscriptionProvider` makes to read the provider's access token
//! reaches a service that holds secrets at all.
//!
//! RULE: a service that resolves a caller-supplied ARN against the named vendor's endpoint
//! reads the Region from the FOURTH field of that ARN, under the Region grammar every AWS
//! model states for it (non-empty, at most 20 characters, `[a-z0-9-]` only) — because that
//! label goes into an endpoint URL's AUTHORITY and not merely into data — and falls back to
//! the Region the request was signed for for every value that names no Region of the vendor
//! at all. So the secret a registration names is read where its own `SecretArn` says it is
//! stored, whatever Region the registration was addressed to; a `SecretArn` naming another
//! Region names no secret this account can register with; and a field the grammar does not
//! admit — the empty one of the partition-global form, a dotted host label, a label past
//! the published length — is no Region, so the read stays in the Region the caller signed
//! for.
//!
//! The SAME parse is what the service's tagging surface settles authorization with — the
//! account field of the `resourceArn` it is addressed with — so the last test here drives it
//! through `ListTagsForResource`/`TagResource`: one shared ARN reader, two operations, read
//! through the assembled router rather than through either crate alone.
//!
//! The rows differ along what the rule generalises over: the Region the request is signed
//! for, the form of the `SecretArn`'s own Region field (the secret's own Region, another
//! Region, a third one, the empty partition-global field, a dotted host label, a label at
//! the published bound and one past it), and the account the tag request's `resourceArn`
//! names.
//!
//! Which Region the credential read was addressed in is made OBSERVABLE through Secrets
//! Manager's own resolution: it resolves a `SecretId` in the Region the request addresses
//! and in no other ("an ARN naming another Region's secret names nothing here"), so a
//! secret planted under one identifier in one Region is the only evidence a client can have
//! of where the hop went — and a value the Region grammar does not admit must leave the
//! secret planted behind it unreachable.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The signing name of the service under test: the first label of its regional endpoint,
/// which is the route key it claims (`OPERATIONS.md`, "Wire protocol").
const SUBSCRIPTIONS: &str = "license-manager-linux-subscriptions";

/// Secrets Manager's signing name — the service the registration's credential read is
/// addressed at, and the one whose Region this suite is about.
const SECRETS: &str = "secretsmanager";

/// The Region the secret of every row is stored in, and the Region a row that is about the
/// fall-back signs its registration for.
const SECRET_REGION: &str = "us-west-2";

/// Two further AWS Regions, so a row can address the registration somewhere other than
/// where its secret lives, and point a `SecretArn` at a Region holding no secret.
const ANOTHER_REGION: &str = "eu-west-1";
const A_THIRD_REGION: &str = "ap-southeast-2";

/// The only `SubscriptionProviderSource` the model declares (`one of: RedHat`).
const RED_HAT: &str = "RedHat";

/// The `SubscriptionProviderStatus` a registration this service took reports, while the
/// validation of its access token has not landed yet.
const PENDING: &str = "PENDING";

/// The one sentence `RegisterSubscriptionProvider` refuses a credential it cannot use with.
const TOKEN_REFUSAL_MESSAGE: &str = "The subscription provider token is invalid or expired";

/// The account this emulator serves, which every ARN it mints carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The name the row's secret is created under in Secrets Manager, where the row names it by
/// the ARN that service minted.
const SECRET_NAME: &str = "rhel-offline-token";

/// The token a version-creating `CreateSecret` has to be given: "If you generate a raw HTTP
/// request to the Secrets Manager service endpoint, then you must generate a
/// ClientRequestToken and include it in the request."
/// (`CreateSecretRequest$ClientRequestToken`, that service's Smithy model.)
const CLIENT_REQUEST_TOKEN: &str = "11111111-2222-3333-4444-555555555555";

/// A Red Hat Subscription Manager OFFLINE token, written out as the three base64url
/// segments a compact JWS carries so this suite holds no second copy of that coding (the
/// tree's own is `service_common::jwt::encode_base64url`, which `core/router`'s manifest
/// does not reach).
///
/// Decoded, the segments are the JOSE header `{"alg":"RS256","typ":"JWT"}` and the payload
/// `{"typ":"Offline"}` — the claim that makes the credential the offline token a `RedHat`
/// registration admits ("the secret contains your Red Hat Offline token",
/// `RegisterSubscriptionProviderRequest$SecretArn`) — followed by a signature segment that
/// is merely present: a JWS signature is over the first two segments and nothing in this
/// emulator can verify one.
const OFFLINE_TOKEN: &str =
    "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJ0eXAiOiJPZmZsaW5lIn0.c2lnbmF0dXJl";

/// The longest Region label the AWS models admit: every `*Arn` pattern bounds that field at
/// `[a-z0-9-]{1,20}`, so a longer value is no Region an AWS endpoint has
/// (`service_common::arn::MAX_REGION_LENGTH`, which states the figure for the whole tree).
const MAX_REGION_LENGTH: usize = 20;

/// The longest Region FIELD the modelled `SecretArn` pattern admits:
/// `[a-z0-9-\.]{0,63}`, which is wider than the grammar above in both length and character
/// class — the gap this suite's non-label rows live in.
const MAX_SECRET_ARN_REGION_FIELD: usize = 63;

/// Account ids other than the one this emulator serves — twelve decimal digits, which is
/// what an ARN's account field holds.
const FOREIGN_ACCOUNTS: &[&str] = &["123456789012", "999999999999", "111122223333"];

/// License Manager Linux Subscriptions and Secrets Manager over ONE shared engine, exactly
/// as `core/router/src/lib.rs` mounts them.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These tests exercise the services, not SigV4: the credential scope is what carries
    // the Region, and the signature itself is the framework's concern.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::
            EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(engine),
    );
    router
}

/// What a call answered: the HTTP status, the wire error code (if any) and the body.
struct Answer {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    /// The error code of a refusal, or `"<none>"` where the answer carries none.
    fn code(&self) -> &str {
        self.error_type.as_deref().unwrap_or("<none>")
    }

    /// The `message` member of an error body, or `""` when there is none.
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    /// A string member of the body, or `None` when the body does not carry it.
    fn member(&self, name: &str) -> Option<&str> {
        self.body.get(name).and_then(Value::as_str)
    }
}

/// Dispatch one request through the router, signed for `region` in `signing_name`'s scope.
///
/// `target` carries the `X-Amz-Target` an `awsJson1_1` service is addressed with (Secrets
/// Manager); a request without one is this service's own `restJson1`.
async fn dispatch(
    router: &MultiServiceRouter,
    signing_name: &str,
    region: &str,
    method: &str,
    path: &str,
    target: Option<&str>,
    body: Option<&Value>,
) -> Answer {
    let host = format!("{signing_name}.{region}.amazonaws.com");
    let payload = match body {
        Some(document) => {
            Bytes::from(serde_json::to_vec(document).expect("the request body serializes"))
        }
        None => Bytes::new(),
    };
    let content_type = if target.is_some() {
        "application/x-amz-json-1.1"
    } else {
        "application/json"
    };
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host.as_str())
        .header("content-type", content_type)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTEXAMPLE/20260101/{region}/{signing_name}\
                 /aws4_request, SignedHeaders=host, Signature=0"
            ),
        );
    if let Some(target) = target {
        request = request.header("x-amz-target", target);
    }
    let response = router
        .dispatch(request.body(payload).expect("a well-formed request"))
        .await;
    let status = response.status().as_u16();
    let header_error = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let payload = response.into_body();
    let body: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    let error_type = header_error.or_else(|| {
        body.get("__type")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    Answer {
        status,
        error_type,
        body,
    }
}

/// Store the offline token in Secrets Manager under the name `name`, in `region`, through
/// that service's own `CreateSecret`, and answer the ARN it minted.
async fn create_secret(router: &MultiServiceRouter, region: &str, name: &str) -> String {
    let answer = dispatch(
        router,
        SECRETS,
        region,
        "POST",
        "/",
        Some("secretsmanager.CreateSecret"),
        Some(&json!({
            "Name": name,
            "SecretString": OFFLINE_TOKEN,
            "ClientRequestToken": CLIENT_REQUEST_TOKEN,
        })),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "seeding CreateSecret {name:?} in {region} expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer
        .member("ARN")
        .unwrap_or_else(|| {
            panic!(
                "CreateSecret reports the new secret's ARN, got {:?}",
                answer.body
            )
        })
        .to_string()
}

/// `RegisterSubscriptionProvider`, signed for `region`, naming `secret_arn`.
async fn register(router: &MultiServiceRouter, region: &str, secret_arn: &str) -> Answer {
    dispatch(
        router,
        SUBSCRIPTIONS,
        region,
        "POST",
        "/subscription/RegisterSubscriptionProvider",
        None,
        Some(&json!({
            "SubscriptionProviderSource": RED_HAT,
            "SecretArn": secret_arn,
        })),
    )
    .await
}

/// `GetRegisteredSubscriptionProvider`, the read side a created registration is proven
/// through.
async fn get_provider(router: &MultiServiceRouter, region: &str, provider_arn: &str) -> Answer {
    dispatch(
        router,
        SUBSCRIPTIONS,
        region,
        "POST",
        "/subscription/GetRegisteredSubscriptionProvider",
        None,
        Some(&json!({ "SubscriptionProviderArn": provider_arn })),
    )
    .await
}

/// An ARN as the `{resourceArn}` label of the tagging surface's path: its colons and
/// slashes are percent-encoded so the whole value reaches the server as one label.
fn label(arn: &str) -> String {
    arn.replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

/// `ListTagsForResource`: `GET /tags/{resourceArn}`.
async fn list_tags(router: &MultiServiceRouter, region: &str, resource_arn: &str) -> Answer {
    dispatch(
        router,
        SUBSCRIPTIONS,
        region,
        "GET",
        &format!("/tags/{}", label(resource_arn)),
        None,
        None,
    )
    .await
}

/// `TagResource`: `PUT /tags/{resourceArn}`, whose body carries the `tags` map.
async fn tag(router: &MultiServiceRouter, region: &str, resource_arn: &str) -> Answer {
    dispatch(
        router,
        SUBSCRIPTIONS,
        region,
        "PUT",
        &format!("/tags/{}", label(resource_arn)),
        None,
        Some(&json!({ "tags": { "env": "prod" } })),
    )
    .await
}

/// `secret_arn` with its Region field — the fourth of the six an ARN is written with —
/// replaced by `region`; `""` yields the partition-global form the modelled `SecretArn`
/// pattern admits (`[a-z0-9-\.]{0,63}`).
fn with_arn_region(secret_arn: &str, region: &str) -> String {
    let mut fields: Vec<&str> = secret_arn.splitn(6, ':').collect();
    assert_eq!(
        fields.len(),
        6,
        "case {secret_arn:?}: an ARN is written with six fields, got {}",
        fields.len()
    );
    fields[3] = region;
    fields.join(":")
}

/// A value of `length` characters drawn from the `[a-z0-9-]` class the models' Region
/// grammar states, for a row that is about the LENGTH of the field rather than about any
/// particular Region.
fn region_of_length(length: usize) -> String {
    const SUFFIX: &str = "-east-1";
    let label = format!("{}{SUFFIX}", "a".repeat(length - SUFFIX.len()));
    assert_eq!(
        label.len(),
        length,
        "case {label:?}: expected a label of {length} characters"
    );
    label
}

/// Assert `answer` is the registration this service TOOK, and that the resource it created
/// is readable back through `GetRegisteredSubscriptionProvider` reporting the very secret
/// the call named.
async fn assert_taken(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    secret_arn: &str,
    answer: &Answer,
) {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected the registration to be taken with HTTP 200, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.member("SubscriptionProviderStatus"),
        Some(PENDING),
        "case {case:?}: a taken registration reports the status of the registration action, \
         got {:?}",
        answer.body
    );
    let provider_arn = answer
        .member("SubscriptionProviderArn")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: no SubscriptionProviderArn in {:?}",
                answer.body
            )
        })
        .to_string();
    let fetched = get_provider(router, region, &provider_arn).await;
    assert_eq!(
        fetched.status, 200,
        "case {case:?}: the created registration must be readable back, got {} with {:?}",
        fetched.status, fetched.body
    );
    assert_eq!(
        fetched.member("SecretArn"),
        Some(secret_arn),
        "case {case:?}: the registration must report the secret it was registered with, got \
         {:?}",
        fetched.body
    );
}

/// Assert `answer` is the credential refusal: the modelled `ValidationException` at HTTP
/// 400, carrying the service's one sentence about the provider token.
fn assert_refused(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected the modelled ValidationException's HTTP 400, got {} with {:?}",
        answer.status, answer.body
    );
    assert!(
        answer.code().ends_with("ValidationException"),
        "case {case:?}: expected the error code ValidationException, got {:?} with {:?}",
        answer.code(),
        answer.body
    );
    assert_eq!(
        answer.message(),
        TOKEN_REFUSAL_MESSAGE,
        "case {case:?}: expected {TOKEN_REFUSAL_MESSAGE:?}, got {:?}",
        answer.message()
    );
}

/// One row of the first table: where the registration is addressed, what Region LABEL its
/// `SecretArn`'s own field names, and whether the read that field addresses finds the
/// secret.
struct LabelRow {
    case: &'static str,
    /// The Region the registration request is signed for.
    signed_region: &'static str,
    /// The Region label to write into the `SecretArn`'s field, or `None` to name the secret
    /// by the ARN Secrets Manager minted for it.
    arn_region: Option<&'static str>,
    /// Whether the registration must be taken, which is whether the Region the field names
    /// is the one holding the secret.
    accepted: bool,
}

fn label_rows() -> Vec<LabelRow> {
    vec![
        // The ARN's own field names the Region the secret is stored in, whatever Region the
        // registration is addressed to.
        LabelRow {
            case: "the secret's own ARN, addressed in the Region it is stored in",
            signed_region: SECRET_REGION,
            arn_region: None,
            accepted: true,
        },
        LabelRow {
            case: "the secret's own ARN, addressed in another Region",
            signed_region: ANOTHER_REGION,
            arn_region: None,
            accepted: true,
        },
        LabelRow {
            case: "the secret's own ARN, addressed in a third Region",
            signed_region: A_THIRD_REGION,
            arn_region: None,
            accepted: true,
        },
        // A Region label that is not where the secret lives: the read is addressed there,
        // and no secret of this account answers to the ARN in that Region.
        LabelRow {
            case: "a SecretArn whose Region field names another Region",
            signed_region: SECRET_REGION,
            arn_region: Some(ANOTHER_REGION),
            accepted: false,
        },
        LabelRow {
            case: "a SecretArn whose Region field names a third Region",
            signed_region: ANOTHER_REGION,
            arn_region: Some(A_THIRD_REGION),
            accepted: false,
        },
        // The boundary the grammar itself draws: the partition-global form's Region field
        // is empty, which is no Region of its own, so the read falls back to the Region the
        // request was signed for — which holds no secret answering to this identifier.
        LabelRow {
            case: "the partition-global form, whose Region field is empty",
            signed_region: SECRET_REGION,
            arn_region: Some(""),
            accepted: false,
        },
    ]
}

#[tokio::test]
async fn a_registration_reads_its_secret_in_the_region_its_secret_arn_names() {
    for row in label_rows() {
        // An emulator per row: the account holds ONE registration per source, so a row
        // whose credential is usable must be the call that creates it rather than one that
        // addresses the row before it.
        let router = router();
        let minted = create_secret(&router, SECRET_REGION, SECRET_NAME).await;
        let secret_arn = match row.arn_region {
            None => minted.clone(),
            Some(region) => with_arn_region(&minted, region),
        };
        let case = row.case;

        let answer = register(&router, row.signed_region, &secret_arn).await;

        if row.accepted {
            assert_taken(&router, row.signed_region, case, &secret_arn, &answer).await;
        } else {
            assert_refused(case, &answer);
        }
    }
}

/// One row of the second table: the value a `SecretArn`'s Region field carries, and whether
/// the models' Region grammar admits it as a LABEL.
///
/// Each row's secret is created under a credential scope naming that very value, so the
/// minted `SecretArn` carries it in its own Region field and Secrets Manager — which
/// resolves a `SecretId` in the Region the request addresses and in no other — answers for
/// it only where a reader addresses that value as a Region. That is what makes the
/// difference observable to a client: a value the grammar does not admit must never become
/// the authority the read is dialled at, so the secret planted behind it stays unreachable.
struct GrammarRow {
    case: String,
    /// The value in the Region field, and the scope the row's secret is created under.
    arn_region: String,
    /// Whether that value is a Region label — non-empty, at most [`MAX_REGION_LENGTH`]
    /// characters, `[a-z0-9-]` only.
    is_label: bool,
}

impl GrammarRow {
    fn new(case: &str, arn_region: String, is_label: bool) -> Self {
        Self {
            case: case.to_string(),
            arn_region,
            is_label,
        }
    }
}

fn grammar_rows() -> Vec<GrammarRow> {
    vec![
        // Region labels: an AWS Region, and the longest value the grammar admits. Both are
        // Regions a read is addressed in, so the secret planted behind them is reached.
        GrammarRow::new(
            "a Region field naming another AWS Region",
            ANOTHER_REGION.to_string(),
            true,
        ),
        GrammarRow::new(
            "a Region field at the longest label the grammar admits",
            region_of_length(MAX_REGION_LENGTH),
            true,
        ),
        // Outside the grammar, inside the modelled pattern: one character past the
        // published length bound, a value carrying dots — which read as text would land in
        // the AUTHORITY of the endpoint the read is dialled at — and the longest such value
        // the pattern admits.
        GrammarRow::new(
            "a Region field one character past the longest label",
            region_of_length(MAX_REGION_LENGTH + 1),
            false,
        ),
        GrammarRow::new(
            "a Region field carrying a dotted host label",
            "host.example.com".to_string(),
            false,
        ),
        GrammarRow::new(
            "a dotted Region field at the longest the modelled pattern admits",
            format!(
                "{}.example.com",
                region_of_length(MAX_SECRET_ARN_REGION_FIELD - ".example.com".len())
            ),
            false,
        ),
    ]
}

/// The Region field — the fourth — of the ARN `arn`.
fn arn_region_of(arn: &str) -> &str {
    arn.split(':')
        .nth(3)
        .unwrap_or_else(|| panic!("case {arn:?}: an ARN is written with six fields"))
}

#[tokio::test]
async fn a_region_field_outside_the_models_grammar_is_never_the_authority_of_the_read() {
    for row in grammar_rows() {
        let router = router();
        let case = &row.case;
        // The secret is planted "in" the row's value: Secrets Manager takes the Region a
        // request is addressed to from its credential scope, so the ARN it mints here
        // carries that value in its own Region field and answers to it nowhere else.
        let secret_arn = create_secret(&router, &row.arn_region, SECRET_NAME).await;
        assert_eq!(
            arn_region_of(&secret_arn),
            row.arn_region,
            "case {case:?}: the planted secret must carry the row's value in its ARN's \
             Region field, got {secret_arn:?}"
        );

        // Every registration here is addressed to a Region the secret is NOT in, so the
        // only reader that reaches the secret is one that takes the Region from the ARN.
        let answer = register(&router, SECRET_REGION, &secret_arn).await;

        if row.is_label {
            // A Region label is a Region: the read is addressed there and the credential is
            // read, whatever Region the registration itself was addressed to.
            assert_taken(&router, SECRET_REGION, case, &secret_arn, &answer).await;
        } else {
            // No label, so no Region: the read stays in the Region the request was signed
            // for — which holds no secret under this identifier — rather than dialling an
            // authority the caller's own text composed, so the credential cannot be read.
            assert_refused(case, &answer);
        }
    }
}

#[tokio::test]
async fn the_tagging_surface_decides_on_the_account_field_of_the_same_arn_parse() {
    let router = router();
    let secret_arn = create_secret(&router, SECRET_REGION, SECRET_NAME).await;
    let registered = register(&router, SECRET_REGION, &secret_arn).await;
    assert_eq!(
        registered.status, 200,
        "seeding the registration expected 200, got {} with {:?}",
        registered.status, registered.body
    );
    let provider_arn = registered
        .member("SubscriptionProviderArn")
        .unwrap_or_else(|| {
            panic!(
                "the registration reports the ARN it minted, got {:?}",
                registered.body
            )
        })
        .to_string();

    // The resource of THIS account is reached: the parse reads an account field that is
    // this emulator's, so nothing about the ARN refuses the request.
    let own = list_tags(&router, SECRET_REGION, &provider_arn).await;
    assert_eq!(
        own.status, 200,
        "case \"the registration's own ARN\": expected HTTP 200, got {} with {:?}",
        own.status, own.body
    );

    // An ARN of another account names a resource no document of this account can grant
    // anything over, on the read and on the write alike.
    for account in FOREIGN_ACCOUNTS {
        let foreign = provider_arn.replace(ACCOUNT, account);
        for (operation, answer) in [
            (
                "ListTagsForResource",
                list_tags(&router, SECRET_REGION, &foreign).await,
            ),
            ("TagResource", tag(&router, SECRET_REGION, &foreign).await),
        ] {
            let case = format!("{operation} on an ARN of account {account}");
            assert_eq!(
                answer.status, 403,
                "case {case:?}: expected HTTP 403, got {} with {:?}",
                answer.status, answer.body
            );
            assert!(
                answer.code().ends_with("AccessDeniedException"),
                "case {case:?}: expected the error code AccessDeniedException, got {:?} with \
                 {:?}",
                answer.code(),
                answer.body
            );
        }
    }

    // The boundary of the same rule: an ARN of THIS account that addresses no registration
    // keeps the surface's absent-resource answer, which is what a denial must not stand in
    // for.
    let absent =
        format!("arn:aws:{SUBSCRIPTIONS}:{SECRET_REGION}:{ACCOUNT}:subscription-provider/absent");
    let answer = list_tags(&router, SECRET_REGION, &absent).await;
    assert_eq!(
        answer.status, 404,
        "case \"an ARN of this account addressing no registration\": expected HTTP 404, got \
         {} with {:?}",
        answer.status, answer.body
    );
    assert!(
        answer.code().ends_with("NotFoundException"),
        "case \"an ARN of this account addressing no registration\": expected the error code \
         NotFoundException, got {:?} with {:?}",
        answer.code(),
        answer.body
    );
}
