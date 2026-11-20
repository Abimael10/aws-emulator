// SPDX-License-Identifier: CC0-1.0

//! The expression a `@pattern` clause of this service quotes for a member whose model text
//! spells an UNRESTRICTED CHARACTER RUN, over the real restJson1 request path
//! (`MultiServiceRouter::dispatch`).
//!
//! RULE: a `@pattern` clause quotes the expression this service publishes for the member's
//! shape, and that expression states a run admitting any sequence of characters as the one
//! any-character class — whichever of the equivalent spellings the model's own text uses for
//! it (this model writes one unbounded member's run as that class and another's as a dot
//! repetition widened with a newline alternative). The surrounding single-violation envelope,
//! the lowerCamel wire member name and the requirement's wording are unchanged, the rejected
//! value is not quoted back, and the CHECK is untouched: exactly the values the modelled
//! trait admits are still accepted, and exactly the ones it refuses are still refused.
//!
//! The recorded unit this was derived from
//! (`iot-managed-integrations.provisioning-profile-lifecycle-status`, one non-PEM
//! `CaCertificate`) is a single row of the table below. The rows differ along what the rule
//! generalises over: what the rejected value looks like (plain text, empty, a PEM framing
//! with the wrong label, only an opening framing, a body carrying newlines, a real
//! certificate with text after its closing framing), how many violations the request
//! carries, and the region it is signed for — the clause states the same published
//! expression for all of them. The accepted rows run the other way: a real certificate, with
//! and without its trailing newline and with newlines through its body, is admitted, so the
//! rendering decided nothing.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the suite's requests are signed for unless a case names another.
const REGION: &str = "us-east-1";

/// The collection route `CreateProvisioningProfile` is bound to
/// (`smithy_query(query="operation",operation="CreateProvisioningProfile")` -> POST
/// `/provisioning-profiles`).
const COLLECTION: &str = "/provisioning-profiles";

/// The wire error code the model's `ValidationException` is rendered under, and the status
/// its `@httpError` declares.
const VALIDATION: &str = "ValidationException";
const VALIDATION_STATUS: u16 = 400;

/// The status `CreateProvisioningProfile` answers an accepted request with
/// (`smithy_query(query="operation",operation="CreateProvisioningProfile")` -> 201).
const CREATED_STATUS: u16 = 201;

/// The `@pattern` of `CaCertificate` exactly as the model declares it
/// (`smithy_query(query="shape",shape="CaCertificate")`): the whole-value anchors, the two
/// PEM framings, and the unrestricted run between them spelled as a dot repetition widened
/// with a newline alternative.
const CA_CERTIFICATE_MODELLED: &str =
    r"^-----BEGIN CERTIFICATE-----.*(.|\n)*-----END CERTIFICATE-----\n?$";

/// The expression this service PUBLISHES for that shape: the modelled text without the
/// anchors the model wraps it in, with the run stated as the any-character class — the
/// spelling this model itself uses for the unbounded run of `CertificatePem`
/// (`smithy_query(query="shape",shape="CertificatePem")` -> pattern `^[\s\S]*$`).
const CA_CERTIFICATE_PUBLISHED: &str =
    r"-----BEGIN CERTIFICATE-----[\s\S]*-----END CERTIFICATE-----\n?";

/// The `@pattern` `ProvisioningProfileName` carries and the expression published for it: a
/// member of the SAME request whose text spells no such run, so the rendering leaves it
/// exactly as the anchors come off (`smithy_query(query="shape",
/// shape="ProvisioningProfileName")` -> pattern `^[0-9A-Za-z_-]+$`).
const PROFILE_NAME_MODELLED: &str = r"^[0-9A-Za-z_-]+$";
const PROFILE_NAME_PUBLISHED: &str = r"[0-9A-Za-z_-]+";

/// One HTTP answer, reduced to what the assertions read.
struct Reply {
    status: u16,
    /// The restJson1 error code, from `x-amzn-errortype`.
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    fn member(&self, name: &str) -> Option<&Value> {
        self.body.get(name).filter(|value| !value.is_null())
    }

    fn string(&self, name: &str) -> Option<&str> {
        self.member(name).and_then(Value::as_str)
    }

    /// The reason an error body carries, on the member the model declares for it.
    fn message(&self) -> &str {
        self.string("Message")
            .or_else(|| self.string("message"))
            .unwrap_or_default()
    }
}

/// A router holding one `iotmanagedintegrations` service over a fresh shared engine: the
/// same assembly `core/router` performs, with one service mounted.
fn emulator() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // The signature itself is not under test here, so requests are unauthenticated.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::new(engine::InMemoryEngine::new())));
    router
}

/// Send one request, signed for `region`, and reduce the answer. The `Authorization`
/// credential scope is what routes the request to this service and names its region.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Reply {
    let payload = body.map_or_else(bytes::Bytes::new, |value| {
        bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize the request body"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://iotmanagedintegrations.{region}.amazonaws.com{path}"
        ))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTESTTEST/20260101/{region}/\
                 iotmanagedintegrations/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply {
        status,
        error_type,
        body,
    }
}

/// [`call_in`] in [`REGION`].
async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Reply {
    call_in(router, REGION, method, path, body).await
}

/// One `CreateProvisioningProfile`, in [`REGION`].
async fn create(router: &MultiServiceRouter, body: Value) -> Reply {
    call(router, "POST", COLLECTION, Some(body)).await
}

/// The AWS validation envelope around ONE clause, as this service states it: the violation
/// count, the member path in lowerCamel, and the requirement — with the rejected value left
/// out of the clause's opening.
fn envelope(member_path: &str, requirement: &str) -> String {
    format!(
        "1 validation error detected: Value at '{member_path}' failed to satisfy constraint: \
         {requirement}"
    )
}

/// The requirement a `@pattern` violation states, quoting `expression`.
fn pattern_requirement(expression: &str) -> String {
    format!("Member must satisfy regular expression pattern: {expression}")
}

/// A real PEM certificate a caller could hold: the claim certificate a minted profile of
/// `router` carries. Taking it from the service's own output keeps the accepted rows driven
/// by material a client really has rather than by text in the shape of a certificate.
async fn a_real_certificate(router: &MultiServiceRouter) -> String {
    let minted = create(router, json!({"ProvisioningType": "FLEET_PROVISIONING"})).await;
    minted
        .string("ClaimCertificate")
        .unwrap_or_else(|| {
            panic!(
                "case \"certificate source\": expected a minted ClaimCertificate, got {} with \
                 body {:?}",
                minted.status, minted.body
            )
        })
        .to_string()
}

/// RULE: every value the member's modelled `@pattern` refuses is refused with the SAME
/// published expression quoted — the run stated as the any-character class, the anchors
/// gone, the rejected value not quoted back — whatever the value looks like and whichever
/// region the request is signed for.
#[tokio::test]
async fn a_run_crossing_newlines_is_quoted_as_the_any_character_class() {
    let router = emulator();
    let certificate = a_real_certificate(&router).await;

    // The row's own statement of the rule: the published expression differs from the
    // modelled trait text in exactly two things — the anchors the model wraps it in, and the
    // spelling of the run — so it carries neither anchor and neither the dot repetition nor
    // the newline alternative.
    assert_eq!(
        CA_CERTIFICATE_MODELLED,
        format!(
            "^{}$",
            CA_CERTIFICATE_PUBLISHED.replace(r"[\s\S]*", r".*(.|\n)*")
        ),
        "the published expression must be the modelled text inside its anchors with the run \
         restated, got {CA_CERTIFICATE_PUBLISHED:?} against {CA_CERTIFICATE_MODELLED:?}"
    );
    assert!(
        !CA_CERTIFICATE_PUBLISHED.starts_with('^')
            && !CA_CERTIFICATE_PUBLISHED.ends_with('$')
            && !CA_CERTIFICATE_PUBLISHED.contains(r"(.|\n)")
            && CA_CERTIFICATE_PUBLISHED.contains(r"[\s\S]*"),
        "the published expression carries no anchor and states the run as the class, got {:?}",
        CA_CERTIFICATE_PUBLISHED
    );

    // (case, the region the request is signed for, the refused `CaCertificate`)
    let cases: Vec<(&str, &str, String)> = vec![
        // The recorded row: a value that is not a certificate at all.
        ("plain text, no framing", REGION, "not-a-pem-certificate".to_string()),
        ("the empty string", REGION, String::new()),
        (
            "a PEM framing with another label",
            REGION,
            "-----BEGIN PRIVATE KEY-----\nbWF0ZXJpYWw=\n-----END PRIVATE KEY-----\n".to_string(),
        ),
        (
            "only the opening framing",
            REGION,
            "-----BEGIN CERTIFICATE-----\nbWF0ZXJpYWw=\n".to_string(),
        ),
        (
            "a base64 body carrying newlines but no framing",
            "eu-west-1",
            "bWF0ZXJpYWw=\nbWF0ZXJpYWw=\n".to_string(),
        ),
        (
            "a real certificate with text after its closing framing",
            "ap-southeast-2",
            format!("{}trailing", certificate.trim_end()),
        ),
    ];

    for (case, region, value) in &cases {
        let reply = call_in(
            &router,
            region,
            "POST",
            COLLECTION,
            Some(json!({"ProvisioningType": "FLEET_PROVISIONING", "CaCertificate": value})),
        )
        .await;

        assert_eq!(
            reply.status, VALIDATION_STATUS,
            "case {case:?}: expected HTTP {VALIDATION_STATUS}, got {} with body {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some(VALIDATION),
            "case {case:?}: expected the modelled {VALIDATION} code, got {:?}",
            reply.error_type
        );
        assert_eq!(
            reply.message(),
            envelope(
                "caCertificate",
                &pattern_requirement(CA_CERTIFICATE_PUBLISHED)
            ),
            "case {case:?}: expected the clause to quote the published expression, got {:?}",
            reply.message()
        );
        assert!(
            !reply.message().contains(CA_CERTIFICATE_MODELLED)
                && !reply.message().contains(r"(.|\n)"),
            "case {case:?}: the clause must not quote the model's own spelling of the run, \
             got {:?}",
            reply.message()
        );
        // `CaCertificate` is `@sensitive`, and this service quotes no rejected value at
        // all: the refused material must not come back on the wire.
        assert!(
            value.is_empty() || !reply.message().contains(value.as_str()),
            "case {case:?}: the clause must not echo the rejected value, got {:?}",
            reply.message()
        );
    }
}

/// RULE: the rendering changed no decision — every value the modelled trait admits is still
/// accepted, the run really crossing the newlines a PEM body is full of.
#[tokio::test]
async fn a_value_the_modelled_expression_admits_is_still_accepted() {
    let router = emulator();
    let certificate = a_real_certificate(&router).await;
    let without_trailing_newline = certificate.trim_end().to_string();
    let with_trailing_newline = format!("{without_trailing_newline}\n");

    // (case, the accepted `CaCertificate`)
    let cases: Vec<(&str, &str)> = vec![
        // A real certificate's PEM body wraps at 64 characters, so this row is the one that
        // shows the run matching across line terminators.
        (
            "a real certificate, as the service minted it",
            certificate.as_str(),
        ),
        (
            "the same certificate without its trailing newline",
            without_trailing_newline.as_str(),
        ),
        (
            "the same certificate with exactly one trailing newline",
            with_trailing_newline.as_str(),
        ),
    ];

    for (case, value) in &cases {
        assert!(
            value.contains('\n'),
            "case {case:?}: the accepted rows exist to cross a newline, got {value:?}"
        );
        let reply = create(
            &router,
            json!({"ProvisioningType": "FLEET_PROVISIONING", "CaCertificate": value}),
        )
        .await;
        assert_eq!(
            reply.status, CREATED_STATUS,
            "case {case:?}: expected HTTP {CREATED_STATUS}, got {} with body {:?}",
            reply.status, reply.body
        );
        assert!(
            !reply.message().contains("regular expression pattern"),
            "case {case:?}: a value the modelled expression admits is not refused for its \
             pattern, got {:?}",
            reply.message()
        );
    }
}

/// RULE: the rendering reaches ONE member's expression and leaves every other as it states
/// it — a request violating two patterns is stated in the multi-violation envelope, with the
/// run member's clause quoting the class form and the other member's quoting its own
/// expression unchanged.
#[tokio::test]
async fn a_second_violated_member_keeps_the_expression_published_for_it() {
    let router = emulator();

    let reply = create(
        &router,
        json!({
            "ProvisioningType": "FLEET_PROVISIONING",
            "Name": "not a legal name",
            "CaCertificate": "not-a-pem-certificate",
        }),
    )
    .await;

    let case = "a name and a CA certificate, both outside their patterns";
    assert_eq!(
        reply.status, VALIDATION_STATUS,
        "case {case:?}: expected HTTP {VALIDATION_STATUS}, got {} with body {:?}",
        reply.status, reply.body
    );
    let message = reply.message();
    assert!(
        message.starts_with("2 validation errors detected: "),
        "case {case:?}: expected the two-violation envelope, got {message:?}"
    );
    assert!(
        message.contains(&format!(
            "Value at 'caCertificate' failed to satisfy constraint: {}",
            pattern_requirement(CA_CERTIFICATE_PUBLISHED)
        )),
        "case {case:?}: expected the published expression for the run member, got {message:?}"
    );
    assert!(
        message.contains(&format!(
            "Value at 'name' failed to satisfy constraint: {}",
            pattern_requirement(PROFILE_NAME_PUBLISHED)
        )),
        "case {case:?}: expected the other member's own published expression, got {message:?}"
    );
    assert!(
        !message.contains(PROFILE_NAME_MODELLED) && !message.contains(CA_CERTIFICATE_MODELLED),
        "case {case:?}: neither clause quotes the modelled trait text, got {message:?}"
    );
}
