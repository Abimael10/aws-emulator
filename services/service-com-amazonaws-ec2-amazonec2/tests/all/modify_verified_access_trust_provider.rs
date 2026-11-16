// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyVerifiedAccessTrustProvider`, driven over the wire the service
//! actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test: a modify APPLIES THE MEMBERS THE REQUEST CARRIES to the stored trust
//! provider — member by member, at every depth of the options blocks it names — leaves every
//! member it does not carry as it was, stamps the trust provider's `LastUpdatedTime`, and
//! answers the record the store now holds, so a later read of the same trust provider reports
//! what the modify left behind. Around that:
//!
//! 1. every shape a modify request can take applies exactly what it carries, at the top level
//!    and inside each options block, and the members the service owns (the id, the kinds and
//!    `CreationTime`) are not touched;
//! 2. the answer is the STORE's record rather than the request echoed back, so
//!    `DescribeVerifiedAccessTrustProviders` reports the same trust provider afterwards, and an
//!    instance that has it ATTACHED reports the modified facts too;
//! 3. each KIND of trust provider takes its own options, so a member the stored kind does not
//!    take is refused rather than applied;
//! 4. a trust provider id this account holds no trust provider for modifies nothing and is
//!    refused, whatever that id looks like, and so is a client token past the service's bound;
//! 5. one `ClientToken` names one modification — a repeat applies nothing further, and the same
//!    token carrying a DIFFERENT request is refused rather than applied twice — and a `DryRun`
//!    reports the outcome the request would have had while writing nothing;
//! 6. the OIDC client secret stays WRITE-ONLY across a modify: a rotated secret is reported as
//!    the mask, never as the value the request sent, and the native application options report
//!    no secret at all — the response shape carries no member for one.
//!
//! Each table row is a distinct input along what the rule generalises over — which member the
//! request carries, how deep it sits, how many at once, whether the stored record already held
//! a value for it, which kind of trust provider is being modified (the model's own
//! `DEVICE_TRUST_PROVIDER_TYPE_VALUES` catalog for the device kind), which id is unresolvable
//! and in which form, how long a client token is — so no row is the specification.

use ec2::ops::types::DEVICE_TRUST_PROVIDER_TYPE_VALUES;
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, MAX_CLIENT_TOKEN_CHARS, REDACTED_CLIENT_SECRET,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{as_timestamp, call, element, error_code, error_message};

/// The Region every call here is signed for: a trust provider is an account-level resource —
/// its modelled shape names no Region — so the Region only selects the envelope.
const REGION: &str = "eu-central-1";

/// The sentence EC2 answers beside `IdempotentParameterMismatch`, spelled out here rather than
/// read from the crate so a change to the wording the handler sends is caught: the text is FIXED
/// for the class and names nothing of the request.
const TOKEN_MISMATCH_MESSAGE: &str = "Parameters on this idempotent request are inconsistent \
                                      with parameters used in previous request(s)";

/// The OIDC endpoints and client the seeded user trust provider is created with: every one of
/// them is a member a modify may replace, so each row can name the ones it leaves alone.
const SEEDED_ISSUER: &str = "https://login.example.test/";
const SEEDED_AUTHORIZATION: &str = "https://login.example.test/authorize";
const SEEDED_TOKEN_ENDPOINT: &str = "https://login.example.test/token";
const SEEDED_USER_INFO: &str = "https://login.example.test/userinfo";
const SEEDED_CLIENT_ID: &str = "client-42";
const SEEDED_SCOPE: &str = "openid";

/// The signing key URL the seeded device trust provider is created with.
const SEEDED_SIGNING_KEY_URL: &str = "https://device.example.test/keys";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// One member of a reported trust provider: the options block that holds it, when it is nested
/// in one, and the element's own name. `issuer` and `clientId` appear in TWO blocks, so a
/// member is only ever read through the block it belongs to.
type MemberPath = (Option<&'static str>, &'static str);

/// One row of the "applies what it carries" table: `(case, the modify's members, the members
/// the trust provider must then report)`.
type AppliedRow = (
    &'static str,
    Params,
    Vec<(MemberPath, Option<&'static str>)>,
);

fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// The create parameters of a user/OIDC trust provider: every OIDC member set, so each row of
/// the table can replace some and leave the rest.
fn user_create(description: &str, client_secret: &str) -> Params {
    vec![
        p("TrustProviderType", "user"),
        p("UserTrustProviderType", "oidc"),
        p("PolicyReferenceName", "refoidc"),
        p("Description", description),
        p("OidcOptions.Issuer", SEEDED_ISSUER),
        p("OidcOptions.AuthorizationEndpoint", SEEDED_AUTHORIZATION),
        p("OidcOptions.TokenEndpoint", SEEDED_TOKEN_ENDPOINT),
        p("OidcOptions.UserInfoEndpoint", SEEDED_USER_INFO),
        p("OidcOptions.ClientId", SEEDED_CLIENT_ID),
        p("OidcOptions.ClientSecret", client_secret),
        p("OidcOptions.Scope", SEEDED_SCOPE),
    ]
}

/// The create parameters of a device trust provider of `sub_type`, every member of its one
/// options block set.
fn device_create(sub_type: &str, description: &str) -> Params {
    vec![
        p("TrustProviderType", "device"),
        p("DeviceTrustProviderType", sub_type),
        p("PolicyReferenceName", format!("ref{sub_type}")),
        p("Description", description),
        p("DeviceOptions.TenantId", format!("tenant-{sub_type}")),
        p("DeviceOptions.PublicSigningKeyUrl", SEEDED_SIGNING_KEY_URL),
    ]
}

/// Create one trust provider that must succeed, and answer the id it was minted.
async fn seed(router: &MultiServiceRouter, case: &str, params: &Params) -> String {
    let (status, body) = call(router, REGION, "CreateVerifiedAccessTrustProvider", params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create under test must succeed, got {status} with body {body}"
    );
    let id = element(&body, "verifiedAccessTrustProviderId").unwrap_or_default();
    assert!(
        id.starts_with("vatp-"),
        "case {case:?}: expected a minted trust provider id, got {id:?}"
    );
    id
}

/// One `ModifyVerifiedAccessTrustProvider` call on `trust_provider_id`, with `params` beside the
/// id.
async fn modify(
    router: &MultiServiceRouter,
    trust_provider_id: &str,
    params: &[(String, String)],
) -> (u16, String) {
    let mut body = vec![p("VerifiedAccessTrustProviderId", trust_provider_id)];
    body.extend_from_slice(params);
    call(
        router,
        REGION,
        "ModifyVerifiedAccessTrustProvider",
        &body,
    )
    .await
}

/// The body `DescribeVerifiedAccessTrustProviders` answers for the one trust provider
/// `trust_provider_id`.
async fn described(router: &MultiServiceRouter, case: &str, trust_provider_id: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "DescribeVerifiedAccessTrustProviders",
        &[p("VerifiedAccessTrustProviderId.1", trust_provider_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe of {trust_provider_id} must succeed, got {status} with \
         body {body}"
    );
    body
}

/// The text of the `child` element of the first `parent` element in `body`, read with the XML
/// parser so a member of one options block is never confused with the same-named member of
/// another.
fn nested(body: &str, parent: &str, child: &str) -> Option<String> {
    let document = roxmltree::Document::parse(body)
        .unwrap_or_else(|error| panic!("the response body must be XML ({error}): {body}"));
    document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == parent)?
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == child)
        .map(|node| node.text().unwrap_or_default().to_string())
}

/// The member `path` of the trust provider `body` reports, or `None` when the report carries no
/// such member.
fn reported(body: &str, path: MemberPath) -> Option<String> {
    match path.0 {
        Some(parent) => nested(body, parent, path.1),
        None => element(body, path.1),
    }
}

/// The description the STORE kept for `trust_provider_id`, read off the state the handlers
/// share.
fn stored_description(engine: &engine::InMemoryEngine, trust_provider_id: &str) -> Option<String> {
    engine
        .state::<Ec2State>()
        .lock()
        .verified_access_trust_providers
        .get(trust_provider_id)
        .and_then(|provider| provider.description.clone())
}

/// Assert every member of `expected` is what the trust provider in `body` reports.
fn assert_members(case: &str, label: &str, body: &str, expected: &[(MemberPath, Option<&str>)]) {
    for (path, want) in expected {
        let actual = reported(body, *path);
        assert_eq!(
            actual.as_deref(),
            *want,
            "case {case:?}: {label} must report {path:?} as {want:?}, got {actual:?}"
        );
    }
}

/// RULE over a user/OIDC trust provider: each shape a modify request can take applies exactly
/// the members it carries — at the top level and inside each options block — leaves the ones it
/// does not carry as they were, and the answer is the store's record, so the describe of the
/// same trust provider reports it too.
#[tokio::test]
async fn a_modify_applies_what_it_carries_and_leaves_the_rest() {
    let rows: Vec<AppliedRow> = vec![
        (
            "a description over the one the create set",
            vec![p("Description", "the support provider")],
            vec![
                ((None, "description"), Some("the support provider")),
                ((Some("oidcOptions"), "issuer"), Some(SEEDED_ISSUER)),
                (
                    (Some("oidcOptions"), "tokenEndpoint"),
                    Some(SEEDED_TOKEN_ENDPOINT),
                ),
            ],
        ),
        (
            "one OIDC member while the rest of the block stays",
            vec![p("OidcOptions.Issuer", "https://idp.example.test/")],
            vec![
                ((None, "description"), Some("the sales provider")),
                (
                    (Some("oidcOptions"), "issuer"),
                    Some("https://idp.example.test/"),
                ),
                (
                    (Some("oidcOptions"), "authorizationEndpoint"),
                    Some(SEEDED_AUTHORIZATION),
                ),
                (
                    (Some("oidcOptions"), "tokenEndpoint"),
                    Some(SEEDED_TOKEN_ENDPOINT),
                ),
                (
                    (Some("oidcOptions"), "userInfoEndpoint"),
                    Some(SEEDED_USER_INFO),
                ),
                ((Some("oidcOptions"), "clientId"), Some(SEEDED_CLIENT_ID)),
                ((Some("oidcOptions"), "scope"), Some(SEEDED_SCOPE)),
                (
                    (Some("oidcOptions"), "clientSecret"),
                    Some(REDACTED_CLIENT_SECRET),
                ),
            ],
        ),
        (
            "several OIDC members at once",
            vec![
                p("OidcOptions.TokenEndpoint", "https://idp.example.test/token"),
                p("OidcOptions.ClientId", "client-77"),
                p("OidcOptions.Scope", "openid profile"),
            ],
            vec![
                ((Some("oidcOptions"), "issuer"), Some(SEEDED_ISSUER)),
                (
                    (Some("oidcOptions"), "tokenEndpoint"),
                    Some("https://idp.example.test/token"),
                ),
                ((Some("oidcOptions"), "clientId"), Some("client-77")),
                ((Some("oidcOptions"), "scope"), Some("openid profile")),
                (
                    (Some("oidcOptions"), "userInfoEndpoint"),
                    Some(SEEDED_USER_INFO),
                ),
            ],
        ),
        (
            "the description and one OIDC member together",
            vec![
                p("Description", "the support provider"),
                p(
                    "OidcOptions.UserInfoEndpoint",
                    "https://idp.example.test/userinfo",
                ),
            ],
            vec![
                ((None, "description"), Some("the support provider")),
                (
                    (Some("oidcOptions"), "userInfoEndpoint"),
                    Some("https://idp.example.test/userinfo"),
                ),
                ((Some("oidcOptions"), "issuer"), Some(SEEDED_ISSUER)),
            ],
        ),
        (
            "the native application OIDC options onto a provider that had none",
            vec![
                p(
                    "NativeApplicationOidcOptions.Issuer",
                    "https://native.example.test/",
                ),
                p(
                    "NativeApplicationOidcOptions.PublicSigningKeyEndpoint",
                    "https://native.example.test/keys",
                ),
                p("NativeApplicationOidcOptions.ClientId", "native-9"),
                p("NativeApplicationOidcOptions.Scope", "openid email"),
            ],
            vec![
                (
                    (Some("nativeApplicationOidcOptions"), "issuer"),
                    Some("https://native.example.test/"),
                ),
                (
                    (
                        Some("nativeApplicationOidcOptions"),
                        "publicSigningKeyEndpoint",
                    ),
                    Some("https://native.example.test/keys"),
                ),
                (
                    (Some("nativeApplicationOidcOptions"), "clientId"),
                    Some("native-9"),
                ),
                (
                    (Some("nativeApplicationOidcOptions"), "scope"),
                    Some("openid email"),
                ),
                // The two OIDC blocks are each other's neighbours, so setting one leaves the
                // other exactly as the create left it.
                ((Some("oidcOptions"), "issuer"), Some(SEEDED_ISSUER)),
                ((Some("oidcOptions"), "clientId"), Some(SEEDED_CLIENT_ID)),
            ],
        ),
        (
            "the server side encryption options",
            vec![
                p("SseSpecification.CustomerManagedKeyEnabled", "true"),
                p(
                    "SseSpecification.KmsKeyArn",
                    "arn:aws:kms:eu-central-1:123456789012:key/11111111-2222-3333-4444-555555555555",
                ),
            ],
            vec![
                (
                    (Some("sseSpecification"), "customerManagedKeyEnabled"),
                    Some("true"),
                ),
                (
                    (Some("sseSpecification"), "kmsKeyArn"),
                    Some(
                        "arn:aws:kms:eu-central-1:123456789012:key/11111111-2222-3333-4444-555555555555",
                    ),
                ),
                ((None, "description"), Some("the sales provider")),
            ],
        ),
        (
            "no member at all beside the trust provider id",
            Vec::new(),
            vec![
                ((None, "description"), Some("the sales provider")),
                ((Some("oidcOptions"), "issuer"), Some(SEEDED_ISSUER)),
                (
                    (Some("sseSpecification"), "customerManagedKeyEnabled"),
                    Some("false"),
                ),
            ],
        ),
    ];

    for (case, members, expected) in rows {
        let (router, engine) = emulator();
        let trust_provider_id =
            seed(&router, case, &user_create("the sales provider", "secret-42")).await;
        let created = described(&router, case, &trust_provider_id).await;
        let creation_time = element(&created, "creationTime").unwrap_or_default();

        let (status, body) = modify(&router, &trust_provider_id, &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "verifiedAccessTrustProviderId").as_deref(),
            Some(trust_provider_id.as_str()),
            "case {case:?}: expected the trust provider the request named, got {:?}",
            element(&body, "verifiedAccessTrustProviderId")
        );
        assert_members(case, "the modify", &body, &expected);

        // The members the service owns: the kinds and the policy reference name are no member
        // of a modify request, so they stay as the create left them, and the creation time does
        // not move.
        assert_members(
            case,
            "the modify",
            &body,
            &[
                ((None, "trustProviderType"), Some("user")),
                ((None, "userTrustProviderType"), Some("oidc")),
                ((None, "policyReferenceName"), Some("refoidc")),
                ((None, "creationTime"), Some(creation_time.as_str())),
            ],
        );
        let last_updated = element(&body, "lastUpdatedTime").unwrap_or_default();
        assert!(
            as_timestamp(&last_updated)
                .zip(as_timestamp(&creation_time))
                .is_some_and(|(updated, created)| updated >= created),
            "case {case:?}: expected a lastUpdatedTime at or after the creationTime \
             {creation_time:?}, got {last_updated:?}"
        );

        // RULE, read back: the answer is the STORE's record, not the request echoed.
        let after = described(&router, case, &trust_provider_id).await;
        assert_members(case, "the describe", &after, &expected);
        assert_eq!(
            element(&after, "lastUpdatedTime").as_deref(),
            Some(last_updated.as_str()),
            "case {case:?}: the modify answers the stored record, so its lastUpdatedTime is the \
             one a later read reports, got {:?}",
            element(&after, "lastUpdatedTime")
        );
        let stored = stored_description(&engine, &trust_provider_id);
        let expected_description = expected
            .iter()
            .find(|(path, _)| *path == (None, "description"))
            .and_then(|(_, want)| *want);
        if let Some(want) = expected_description {
            assert_eq!(
                stored.as_deref(),
                Some(want),
                "case {case:?}: expected the store to keep description {want:?}, got {stored:?}"
            );
        }
    }
}

/// RULE over the DEVICE kind, for every sub-type the model publishes
/// (`DEVICE_TRUST_PROVIDER_TYPE_VALUES`): a device provider's own options block is applied
/// member by member too, and the tenant a modify carries no member for stays as the create set
/// it.
#[tokio::test]
async fn a_modify_of_a_device_provider_applies_its_own_options() {
    for sub_type in DEVICE_TRUST_PROVIDER_TYPE_VALUES {
        let case = format!("the {sub_type} device provider");
        let (router, _engine) = emulator();
        let trust_provider_id =
            seed(&router, &case, &device_create(sub_type, "the laptops")).await;

        let (status, body) = modify(
            &router,
            &trust_provider_id,
            &[
                p("Description", "the managed laptops"),
                p(
                    "DeviceOptions.PublicSigningKeyUrl",
                    "https://device.example.test/rotated",
                ),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let expected: Vec<(MemberPath, Option<&str>)> = vec![
            ((None, "description"), Some("the managed laptops")),
            ((None, "trustProviderType"), Some("device")),
            ((None, "deviceTrustProviderType"), Some(sub_type)),
            (
                (Some("deviceOptions"), "publicSigningKeyUrl"),
                Some("https://device.example.test/rotated"),
            ),
        ];
        assert_members(&case, "the modify", &body, &expected);
        // A modify's device options carry no tenant id, so the tenant the create named is the
        // tenant the provider keeps.
        let tenant = nested(&body, "deviceOptions", "tenantId");
        assert_eq!(
            tenant.as_deref(),
            Some(format!("tenant-{sub_type}").as_str()),
            "case {case:?}: a modify carries no tenant id, so the create's must stand, got \
             {tenant:?}"
        );

        let after = described(&router, &case, &trust_provider_id).await;
        assert_members(&case, "the describe", &after, &expected);
    }
}

/// RULE's kind rules and refusals: a member the STORED kind of trust provider does not take is
/// refused rather than applied, an id this account holds no trust provider for modifies nothing
/// whatever it looks like, and a client token is bounded in characters — on the bound and one
/// past it.
#[tokio::test]
async fn a_modify_refuses_a_member_the_kind_does_not_take_and_an_unresolvable_id() {
    // (case, an id no create minted) — the long and the short EC2 id form, an id of another
    // Verified Access resource kind, and a string that is no EC2 id at all.
    let unminted: Vec<(&str, &str)> = vec![
        (
            "an unminted long-form trust provider id",
            "vatp-0123456789abcdef0",
        ),
        ("an unminted short-form trust provider id", "vatp-01234567"),
        ("an instance id", "vai-0123456789abcdef0"),
        ("an id of no EC2 resource", "not-a-trust-provider"),
    ];
    for (case, trust_provider_id) in unminted {
        let (router, engine) = emulator();
        let held = seed(&router, case, &user_create("the one provider", "secret-42")).await;
        let (status, body) = modify(
            &router,
            trust_provider_id,
            &[p("Description", "a description nothing keeps")],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a trust provider nothing minted, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidVerifiedAccessTrustProviderId.NotFound",
            "case {case:?}: expected the trust provider's own NotFound code, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("VerifiedAccessTrustProvider {trust_provider_id} does not exist"),
            "case {case:?}: expected the refusal to name the id the request gave, got {}",
            error_message(&body)
        );
        assert_eq!(
            stored_description(&engine, &held).as_deref(),
            Some("the one provider"),
            "case {case:?}: a refused modify changes no other trust provider, got {:?}",
            stored_description(&engine, &held)
        );
    }

    let token = |chars: usize| "t".repeat(chars);
    // (case, whether the trust provider under test is the user kind, the members beside the id,
    // the HTTP status, the error code an accepted modify reports as the empty string).
    let rows: Vec<(&str, bool, Params, u16, &str)> = vec![
        (
            "device options on a user trust provider",
            true,
            vec![p(
                "DeviceOptions.PublicSigningKeyUrl",
                "https://device.example.test/keys",
            )],
            400,
            "InvalidParameterCombination",
        ),
        (
            "OIDC options on a device trust provider",
            false,
            vec![p("OidcOptions.Issuer", "https://idp.example.test/")],
            400,
            "InvalidParameterCombination",
        ),
        (
            "native application OIDC options on a device trust provider",
            false,
            vec![p(
                "NativeApplicationOidcOptions.Issuer",
                "https://native.example.test/",
            )],
            400,
            "InvalidParameterCombination",
        ),
        (
            "the options the user kind does take",
            true,
            vec![p("OidcOptions.Issuer", "https://idp.example.test/")],
            200,
            "",
        ),
        (
            "the options the device kind does take",
            false,
            vec![p(
                "DeviceOptions.PublicSigningKeyUrl",
                "https://device.example.test/rotated",
            )],
            200,
            "",
        ),
        (
            "as long a client token as the service takes",
            true,
            vec![p("ClientToken", token(MAX_CLIENT_TOKEN_CHARS))],
            200,
            "",
        ),
        (
            "one client token character too many",
            true,
            vec![p("ClientToken", token(MAX_CLIENT_TOKEN_CHARS + 1))],
            400,
            "InvalidParameterValue",
        ),
    ];
    for (case, is_user, params, expected_status, expected_code) in rows {
        let (router, engine) = emulator();
        let create = if is_user {
            user_create("the one provider", "secret-42")
        } else {
            device_create("jamf", "the one provider")
        };
        let trust_provider_id = seed(&router, case, &create).await;
        let mut members = vec![p("Description", "the modified provider")];
        members.extend(params);
        let (status, body) = modify(&router, &trust_provider_id, &members).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected the {expected_code:?} refusal, got {body}"
        );
        let expected_description = if expected_status == 200 {
            "the modified provider"
        } else {
            "the one provider"
        };
        assert_eq!(
            stored_description(&engine, &trust_provider_id).as_deref(),
            Some(expected_description),
            "case {case:?}: expected the store to hold {expected_description:?}, got {:?}",
            stored_description(&engine, &trust_provider_id)
        );
    }
}

/// RULE's idempotency: one `ClientToken` names one modification, so a repeat of the request
/// applies nothing further — not even a second `LastUpdatedTime` — while the same token carrying
/// a different request is refused rather than applied a second time, and a token the store has
/// not seen applies a modification of its own.
#[tokio::test]
async fn one_client_token_names_one_modification() {
    let (router, engine) = emulator();
    let mut create = user_create("the provider under test", "secret-42");
    create.push(p("ClientToken", "the-create-token"));
    let trust_provider_id = seed(&router, "the provider under test", &create).await;

    let first_params = vec![
        p("ClientToken", "the-first-token"),
        p("Description", "the modified provider"),
        p("OidcOptions.Issuer", "https://idp.example.test/"),
    ];
    let (status, body) = modify(&router, &trust_provider_id, &first_params).await;
    assert_eq!(
        status, 200,
        "the first modify must succeed, got {status} with body {body}"
    );
    let applied_at = element(&body, "lastUpdatedTime").unwrap_or_default();

    // (case, the members of the next call, its HTTP status, its error code, the description the
    // store must hold afterwards, whether its lastUpdatedTime must be the first's).
    type Row = (&'static str, Params, u16, &'static str, &'static str, bool);
    let rows: Vec<Row> = vec![
        (
            "the same token and the same request",
            first_params.clone(),
            200,
            "",
            "the modified provider",
            true,
        ),
        (
            "the same token and a different description",
            vec![
                p("ClientToken", "the-first-token"),
                p("Description", "a description nothing keeps"),
                p("OidcOptions.Issuer", "https://idp.example.test/"),
            ],
            400,
            "IdempotentParameterMismatch",
            "the modified provider",
            false,
        ),
        (
            "the same token and a different member of the options block",
            vec![
                p("ClientToken", "the-first-token"),
                p("Description", "the modified provider"),
                p("OidcOptions.Issuer", "https://other.example.test/"),
            ],
            400,
            "IdempotentParameterMismatch",
            "the modified provider",
            false,
        ),
        (
            "the same token and one member more",
            vec![
                p("ClientToken", "the-first-token"),
                p("Description", "the modified provider"),
                p("OidcOptions.Issuer", "https://idp.example.test/"),
                p("OidcOptions.Scope", "openid profile"),
            ],
            400,
            "IdempotentParameterMismatch",
            "the modified provider",
            false,
        ),
        (
            // A token is read against the request it was given with, so the token the CREATE was
            // given names no modification: this is a modification, not a replay of that create.
            "the token the create was given",
            vec![
                p("ClientToken", "the-create-token"),
                p("Description", "modified under the create's token"),
            ],
            200,
            "",
            "modified under the create's token",
            false,
        ),
        (
            "a token the store has not seen",
            vec![
                p("ClientToken", "the-second-token"),
                p("Description", "modified again"),
            ],
            200,
            "",
            "modified again",
            false,
        ),
        (
            "no token at all",
            vec![p("Description", "modified without a token")],
            200,
            "",
            "modified without a token",
            false,
        ),
    ];

    for (case, params, expected_status, expected_code, expected_description, replayed) in rows {
        let (status, body) = modify(&router, &trust_provider_id, &params).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected the {expected_code:?} refusal, got {body}"
        );
        if expected_code == "IdempotentParameterMismatch" {
            assert_eq!(
                error_message(&body),
                TOKEN_MISMATCH_MESSAGE,
                "case {case:?}: expected the fixed sentence of that class, got {}",
                error_message(&body)
            );
        }
        assert_eq!(
            stored_description(&engine, &trust_provider_id).as_deref(),
            Some(expected_description),
            "case {case:?}: expected the store to hold {expected_description:?}, got {:?}",
            stored_description(&engine, &trust_provider_id)
        );
        if replayed {
            assert_eq!(
                element(&body, "lastUpdatedTime").as_deref(),
                Some(applied_at.as_str()),
                "case {case:?}: a replay applies nothing further, so the trust provider keeps \
                 the lastUpdatedTime the first call stamped, got {:?}",
                element(&body, "lastUpdatedTime")
            );
        }
        // The issuer is only ever moved by the first call and the rows that were refused, so a
        // provider still reporting the first call's issuer is one no refused call wrote to.
        let after = described(&router, case, &trust_provider_id).await;
        assert_eq!(
            nested(&after, "oidcOptions", "issuer").as_deref(),
            Some("https://idp.example.test/"),
            "case {case:?}: a refused modify writes nothing, so the issuer is the one the first \
             call applied, got {:?}",
            nested(&after, "oidcOptions", "issuer")
        );
    }
}

/// RULE's `DryRun`: the flag reports the outcome the request WOULD have had — the refusal when
/// it would have been refused — and writes nothing either way.
#[tokio::test]
async fn a_dry_run_reports_the_outcome_and_writes_nothing() {
    // (case, whether the request names the trust provider that exists, the members beside the
    // flag, the HTTP status, the error code).
    let rows: Vec<(&str, bool, Params, u16, &str)> = vec![
        (
            "a modify that would have succeeded",
            true,
            vec![p("Description", "a description nothing keeps")],
            412,
            "DryRunOperation",
        ),
        (
            "a modify that carries no member to apply",
            true,
            Vec::new(),
            412,
            "DryRunOperation",
        ),
        (
            "a modify that would have been refused for its options block",
            true,
            vec![p(
                "DeviceOptions.PublicSigningKeyUrl",
                "https://device.example.test/keys",
            )],
            400,
            "InvalidParameterCombination",
        ),
        (
            "a modify that would have been refused for its trust provider id",
            false,
            vec![p("Description", "a description nothing keeps")],
            400,
            "InvalidVerifiedAccessTrustProviderId.NotFound",
        ),
    ];

    for (case, resolvable, extra, expected_status, expected_code) in rows {
        let (router, engine) = emulator();
        let trust_provider_id =
            seed(&router, case, &user_create("the one provider", "secret-42")).await;
        let named = if resolvable {
            trust_provider_id.clone()
        } else {
            "vatp-0123456789abcdef0".to_string()
        };
        let mut params = vec![p("DryRun", "true")];
        params.extend(extra);
        let (status, body) = modify(&router, &named, &params).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected the {expected_code:?} refusal, got {body}"
        );
        assert_eq!(
            stored_description(&engine, &trust_provider_id).as_deref(),
            Some("the one provider"),
            "case {case:?}: a dry run writes nothing, got {:?}",
            stored_description(&engine, &trust_provider_id)
        );
    }
}

/// RULE for the write-only client secret: a modify that rotates the OIDC secret reports the mask
/// in its place and never the value it sent, and the native application options report no secret
/// at all — the response shape carries no member for one.
#[tokio::test]
async fn a_rotated_client_secret_is_never_reported() {
    let case = "a rotated client secret";
    let (router, _engine) = emulator();
    let trust_provider_id = seed(&router, case, &user_create("the one provider", "secret-42")).await;

    let (status, body) = modify(
        &router,
        &trust_provider_id,
        &[
            p("OidcOptions.ClientSecret", "rotated-oidc-secret"),
            p(
                "NativeApplicationOidcOptions.Issuer",
                "https://native.example.test/",
            ),
            p(
                "NativeApplicationOidcOptions.ClientSecret",
                "native-app-secret",
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );

    for report in [body.clone(), described(&router, case, &trust_provider_id).await] {
        assert_eq!(
            nested(&report, "oidcOptions", "clientSecret").as_deref(),
            Some(REDACTED_CLIENT_SECRET),
            "case {case:?}: expected the mask in place of the rotated secret, got {:?}",
            nested(&report, "oidcOptions", "clientSecret")
        );
        assert_eq!(
            nested(&report, "nativeApplicationOidcOptions", "clientSecret"),
            None,
            "case {case:?}: the native application options report no secret, got {:?}",
            nested(&report, "nativeApplicationOidcOptions", "clientSecret")
        );
        for secret in ["rotated-oidc-secret", "native-app-secret"] {
            assert!(
                !report.contains(secret),
                "case {case:?}: no report may carry the secret {secret:?}, got {report}"
            );
        }
    }
}

/// RULE read through the OTHER reader of a trust provider's facts: an instance that has the
/// trust provider ATTACHED carries a copy of its description and kinds, so a modify of the
/// description is what that instance reports afterwards too.
#[tokio::test]
async fn an_attached_instance_reports_the_modified_description() {
    let case = "an attached trust provider";
    let (router, _engine) = emulator();
    let trust_provider_id = seed(&router, case, &user_create("the sales provider", "secret-42")).await;
    let (status, body) = call(
        &router,
        REGION,
        "CreateVerifiedAccessInstance",
        &[p("Description", "the instance under test")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the instance under test must be created, got {status} with body {body}"
    );
    let instance_id = element(&body, "verifiedAccessInstanceId").unwrap_or_default();

    let (status, body) = call(
        &router,
        REGION,
        "AttachVerifiedAccessTrustProvider",
        &[
            p("VerifiedAccessInstanceId", &instance_id),
            p("VerifiedAccessTrustProviderId", &trust_provider_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the attach must succeed, got {status} with body {body}"
    );

    let (status, body) = modify(
        &router,
        &trust_provider_id,
        &[p("Description", "the support provider")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );

    let (status, body) = call(
        &router,
        REGION,
        "DescribeVerifiedAccessInstances",
        &[p("VerifiedAccessInstanceId.1", &instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe of the instance must succeed, got {status} with body {body}"
    );
    let attached = element(&body, "verifiedAccessTrustProviderSet").unwrap_or_default();
    assert_eq!(
        element(&attached, "verifiedAccessTrustProviderId").as_deref(),
        Some(trust_provider_id.as_str()),
        "case {case:?}: the instance must still report the trust provider it has attached, got \
         {body}"
    );
    assert_eq!(
        element(&attached, "description").as_deref(),
        Some("the support provider"),
        "case {case:?}: the attached entry is a copy of the trust provider's own facts, so it \
         reports the modified description, got {:?}",
        element(&attached, "description")
    );
}
