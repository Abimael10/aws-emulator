// SPDX-License-Identifier: CC0-1.0

//! WHICH kind of login one `Logins` entry of this service is, and what follows from it.
//!
//! RULE: a `Logins` entry is a DEVELOPER provider's wherever this service cannot say which
//! identity PROVIDER its name is — its own provider name, one of the names AWS publishes for
//! a public provider's login, an Amazon Cognito user pool (whose name carries the pool), or a
//! provider the identity pool's own configuration declares. Every other name is a "domain" of
//! the caller's own back end, whose value is a developer user identifier rather than a token
//! (`smithy_query(query="doc",shape="GetOpenIdTokenForDeveloperIdentityInput",member="Logins")`:
//! "if the user is from a developer provider, the name-value pair will follow the syntax
//! `developer_provider_name: developer_user_identifier`"). Two things follow:
//!
//! * an identity pool is linked to ONE developer provider — "you can only specify one
//!   developer provider as part of the `Logins` map, which is linked to the identity pool"
//!   (`smithy_query(query="doc",shape="GetOpenIdTokenForDeveloperIdentity")`) — so a map
//!   naming two of them is refused the bad input parameter it is, with no identity minted and
//!   no token vended;
//! * such an entry carries no token, so it buys no credential at the exchange that redeems a
//!   login into one, whichever domain it names and whether or not it is the pool's own.
//!
//! And the identity side of the same call: an UNAUTHENTICATED identity a developer-login call
//! resolves AWAY from — because the developer user identifier it presents is already
//! registered under another identity — is ABSORBED into the identity that answers. The two
//! are one account from there on, so a read of the absorbed id projects the sentinel a join
//! leaves behind and no later call reaches it as the guest identity it was. Where BOTH sides
//! of the link are identities the pool holds instead — a presented login proving one, the
//! developer user identifier registered under another — the call asks for a join of two
//! stored identities, which only `MergeDeveloperIdentities` performs: the conflict between
//! those identities.
//!
//! The rows span what the rule generalises over: the NUMBER of developer providers a map
//! names (one, two, three), WHICH names they are (the pool's own, another domain, a dotted
//! one, a bare one), and which names are providers' instead (each published public name, a
//! user pool, a provider the pool declares, this service's own) — asserted on names the
//! issue did not cite as well.
//!
//! The harness is `response_request_id`'s, because these refusals are asserted in the
//! `x-amzn-errortype` header as well as in the body; it drives the same `MultiServiceRouter`
//! the production router builds over one shared engine.

use serde_json::{json, Value};

use crate::developer_identity_tokens::token_request;
use crate::mapped_role_resolution::identity_token;
use crate::response_request_id::{Caller, Emulator};

/// The operation the developer-provider rules are asserted at.
const MINT: &str = "GetOpenIdTokenForDeveloperIdentity";

/// The exchange that redeems a login into a credential.
const EXCHANGE: &str = "GetCredentialsForIdentity";

/// The status every refusal of this service travels with, whatever `@httpError` the shape it
/// answers is annotated with (`crate::service` owns that rule).
const REFUSED_STATUS: u16 = 400;

/// The header a failure names its exception shape in.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The pool's own `DeveloperProviderName` — the "domain" its developer logins are made under.
const DEVELOPER_PROVIDER: &str = "login.example.backend";

/// A public provider of the pool: one of the names `GetId$Logins` publishes, whose login is
/// an opaque OAuth access token.
const PUBLIC_PROVIDER: &str = "graph.facebook.com";

/// An Amazon Cognito user pool, named as `GetId$Logins` publishes one
/// (`cognito-idp.<region>.amazonaws.com/<UserPoolId>`), whose login is that pool's id token.
const USER_POOL_PROVIDER: &str = "cognito-idp.us-east-1.amazonaws.com/us-east-1_devprov01";

/// This service's own `Logins` provider name, under which a token it minted is handed back.
const COGNITO_IDENTITY_PROVIDER: &str = "cognito-identity.amazonaws.com";

/// AWS's fixed sentences, spelled as literals so the wire-visible text is pinned by the test:
/// each is fixed text, with no part taken from the request or from stored state.
const SEVERAL_DEVELOPER_PROVIDERS: &str =
    "An identity pool only supports one developer provider. The logins provided have more \
     than one developer provider.";
const NOT_A_COGNITO_TOKEN: &str = "Invalid login token. Not a Cognito token.";
const CANNOT_MERGE_IDENTITIES: &str = "Cannot merge these identities.";

/// The sentinel a read of an identity a join absorbed opens its `Logins` list with.
const DISABLED_PROJECTION: &str = "DISABLED";

/// A pool carrying the developer provider the rows present logins of, the public provider and
/// the user pool whose names are providers' rather than a back end's, and unauthenticated
/// identities allowed so a guest identity is one of the states a row can name.
async fn developer_pool(emulator: &Emulator, name: &str) -> String {
    emulator
        .ok(
            "CreateIdentityPool",
            &json!({
                "IdentityPoolName": name,
                "AllowUnauthenticatedIdentities": true,
                "DeveloperProviderName": DEVELOPER_PROVIDER,
                "CognitoIdentityProviders": [{
                    "ProviderName": USER_POOL_PROVIDER,
                    "ClientId": "devprovclient",
                    "ServerSideTokenCheck": false,
                }],
                "SupportedLoginProviders": {PUBLIC_PROVIDER: "0123456789"},
            }),
        )
        .await["IdentityPoolId"]
        .as_str()
        .expect("CreateIdentityPool answers an IdentityPoolId")
        .to_string()
}

/// The id token the fixture user pool issues for `subject`.
fn user_pool_token(subject: &str) -> String {
    identity_token(&json!({
        "iss": format!("https://{USER_POOL_PROVIDER}"),
        "sub": subject,
        "aud": "devprovclient",
        "token_use": "id",
        "exp": 4_102_444_800u64,
    }))
}

/// One answered call: its status, its parsed body and the exception shape its
/// `x-amzn-errortype` header names.
async fn answer(emulator: &Emulator, operation: &str, request: &Value) -> (u16, Value, String) {
    let response = emulator.dispatch(Caller::Signed, operation, request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get(ERROR_TYPE_HEADER)
        .map(|value| value.to_str().expect("the header is ASCII").to_string())
        .unwrap_or_default();
    let bytes = response.into_body();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body, error_type)
}

/// The exception shape the body names (`__type`, on the awsJson protocols), reduced to the
/// bare shape name.
fn body_shape(case: &str, body: &Value) -> String {
    let raw = body["__type"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: an error response names a __type: {body}"));
    let bare = raw.rsplit('#').next().unwrap_or(raw);
    bare.split(':').next().unwrap_or(bare).to_string()
}

/// The `message` an error response carries.
fn error_message(case: &str, body: &Value) -> String {
    body["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: an error response carries a message: {body}"))
        .to_string()
}

/// Assert that `operation` was refused with `shape` and the fixed sentence `message`, in the
/// body and in the header alike.
fn assert_refused(case: &str, answered: &(u16, Value, String), shape: &str, message: &str) {
    let (status, body, error_type) = answered;
    assert_eq!(
        (*status, body_shape(case, body).as_str()),
        (REFUSED_STATUS, shape),
        "case {case:?}: expected ({REFUSED_STATUS}, {shape}), got ({status}, {body})"
    );
    assert_eq!(
        error_type.as_str(),
        format!("{shape}:"),
        "case {case:?}: expected the header to name {shape}, got {error_type:?}"
    );
    assert_eq!(
        error_message(case, body),
        message,
        "case {case:?}: expected the fixed sentence {message:?}, got {body}"
    );
}

/// How many identities the pool holds.
async fn identity_count(emulator: &Emulator, pool_id: &str) -> usize {
    emulator
        .ok(
            "ListIdentities",
            &json!({"IdentityPoolId": pool_id, "MaxResults": 60}),
        )
        .await["Identities"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

/// The provider names `DescribeIdentity` reports for one identity.
async fn described_logins(emulator: &Emulator, identity_id: &str) -> Vec<String> {
    emulator
        .ok("DescribeIdentity", &json!({"IdentityId": identity_id}))
        .await["Logins"]
        .as_array()
        .map(|logins| {
            logins
                .iter()
                .map(|login| login.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: a `Logins` map naming more than ONE developer provider is refused the bad input
/// parameter it is, and a map whose other entries are identity PROVIDERS' is not — the
/// classification being the name alone, not whether the pool was created with it.
#[tokio::test]
async fn a_logins_map_naming_several_developer_providers_is_refused() {
    let emulator = Emulator::new(false);
    let pool_id = developer_pool(&emulator, "several-developer-providers").await;

    // (case, the logins presented) — every row names two or more providers this service
    // cannot say names an identity provider, so every row is refused.
    let rows: Vec<(&str, Vec<(&str, &str)>)> = vec![
        (
            "the pool's own developer provider beside another domain",
            vec![
                (DEVELOPER_PROVIDER, "backend-user-1"),
                ("login.example.other", "other-backend-user"),
            ],
        ),
        (
            "two domains, neither of them the pool's own",
            vec![
                ("login.example.first", "first-backend-user"),
                ("login.example.second", "second-backend-user"),
            ],
        ),
        (
            "a bare name beside a dotted one",
            vec![
                (DEVELOPER_PROVIDER, "backend-user-2"),
                ("my_backend", "bare-name-user"),
            ],
        ),
        (
            "three developer providers at once",
            vec![
                (DEVELOPER_PROVIDER, "backend-user-3"),
                ("login.example.third", "third-backend-user"),
                ("login.example.fourth", "fourth-backend-user"),
            ],
        ),
        (
            "two domains beside a public provider's login",
            vec![
                (DEVELOPER_PROVIDER, "backend-user-4"),
                ("login.example.fifth", "fifth-backend-user"),
                (PUBLIC_PROVIDER, "an-oauth-access-token"),
            ],
        ),
    ];

    let identities_before = identity_count(&emulator, &pool_id).await;
    for (case, logins) in &rows {
        let answered = answer(&emulator, MINT, &token_request(&pool_id, logins, None, None)).await;
        assert_refused(
            case,
            &answered,
            "InvalidParameterException",
            SEVERAL_DEVELOPER_PROVIDERS,
        );
        assert!(
            answered.1.get("Token").is_none() && answered.1.get("IdentityId").is_none(),
            "case {case:?}: a refused mint vends nothing, got {}",
            answered.1
        );
    }
    assert_eq!(
        identity_count(&emulator, &pool_id).await,
        identities_before,
        "a refused mint mints no identity, expected {identities_before} of them"
    );

    // The boundary of the rule: ONE developer provider, beside logins whose provider names
    // this service CAN place — each published public name, an Amazon Cognito user pool, a
    // provider the pool declares — is served, so the refusals above are about the count of
    // developer providers and not about the size or shape of the map.
    let user_pool_login = user_pool_token("a-user-pool-user");
    let served: Vec<(&str, Vec<(&str, &str)>)> = vec![
        (
            "the developer provider alone",
            vec![(DEVELOPER_PROVIDER, "served-user-1")],
        ),
        (
            "beside a public provider the pool declares",
            vec![
                (DEVELOPER_PROVIDER, "served-user-2"),
                (PUBLIC_PROVIDER, "another-oauth-access-token"),
            ],
        ),
        (
            "beside an Amazon Cognito user pool's id token",
            vec![
                (DEVELOPER_PROVIDER, "served-user-3"),
                (USER_POOL_PROVIDER, user_pool_login.as_str()),
            ],
        ),
        // The published names the issue did not cite, on a pool that declares none of them:
        // a name on AWS's own list is a provider's whatever the pool is configured with.
        (
            "beside Amazon's and Twitter's published provider names",
            vec![
                (DEVELOPER_PROVIDER, "served-user-4"),
                ("www.amazon.com", "an-amazon-access-token"),
                ("api.twitter.com", "a-twitter-access-token"),
            ],
        ),
        (
            "beside Google's and Apple's published provider names",
            vec![
                (DEVELOPER_PROVIDER, "served-user-5"),
                ("accounts.google.com", "a-google-id-token"),
                ("appleid.apple.com", "an-apple-id-token"),
            ],
        ),
        (
            "beside this service's own provider name",
            vec![
                (DEVELOPER_PROVIDER, "served-user-6"),
                (COGNITO_IDENTITY_PROVIDER, "a-token-this-service-minted"),
            ],
        ),
    ];
    for (case, logins) in &served {
        let (status, body, _) =
            answer(&emulator, MINT, &token_request(&pool_id, logins, None, None)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: one developer provider is served, got {status} with {body}"
        );
        assert!(
            body["IdentityId"].as_str().is_some_and(|id| !id.is_empty()),
            "case {case:?}: a served mint answers an IdentityId, got {body}"
        );
    }
}

/// RULE: a developer-provider entry carries a developer user identifier and no token, so the
/// credential exchange refuses it as the value it is — whichever domain names it, the pool's
/// own included — while an entry of a provider this service can place is not refused for
/// that reason.
#[tokio::test]
async fn the_exchange_refuses_a_developer_provider_entry_whatever_domain_names_it() {
    let emulator = Emulator::new(false);
    let pool_id = developer_pool(&emulator, "exchange-developer-entry").await;

    // A guest identity, so the exchange is reached by every row: what is being asserted is
    // the refusal the ENTRY carries, not the identity's own authorization.
    let guest = emulator
        .ok("GetId", &json!({"IdentityPoolId": &pool_id}))
        .await["IdentityId"]
        .as_str()
        .expect("GetId answers an IdentityId")
        .to_string();

    // (case, the logins presented)
    let rows: Vec<(&str, Value)> = vec![
        (
            "the pool's own developer provider",
            json!({DEVELOPER_PROVIDER: "backend-user-1"}),
        ),
        (
            "a domain the pool was never created with",
            json!({"login.example.other": "other-backend-user"}),
        ),
        (
            "a bare back-end name",
            json!({"my_backend": "bare-name-user"}),
        ),
        (
            "such a domain beside a public provider's login",
            json!({
                "login.example.other": "other-backend-user",
                PUBLIC_PROVIDER: "an-oauth-access-token",
            }),
        ),
    ];

    for (case, logins) in &rows {
        let answered = answer(
            &emulator,
            EXCHANGE,
            &json!({"IdentityId": &guest, "Logins": logins}),
        )
        .await;
        assert_refused(
            case,
            &answered,
            "NotAuthorizedException",
            NOT_A_COGNITO_TOKEN,
        );
        assert!(
            answered.1.get("Credentials").is_none(),
            "case {case:?}: a refused exchange vends nothing, got {}",
            answered.1
        );
    }

    // The boundary: an entry whose provider name this service CAN place is decided by the
    // rules of that provider instead — the pool's own public provider is admitted and the
    // vend reaches the pool's role configuration, which this pool has none of.
    let (status, body, _) = answer(
        &emulator,
        EXCHANGE,
        &json!({
            "IdentityId": &guest,
            "Logins": {PUBLIC_PROVIDER: "an-oauth-access-token"},
        }),
    )
    .await;
    assert_eq!(
        status, REFUSED_STATUS,
        "a pool with no role configuration vends nothing, got {status} with {body}"
    );
    assert_eq!(
        body_shape("a public provider's login", &body),
        "InvalidIdentityPoolConfigurationException",
        "a public provider's login is admitted and reaches the pool's roles, got {body}"
    );
}

/// RULE: the UNAUTHENTICATED identity a developer-login call resolves away from is absorbed
/// into the identity that answers, so a read of it projects the sentinel a join leaves behind
/// and no later call reaches it as the guest it was.
#[tokio::test]
async fn a_guest_identity_a_developer_login_resolves_away_from_is_absorbed() {
    let emulator = Emulator::new(false);
    let pool_id = developer_pool(&emulator, "absorbed-guest").await;

    // (case, the identifier registered, the logins the absorbing call presents beside it)
    type Row<'a> = (&'a str, &'a str, Vec<(&'a str, &'a str)>);
    let rows: Vec<Row<'_>> = vec![
        ("the developer login alone", "backend-user-1", vec![]),
        (
            "the developer login beside a public provider's",
            "backend-user-2",
            vec![(PUBLIC_PROVIDER, "an-oauth-access-token")],
        ),
    ];

    for (case, identifier, beside) in &rows {
        let registered = emulator
            .ok(
                MINT,
                &token_request(&pool_id, &[(DEVELOPER_PROVIDER, identifier)], None, None),
            )
            .await["IdentityId"]
            .as_str()
            .expect("the mint answers an IdentityId")
            .to_string();
        let guest = emulator
            .ok("GetId", &json!({"IdentityPoolId": &pool_id}))
            .await["IdentityId"]
            .as_str()
            .expect("GetId answers an IdentityId")
            .to_string();
        assert_ne!(
            guest, registered,
            "case {case:?}: the fixture needs two distinct identities"
        );

        let mut logins: Vec<(&str, &str)> = vec![(DEVELOPER_PROVIDER, identifier)];
        logins.extend(beside.iter().copied());
        let answered = emulator
            .ok(
                MINT,
                &token_request(&pool_id, &logins, Some(guest.as_str()), None),
            )
            .await;
        assert_eq!(
            answered["IdentityId"],
            json!(registered),
            "case {case:?}: the identity holding the identifier answers, got {answered}"
        );

        // The guest identity is the side the join gave up: its record is still stored and
        // still described, projecting the sentinel, and no login of it is left.
        let projected = described_logins(&emulator, &guest).await;
        assert_eq!(
            projected.first().map(String::as_str),
            Some(DISABLED_PROJECTION),
            "case {case:?}: the absorbed identity projects the join's sentinel, got \
             {projected:?}"
        );

        // So the exchange no longer serves it as an unauthenticated identity anyone holding
        // its id can reach: what names an identity a login has been linked to is a login OF
        // it, and the absorbed one has none.
        let refused = answer(&emulator, EXCHANGE, &json!({"IdentityId": &guest})).await;
        assert_eq!(
            (refused.0, body_shape(case, &refused.1).as_str()),
            (REFUSED_STATUS, "NotAuthorizedException"),
            "case {case:?}: the absorbed identity is reached by no call, got ({}, {})",
            refused.0,
            refused.1
        );
        assert_eq!(
            error_message(case, &refused.1),
            format!("Access to Identity '{guest}' is forbidden."),
            "case {case:?}: expected the forbidden-identity sentence, got {}",
            refused.1
        );
    }
}

/// RULE: where BOTH sides of the link are identities the pool already HOLDS — a presented
/// login proving one, the developer user identifier registered under another — the call asks
/// for a join of two stored identities, which only `MergeDeveloperIdentities` performs, so it
/// is refused as the conflict between those identities and writes nothing.
#[tokio::test]
async fn a_login_proving_another_stored_identity_is_the_merge_conflict() {
    let emulator = Emulator::new(false);
    let pool_id = developer_pool(&emulator, "cross-identity-conflict").await;

    // The identity the developer user identifier is registered under, and the identities two
    // kinds of identity provider's login prove: an opaque public-provider credential and an
    // Amazon Cognito user pool's id token.
    let registered_user = "the-backends-own-user";
    let registered = emulator
        .ok(
            MINT,
            &token_request(&pool_id, &[(DEVELOPER_PROVIDER, registered_user)], None, None),
        )
        .await["IdentityId"]
        .as_str()
        .expect("the mint answers an IdentityId")
        .to_string();
    let facebook_login = "a-facebook-access-token";
    let facebook_proven = emulator
        .ok(
            "GetId",
            &json!({
                "IdentityPoolId": &pool_id,
                "Logins": {PUBLIC_PROVIDER: facebook_login},
            }),
        )
        .await["IdentityId"]
        .as_str()
        .expect("GetId answers an IdentityId")
        .to_string();
    let user_pool_login = user_pool_token("the-user-pools-own-user");
    let user_pool_proven = emulator
        .ok(
            "GetId",
            &json!({
                "IdentityPoolId": &pool_id,
                "Logins": {USER_POOL_PROVIDER: &user_pool_login},
            }),
        )
        .await["IdentityId"]
        .as_str()
        .expect("GetId answers an IdentityId")
        .to_string();

    // The rows differ along which provider proves the other identity and WHICH of the two
    // stored identities the request names — either side of the join it asks for.
    type Row<'a> = (&'a str, &'a str, Vec<(&'a str, &'a str)>);
    let rows: Vec<Row<'_>> = vec![
        (
            "a public provider's login, the identity it proves named",
            facebook_proven.as_str(),
            vec![
                (PUBLIC_PROVIDER, facebook_login),
                (DEVELOPER_PROVIDER, registered_user),
            ],
        ),
        (
            "a public provider's login, the identifier's own identity named",
            registered.as_str(),
            vec![
                (PUBLIC_PROVIDER, facebook_login),
                (DEVELOPER_PROVIDER, registered_user),
            ],
        ),
        (
            "a user pool's id token, the identity it proves named",
            user_pool_proven.as_str(),
            vec![
                (USER_POOL_PROVIDER, user_pool_login.as_str()),
                (DEVELOPER_PROVIDER, registered_user),
            ],
        ),
        (
            "a user pool's id token, the identifier's own identity named",
            registered.as_str(),
            vec![
                (USER_POOL_PROVIDER, user_pool_login.as_str()),
                (DEVELOPER_PROVIDER, registered_user),
            ],
        ),
    ];

    let identities_before = identity_count(&emulator, &pool_id).await;
    for (case, identity_id, logins) in &rows {
        let answered = answer(
            &emulator,
            MINT,
            &token_request(&pool_id, logins, Some(identity_id), None),
        )
        .await;
        assert_refused(
            case,
            &answered,
            "ResourceConflictException",
            CANNOT_MERGE_IDENTITIES,
        );
        // The sentence is fixed text, so it reports no identity of the pool.
        for named in [&registered, &facebook_proven, &user_pool_proven] {
            assert!(
                !error_message(case, &answered.1).contains(named.as_str()),
                "case {case:?}: the refusal must name no identity, got {}",
                answered.1
            );
        }
    }

    // A refused call wrote nothing: the pool holds the identities it held, and the registered
    // identifier still resolves to the identity it was registered under.
    assert_eq!(
        identity_count(&emulator, &pool_id).await,
        identities_before,
        "a refused call mints no identity, expected {identities_before} of them"
    );
    let looked_up = emulator
        .ok(
            "LookupDeveloperIdentity",
            &json!({"IdentityPoolId": &pool_id, "DeveloperUserIdentifier": registered_user}),
        )
        .await;
    assert_eq!(
        looked_up["IdentityId"],
        json!(registered),
        "the identifier still resolves to the identity it was registered under, got {looked_up}"
    );
}
