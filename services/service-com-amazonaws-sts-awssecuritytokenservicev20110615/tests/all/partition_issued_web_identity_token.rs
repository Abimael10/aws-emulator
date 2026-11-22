// SPDX-License-Identifier: CC0-1.0

//! A token the caller's OWN partition minted, presented back to `AssumeRoleWithWebIdentity`.
//!
//! RULE: web identity federation hosted inside the partition is OUTBOUND only — the JWT
//! `GetWebIdentityToken` vends against the account's own issuer is for a party outside the
//! partition to verify — so presenting such a token to this partition's web identity
//! exchange is refused as a token class the exchange does not serve: `InvalidIdentityToken`
//! (HTTP 400) with that class's own fixed sentence, naming no value of the request, and no
//! credentials are minted. A token of any OTHER issuer keeps the class it already earned.
//!
//! Each test is a TABLE whose rows differ along what the rule generalises over: which
//! algorithm the token was signed under, how many audiences it names, how long it lasts,
//! whether the role the exchange names exists, and whether the account still has the feature
//! on when the token is presented. No single request is the specification.
//!
//! The awsQuery harness (`emulator_with_iam`, `call`, `query`, `element`, `member`,
//! `seed_demo_role`, `LONG_TERM_KEY`) is the one `tests/all/sts_sessions.rs` owns and the
//! other suites in this crate share.

use smithy_server_runtime::MultiServiceRouter;

use super::sts_sessions::{
    call, element, emulator_account, emulator_with_iam, member, query, seed_demo_role,
    LONG_TERM_KEY,
};
use super::web_identity_federation::segment;

/// IAM's SigV4 signing name and awsQuery API version, which is how the account-level
/// federation calls of this suite resolve to that service.
const IAM_SIGNING_NAME: &str = "iam";
const IAM_API_VERSION: &str = "2010-05-08";

/// The exchange under test.
const ACTION: &str = "AssumeRoleWithWebIdentity";

/// The awsQuery error code `InvalidIdentityTokenException` carries
/// (`smithy_query(query="operation", operation="AssumeRoleWithWebIdentity")` ->
/// `awsQueryError {code: InvalidIdentityToken, httpResponseCode: 400}`).
const INVALID_IDENTITY_TOKEN_CODE: &str = "InvalidIdentityToken";

/// The status that refusal carries, from the same `awsQueryError` trait.
const INVALID_IDENTITY_TOKEN_STATUS: u16 = 400;

/// The whole message this refusal class carries, spelled out here rather than read from the
/// service, so a change of wording fails this suite instead of following it. It interpolates
/// nothing: neither the issuer, nor the role, nor the token appears in it.
const STS_ISSUED_TOKEN_MESSAGE: &str =
    "AssumeRoleWithWebIdentity with STS-issued tokens is not yet supported.";

/// How AWS words the refusal of a token whose issuer the account can resolve no provider
/// for: the class every OTHER unresolvable issuer keeps, which is what tells this suite the
/// new class did not swallow it.
fn no_provider_message(issuer: &str) -> String {
    format!("No OpenIDConnect provider found in your account for {issuer}")
}

/// The two algorithms a web identity token may be signed under: "Valid values are RS256 (RSA
/// with SHA-256) and ES384 (ECDSA using P-384 curve with SHA-384)"
/// (`smithy_query(query="doc",shape="GetWebIdentityTokenRequest",member="SigningAlgorithm")`).
const SIGNING_ALGORITHMS: [&str; 2] = ["RS256", "ES384"];

/// Turn the account's outbound web identity federation ON through IAM's own operation, and
/// report the issuer URL it announced.
async fn enable_federation(router: &MultiServiceRouter) -> String {
    let (status, body) = query(
        router,
        LONG_TERM_KEY,
        IAM_SIGNING_NAME,
        IAM_API_VERSION,
        "EnableOutboundWebIdentityFederation",
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture enable must succeed, got {status}: {body}"
    );
    member("enabling outbound federation", &body, "IssuerIdentifier")
}

/// Turn it off again through IAM's own operation.
async fn disable_federation(router: &MultiServiceRouter) {
    let (status, body) = query(
        router,
        LONG_TERM_KEY,
        IAM_SIGNING_NAME,
        IAM_API_VERSION,
        "DisableOutboundWebIdentityFederation",
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture disable must succeed, got {status}: {body}"
    );
}

/// One token this partition's own STS minted, through `GetWebIdentityToken`.
async fn minted_token(
    router: &MultiServiceRouter,
    case: &str,
    algorithm: &str,
    audiences: &[&str],
    duration: Option<i64>,
) -> String {
    let mut params = vec![("SigningAlgorithm".to_string(), algorithm.to_string())];
    for (index, audience) in audiences.iter().enumerate() {
        params.push((
            format!("Audience.member.{}", index + 1),
            (*audience).to_string(),
        ));
    }
    if let Some(duration) = duration {
        params.push(("DurationSeconds".to_string(), duration.to_string()));
    }
    let params: Vec<(&str, String)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.clone()))
        .collect();
    let (status, body) = call(router, LONG_TERM_KEY, "GetWebIdentityToken", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture mint must succeed, got {status}: {body}"
    );
    member(case, &body, "WebIdentityToken")
}

/// One `AssumeRoleWithWebIdentity` presenting `token` for `role_arn`.
async fn exchange(
    router: &MultiServiceRouter,
    token: &str,
    role_arn: &str,
) -> (u16, String) {
    let params = vec![
        ("RoleArn", role_arn.to_string()),
        ("RoleSessionName", "partition-issuer-probe".to_string()),
        ("WebIdentityToken", token.to_string()),
    ];
    call(router, LONG_TERM_KEY, ACTION, &params).await
}

/// A compact JWS of `issuer` with a non-empty signature segment and an unexpired `exp`: the
/// shape a token some OTHER issuer signed has, whoever signed it. The base64url coding is
/// `tests/all/web_identity_federation.rs`'s, which that suite owns.
fn third_party_token(issuer: &str) -> String {
    let expiry = super::sts_sessions::now_secs() + 3600;
    format!(
        "{}.{}.{}",
        segment(r#"{"alg":"RS256","typ":"JWT","kid":"key-1"}"#),
        segment(&format!(
            r#"{{"iss":"{issuer}","sub":"user-1","exp":{expiry}}}"#
        )),
        segment("a-signature-this-emulator-cannot-check")
    )
}

/// A role ARN of this account naming a role it does not hold: the token is decided before
/// the role is, so the refusal must not depend on the role existing.
fn absent_role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/never-created-for-this-suite",
        emulator_account()
    )
}

/// Assert the exchange refused `token` as a token of the partition's own issuer, and minted
/// nothing.
async fn assert_refused_as_partition_issued(
    router: &MultiServiceRouter,
    case: &str,
    token: &str,
    role_arn: &str,
) {
    let (status, body) = exchange(router, token, role_arn).await;
    assert_eq!(
        status, INVALID_IDENTITY_TOKEN_STATUS,
        "case {case:?}: expected HTTP {INVALID_IDENTITY_TOKEN_STATUS}, got {status}: {body}"
    );
    assert_eq!(
        member(case, &body, "Code"),
        INVALID_IDENTITY_TOKEN_CODE,
        "case {case:?}: expected {INVALID_IDENTITY_TOKEN_CODE}, got {body}"
    );
    assert_eq!(
        member(case, &body, "Message"),
        STS_ISSUED_TOKEN_MESSAGE,
        "case {case:?}: expected this refusal's fixed sentence, got {body}"
    );
    assert!(
        element(&body, "AccessKeyId").is_none(),
        "case {case:?}: a refused exchange must mint no credential, got {body}"
    );
    assert!(
        element(&body, "AssumedRoleId").is_none(),
        "case {case:?}: a refused exchange must mint no session, got {body}"
    );
}

/// One row of the table below: how the token was minted, and the role the exchange that
/// presents it names.
struct MintedRow {
    /// The case the row is about, named in every assertion.
    case: &'static str,
    /// `SigningAlgorithm` the token was signed under.
    algorithm: &'static str,
    /// The `Audience` members the request named.
    audiences: Vec<&'static str>,
    /// `DurationSeconds`, or `None` for the default the operation documents.
    duration: Option<i64>,
    /// The `RoleArn` the exchange names — a role the account holds, or one it does not.
    role_arn: String,
}

/// RULE: a token the partition's own web identity issuer minted is refused by the exchange
/// with one fixed sentence, whichever algorithm signed it, however many audiences it names,
/// however long it lasts and whether or not the role it names exists.
#[tokio::test]
async fn a_token_the_partition_minted_is_not_exchanged() {
    let (router, engine) = emulator_with_iam();
    let (held_role, _role_id) = seed_demo_role(&engine);
    enable_federation(&router).await;

    // One row per way of minting and presenting such a token: each algorithm, one and three
    // audiences, the default duration and both published bounds of `DurationSeconds`
    // (`webIdentityTokenDurationSecondsType`, range 60..3600), against a role the account
    // holds and a role it does not.
    let rows: Vec<MintedRow> = vec![
        MintedRow {
            case: "RS256, one audience, the default duration, a held role",
            algorithm: SIGNING_ALGORITHMS[0],
            audiences: vec!["https://example.com/webid-rejection-check"],
            duration: None,
            role_arn: held_role.clone(),
        },
        MintedRow {
            case: "ES384, three audiences, the shortest duration, a held role",
            algorithm: SIGNING_ALGORITHMS[1],
            audiences: vec![
                "https://example.com/a",
                "https://example.com/b",
                "https://example.com/c",
            ],
            duration: Some(60),
            role_arn: held_role.clone(),
        },
        MintedRow {
            case: "RS256, one audience, the longest duration, a role the account does not hold",
            algorithm: SIGNING_ALGORITHMS[0],
            audiences: vec!["https://example.com/webid-rejection-check"],
            duration: Some(3600),
            role_arn: absent_role_arn(),
        },
        MintedRow {
            case: "ES384, one audience, the default duration, a role the account does not hold",
            algorithm: SIGNING_ALGORITHMS[1],
            audiences: vec!["https://example.com/webid-rejection-check"],
            duration: None,
            role_arn: absent_role_arn(),
        },
    ];
    for row in rows {
        let token = minted_token(
            &router,
            row.case,
            row.algorithm,
            &row.audiences,
            row.duration,
        )
        .await;
        assert_refused_as_partition_issued(&router, row.case, &token, &row.role_arn).await;
    }
}

/// RULE: the class is the token's ISSUER, so it holds after the account has turned the
/// feature off again — the issuer stays the account's, and a token vended while it was on is
/// still a token of the partition's own issuer.
#[tokio::test]
async fn the_refusal_holds_after_the_feature_is_disabled() {
    let (router, engine) = emulator_with_iam();
    let (held_role, _role_id) = seed_demo_role(&engine);
    for algorithm in SIGNING_ALGORITHMS {
        let case = format!("{algorithm} across a disable");
        enable_federation(&router).await;
        let token = minted_token(
            &router,
            &case,
            algorithm,
            &["https://example.com/webid-rejection-check"],
            None,
        )
        .await;
        assert_refused_as_partition_issued(&router, &case, &token, &held_role).await;

        disable_federation(&router).await;
        assert_refused_as_partition_issued(
            &router,
            &format!("{case}, after the disable"),
            &token,
            &held_role,
        )
        .await;
    }
}

/// RULE: only the partition's OWN issuer earns that class — a token naming any other issuer
/// keeps the refusal its own class carries, so the new sentence did not replace the general
/// one.
#[tokio::test]
async fn a_token_of_another_issuer_keeps_its_own_class() {
    let (router, engine) = emulator_with_iam();
    let (held_role, _role_id) = seed_demo_role(&engine);
    let issuer = enable_federation(&router).await;

    // Rows: issuers the account can resolve no provider for — a third party, a host of this
    // emulator that is not an issuer it hosts, and the account's own issuer with its opaque
    // segment altered, which is an address of this emulator that names no issuer at all.
    let others = [
        "https://accounts.google.com".to_string(),
        format!("{issuer}/extra"),
        format!("{}z", &issuer[..issuer.len() - 1]),
    ];
    for other in others {
        let case = format!("a token of {other}");
        assert_ne!(
            other, issuer,
            "case {case:?}: this row must name an issuer other than the account's own"
        );
        let token = third_party_token(&other);
        let (status, body) = exchange(&router, &token, &held_role).await;
        assert_eq!(
            status, INVALID_IDENTITY_TOKEN_STATUS,
            "case {case:?}: expected HTTP {INVALID_IDENTITY_TOKEN_STATUS}, got {status}: {body}"
        );
        assert_eq!(
            member(&case, &body, "Message"),
            no_provider_message(&other),
            "case {case:?}: expected the unresolvable-issuer sentence, got {body}"
        );
    }
}
