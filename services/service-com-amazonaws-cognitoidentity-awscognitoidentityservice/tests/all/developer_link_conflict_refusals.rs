// SPDX-License-Identifier: CC0-1.0

//! How `GetOpenIdTokenForDeveloperIdentity` refuses an implicit link it cannot make.
//!
//! RULE: a call whose presented logins cannot all name ONE account is refused, and WHICH
//! refusal it is follows what the call has PROVEN and how many of the link's two sides the
//! pool already holds. A call that presents a login OF the identity it names has shown the
//! authority this operation asks over that identity, so where it also presents a developer
//! user identifier the pool has registered elsewhere, both sides of the link are identities
//! the pool holds and what it asks for is a MERGE of the two — the modelled
//! `ResourceConflictException` carrying "Cannot merge these identities.", the join of two
//! stored identities being what `MergeDeveloperIdentities` performs. A request that names an
//! `IdentityId`, presents NO login of it AND presents a developer user identifier the pool
//! has already registered elsewhere has shown no login of the identity it named, so it is
//! the failure of the caller's authority over THAT identity: the modelled
//! `NotAuthorizedException` carrying the fixed logins-mismatch sentence ("Logins don't
//! match. Please include at least one valid login for this identity or identity pool.").
//! Where one of the link's two sides is not identified — a developer user identifier the pool
//! has registered against nothing, or no `IdentityId` at all — there are no two stored
//! identities to merge and the fact is about the POOL's state: the modelled
//! `ResourceConflictException`, "Thrown when a user tries to use a login which is already
//! linked to another account" (`smithy_query(query="doc",shape="ResourceConflictException")`),
//! named in the body's `__type` and in the `x-amzn-errortype` header, carrying "Cannot link
//! these logins." — which is also what a call asking to join a developer user into an account
//! an identity PROVIDER's login already proves carries, whatever login of that account it
//! presents. No sentence has a request- or state-derived part, so none names an
//! identity, a provider or a login, and a refused call writes nothing.
//!
//! The rows differ along what the rule generalises over: whether the request names an
//! `IdentityId` at all, what state the identity it names is in (a developer identity of the
//! same backend, the identifier's OWN identity, an identity a user-pool id token proves, an
//! identity a public provider's access token proves, an identity an unlink left linked to
//! nothing, an unauthenticated guest), which logins the call presents beside the developer
//! one and which identity each is bound to, and whether the developer user identifier is new
//! or already registered.
//!
//! The harness is `response_request_id`'s, because this rule is about a response HEADER as
//! well as a response body; it drives the same `MultiServiceRouter` the production router
//! builds over one shared engine.

use serde_json::{json, Value};

use crate::developer_identity_tokens::token_request;
use crate::mapped_role_resolution::identity_token;
use crate::response_request_id::{Caller, Emulator};

/// The operation under test, spelled once.
const OPERATION: &str = "GetOpenIdTokenForDeveloperIdentity";

/// The status every refusal of this service travels with, whatever `@httpError` the shape it
/// answers is annotated with (`crate::service` owns that rule).
const REFUSED_STATUS: u16 = 400;

/// The exception shapes the refusals below name.
const CONFLICT: &str = "ResourceConflictException";
const NOT_AUTHORIZED: &str = "NotAuthorizedException";

/// The header a failure names its exception shape in.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The pool's `DeveloperProviderName` — the "domain" its developer logins are made under.
const DEVELOPER_PROVIDER: &str = "login.example.conflicts";

/// The `Logins` provider name of the Amazon Cognito user pool the fixture pool trusts: a user
/// pool is named `cognito-idp.<region>.amazonaws.com/<UserPoolId>`
/// (`smithy_query(query="doc",shape="GetIdInput",member="Logins")`), and its login is that
/// user pool's id token.
const USER_POOL_PROVIDER: &str = "cognito-idp.us-east-1.amazonaws.com/us-east-1_conflict1";

/// The app client the fixture user pool's id tokens are issued to.
const CLIENT_ID: &str = "conflictclient";

/// A public provider of the fixture pool: one of the names `GetId$Logins` publishes, whose
/// login is an opaque OAuth access token rather than an identity token.
const PUBLIC_PROVIDER: &str = "graph.facebook.com";

/// This service's OWN `Logins` provider name: the name a token `GetOpenIdToken` minted is
/// presented back under (`smithy_query(query="doc",shape="GetIdInput",member="Logins")`).
const COGNITO_IDENTITY_PROVIDER: &str = "cognito-identity.amazonaws.com";

/// The two fixed sentences AWS words these refusals with, spelled out here so the
/// wire-visible text is pinned by the test rather than read back out of the crate.
const UNMATCHED_LOGINS: &str = "Logins don't match. Please include at least one valid login \
                                for this identity or identity pool.";
const CANNOT_LINK_LOGINS: &str = "Cannot link these logins.";
const CANNOT_MERGE_IDENTITIES: &str = "Cannot merge these identities.";

/// A pool carrying a `DeveloperProviderName` AND the identity providers whose logins the rows
/// present beside the developer one, with unauthenticated identities allowed so a guest
/// identity is one of the states a row can name.
async fn conflict_pool(emulator: &Emulator, name: &str) -> String {
    emulator
        .ok(
            "CreateIdentityPool",
            &json!({
                "IdentityPoolName": name,
                "AllowUnauthenticatedIdentities": true,
                "DeveloperProviderName": DEVELOPER_PROVIDER,
                "CognitoIdentityProviders": [{
                    "ProviderName": USER_POOL_PROVIDER,
                    "ClientId": CLIENT_ID,
                    "ServerSideTokenCheck": false,
                }],
                "SupportedLoginProviders": {PUBLIC_PROVIDER: "0123456789"},
            }),
        )
        .await["IdentityPoolId"]
        .as_str()
        .expect("CreateIdentityPool returns an IdentityPoolId")
        .to_string()
}

/// The id token the fixture user pool issues for `subject`.
fn user_pool_token(subject: &str) -> String {
    identity_token(&json!({
        "iss": format!("https://{USER_POOL_PROVIDER}"),
        "sub": subject,
        "aud": CLIENT_ID,
        "token_use": "id",
        "exp": 4_102_444_800u64,
    }))
}

/// The identity `GetId` mints (or resolves) for the logins given, which is how an identity a
/// provider's login proves — and a guest identity, for `None` — comes to exist.
async fn federated_identity(emulator: &Emulator, pool_id: &str, logins: Option<Value>) -> String {
    let mut request = json!({"IdentityPoolId": pool_id});
    if let Some(logins) = logins {
        request["Logins"] = logins;
    }
    emulator.ok("GetId", &request).await["IdentityId"]
        .as_str()
        .expect("GetId returns an IdentityId")
        .to_string()
}

/// The identity the pool registers `identifier` under, through the operation under test.
async fn registered_identity(emulator: &Emulator, pool_id: &str, identifier: &str) -> String {
    emulator
        .ok(
            OPERATION,
            &token_request(pool_id, &[(DEVELOPER_PROVIDER, identifier)], None, None),
        )
        .await["IdentityId"]
        .as_str()
        .expect("a served exchange answers an IdentityId")
        .to_string()
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

/// The identity the pool resolves a developer user identifier to.
async fn resolved_identity(emulator: &Emulator, pool_id: &str, identifier: &str) -> String {
    emulator
        .ok(
            "LookupDeveloperIdentity",
            &json!({"IdentityPoolId": pool_id, "DeveloperUserIdentifier": identifier}),
        )
        .await["IdentityId"]
        .as_str()
        .expect("LookupDeveloperIdentity answers an IdentityId")
        .to_string()
}

/// One answered call of the operation under test: its status, its parsed body and the
/// exception shape its `x-amzn-errortype` header names.
async fn answer(
    emulator: &Emulator,
    request: &Value,
) -> (u16, Value, Option<String>) {
    let response = emulator.dispatch(Caller::Signed, OPERATION, request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get(ERROR_TYPE_HEADER)
        .map(|value| {
            value
                .to_str()
                .expect("the error-type header is ASCII")
                .to_string()
        });
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

/// RULE: every link this operation cannot make is refused in the body and in the header
/// alike — the merge sentence where the call has proven the identity it names and the pool
/// holds the link's other side too, the logins-mismatch refusal where it has proven nothing
/// of the identity it named, and the link sentence where one side of the link does not exist
/// yet — and a refused call writes nothing.
#[tokio::test]
async fn a_link_that_cannot_be_made_is_refused_as_the_conflict_it_is() {
    let emulator = Emulator::new(false);
    let pool_id = conflict_pool(&emulator, "developer-link-conflicts").await;

    // The developer user identifiers the pool has registered, and the identities they are
    // registered under.
    let registered_user = "the-owners-backend-user";
    let registered = registered_identity(&emulator, &pool_id, registered_user).await;
    let other_registered =
        registered_identity(&emulator, &pool_id, "another-backend-user").await;

    // An account the pool holds BOTH kinds of login for: the joined account one call makes of
    // a public-provider login the pool had bound to no identity and a developer user
    // identifier ("supplying multiple logins will create an implicit linked account"), so an
    // identity provider's login proves it as well as the backend's own user.
    let joined_user = "the-joined-accounts-backend-user";
    let joined_users_token = user_pool_token("a-joined-account-user-pool-user");
    let joined_account = emulator
        .ok(
            OPERATION,
            &token_request(
                &pool_id,
                &[
                    (USER_POOL_PROVIDER, joined_users_token.as_str()),
                    (DEVELOPER_PROVIDER, joined_user),
                ],
                None,
                None,
            ),
        )
        .await["IdentityId"]
        .as_str()
        .expect("a served exchange answers an IdentityId")
        .to_string();

    // The identities a provider's login proves, a guest identity, and one an unlink left
    // reachable through no login at all: the states the identity a request names can be in.
    let user_pool_login = user_pool_token("a-user-pool-user");
    let idp_proven = federated_identity(
        &emulator,
        &pool_id,
        Some(json!({USER_POOL_PROVIDER: &user_pool_login})),
    )
    .await;
    let facebook_login = "a-facebook-access-token";
    let facebook_proven = federated_identity(
        &emulator,
        &pool_id,
        Some(json!({PUBLIC_PROVIDER: facebook_login})),
    )
    .await;
    let guest = federated_identity(&emulator, &pool_id, None).await;
    let unlinked_login = "a-second-facebook-access-token";
    let unlinked = federated_identity(
        &emulator,
        &pool_id,
        Some(json!({PUBLIC_PROVIDER: unlinked_login})),
    )
    .await;
    emulator
        .ok(
            "UnlinkIdentity",
            &json!({
                "IdentityId": &unlinked,
                "Logins": {PUBLIC_PROVIDER: unlinked_login},
                "LoginsToRemove": [PUBLIC_PROVIDER],
            }),
        )
        .await;

    // (case, the IdentityId sent, the logins presented, the shape owed, the sentence owed)
    type Row<'a> = (
        &'a str,
        Option<&'a str>,
        Vec<(&'a str, &'a str)>,
        &'a str,
        &'a str,
    );
    let rows: Vec<Row<'_>> = vec![
        // The request names an identity, presents an identifier the pool has already
        // registered elsewhere and shows no login of the identity it named: it has proven
        // nothing about that identity.
        (
            "an identity a user-pool id token proves, with an identifier registered \
             elsewhere",
            Some(idp_proven.as_str()),
            vec![(DEVELOPER_PROVIDER, registered_user)],
            NOT_AUTHORIZED,
            UNMATCHED_LOGINS,
        ),
        // Both sides identified, the identity named proven by a login the call presents, and
        // the identifier registered under an account an identity provider's login proves too:
        // two federated accounts, which only a merge could join.
        (
            "that identity, with its own id token beside an identifier registered under \
             another federated account",
            Some(idp_proven.as_str()),
            vec![
                (USER_POOL_PROVIDER, user_pool_login.as_str()),
                (DEVELOPER_PROVIDER, joined_user),
            ],
            CONFLICT,
            CANNOT_MERGE_IDENTITIES,
        ),
        (
            "the identifier's OWN identity, beside a login bound to another identity",
            Some(registered.as_str()),
            vec![
                (USER_POOL_PROVIDER, user_pool_login.as_str()),
                (DEVELOPER_PROVIDER, registered_user),
            ],
            CONFLICT,
            CANNOT_MERGE_IDENTITIES,
        ),
        (
            "an identity a public provider's access token proves, with its own token beside \
             an identifier registered under another federated account",
            Some(facebook_proven.as_str()),
            vec![
                (PUBLIC_PROVIDER, facebook_login),
                (DEVELOPER_PROVIDER, joined_user),
            ],
            CONFLICT,
            CANNOT_MERGE_IDENTITIES,
        ),
        (
            "another developer identity of the same backend",
            Some(other_registered.as_str()),
            vec![(DEVELOPER_PROVIDER, registered_user)],
            NOT_AUTHORIZED,
            UNMATCHED_LOGINS,
        ),
        // Again a call that has proven nothing of the identity it names, over the other
        // states that identity can be in.
        (
            "an identity an unlink left linked to nothing",
            Some(unlinked.as_str()),
            vec![(DEVELOPER_PROVIDER, registered_user)],
            NOT_AUTHORIZED,
            UNMATCHED_LOGINS,
        ),
        // One side of the link not identified by the request: no `IdentityId` at all, or a
        // developer user identifier the pool has registered against nothing, so the pool
        // does not hold the two identities a merge would join.
        (
            "no identity named, a user-pool login proving one identity beside a new \
             identifier",
            None,
            vec![
                (USER_POOL_PROVIDER, user_pool_login.as_str()),
                (DEVELOPER_PROVIDER, "a-brand-new-backend-user"),
            ],
            CONFLICT,
            CANNOT_LINK_LOGINS,
        ),
        (
            "no identity named, a user-pool login proving one identity beside an identifier \
             registered under another",
            None,
            vec![
                (USER_POOL_PROVIDER, user_pool_login.as_str()),
                (DEVELOPER_PROVIDER, registered_user),
            ],
            CONFLICT,
            CANNOT_LINK_LOGINS,
        ),
        (
            "a guest identity, with a public login bound to another identity",
            Some(guest.as_str()),
            vec![
                (PUBLIC_PROVIDER, facebook_login),
                (DEVELOPER_PROVIDER, "a-second-brand-new-backend-user"),
            ],
            CONFLICT,
            CANNOT_LINK_LOGINS,
        ),
        (
            "an identity a user-pool id token proves, with a new identifier and none of its \
             logins",
            Some(idp_proven.as_str()),
            vec![(DEVELOPER_PROVIDER, "a-third-brand-new-backend-user")],
            CONFLICT,
            CANNOT_LINK_LOGINS,
        ),
        // The joined account of a provider's user and one of the backend's, with its own
        // login presented: one call registers ONE of the backend's users, and that identity
        // already stands for a provider's user and one of them, so another of the backend's
        // users comes onto it only by a join of two identities — what cannot be done here is
        // the link.
        (
            "the joined account of a provider's user and one of the backend's, with its own \
             id token beside a new identifier",
            Some(joined_account.as_str()),
            vec![
                (USER_POOL_PROVIDER, joined_users_token.as_str()),
                (DEVELOPER_PROVIDER, "a-fourth-brand-new-backend-user"),
            ],
            CONFLICT,
            CANNOT_LINK_LOGINS,
        ),
    ];

    // What the pool held before the refusals, so they can be shown to have written nothing.
    let watched = [
        ("the identity the identifier is registered under", registered.as_str()),
        ("the other developer identity", other_registered.as_str()),
        ("the joined account", joined_account.as_str()),
        ("the identity a user-pool id token proves", idp_proven.as_str()),
        ("the identity a public token proves", facebook_proven.as_str()),
        ("the guest identity", guest.as_str()),
        ("the identity an unlink left linked to nothing", unlinked.as_str()),
    ];
    let mut before: Vec<(&str, &str, Vec<String>)> = Vec::new();
    for (case, identity_id) in watched {
        before.push((case, identity_id, described_logins(&emulator, identity_id).await));
    }
    let identities_before = identity_count(&emulator, &pool_id).await;

    for (case, identity_id, presented, shape, sentence) in &rows {
        let (status, body, error_type) = answer(
            &emulator,
            &token_request(&pool_id, presented, *identity_id, None),
        )
        .await;
        assert_eq!(
            (status, body_shape(case, &body).as_str()),
            (REFUSED_STATUS, *shape),
            "case {case:?}: expected ({REFUSED_STATUS}, {shape}), got ({status}, {body})"
        );
        assert_eq!(
            error_type.as_deref(),
            Some(format!("{shape}:").as_str()),
            "case {case:?}: expected the header to name {shape}, got {error_type:?}"
        );
        assert_eq!(
            error_message(case, &body),
            *sentence,
            "case {case:?}: expected the fixed sentence {sentence:?}, got {body}"
        );
        // The sentences are fixed text: a refusal reports no identity of the pool, not even
        // one the caller named itself.
        let message = error_message(case, &body);
        for (watched_case, watched_identity, _) in &before {
            assert!(
                !message.contains(watched_identity),
                "case {case:?}: the refusal must name no identity, got {watched_case:?} in \
                 {body}"
            );
        }
    }

    // A refused call wrote nothing: every identity carries the logins it carried, the pool
    // holds the identities it held, and the registered identifier still resolves to its own.
    for (case, identity_id, logins) in &before {
        assert_eq!(
            &described_logins(&emulator, identity_id).await,
            logins,
            "case {case:?}: a refused call linked nothing, expected {logins:?}"
        );
    }
    assert_eq!(
        identity_count(&emulator, &pool_id).await,
        identities_before,
        "a refused call mints no identity, expected {identities_before} of them"
    );
    assert_eq!(
        resolved_identity(&emulator, &pool_id, registered_user).await,
        registered,
        "the registered identifier still resolves to the identity it was registered under"
    );

    // And the boundary of the rule: a public-provider login the pool has bound to NO identity
    // is linked with the developer one into the single account the call registers that
    // developer user in — "supplying multiple logins will create an implicit linked account" —
    // so the conflict is decided by what the pool HOLDS and not by the shape of the request.
    let unbound_login = user_pool_token("a-user-pool-user-the-pool-has-not-seen");
    let served = emulator
        .ok(
            OPERATION,
            &token_request(
                &pool_id,
                &[
                    (USER_POOL_PROVIDER, unbound_login.as_str()),
                    (DEVELOPER_PROVIDER, "the-implicitly-linked-backend-id"),
                ],
                None,
                None,
            ),
        )
        .await;
    let linked_account = served["IdentityId"]
        .as_str()
        .expect("a served exchange answers an IdentityId")
        .to_string();
    let mut linked = described_logins(&emulator, &linked_account).await;
    linked.sort();
    let mut owed_providers = vec![
        USER_POOL_PROVIDER.to_string(),
        DEVELOPER_PROVIDER.to_string(),
    ];
    owed_providers.sort();
    assert_eq!(
        linked, owed_providers,
        "the implicit linked account carries both logins the served call linked onto it, got \
         {linked:?}"
    );
    assert_eq!(
        resolved_identity(&emulator, &pool_id, "the-implicitly-linked-backend-id").await,
        linked_account,
        "the new identifier resolves to the account the implicit link created"
    );
}

/// A token this service minted for one identity, which is a login OF that identity presented
/// back under this service's own provider name.
async fn minted_token(emulator: &Emulator, identity_id: &str) -> String {
    emulator
        .ok("GetOpenIdToken", &json!({"IdentityId": identity_id}))
        .await["Token"]
        .as_str()
        .expect("GetOpenIdToken returns a Token")
        .to_string()
}

/// RULE, over WHICH login carries the proof and how many logins are bound elsewhere: the
/// merge sentence is owed wherever the call has proven the identity it names and the pool
/// holds the link's other side (a developer user identifier it has registered), whatever kind
/// of login proves it and however many presented logins are bound to other identities; where
/// that other side does not exist — a brand-new developer user identifier — the same call
/// carries the link sentence instead.
#[tokio::test]
async fn the_proof_a_call_presents_decides_the_sentence_its_refusal_carries() {
    let emulator = Emulator::new(false);
    let pool_id = conflict_pool(&emulator, "developer-link-conflict-proofs").await;

    // The developer user identifier the pool has registered, and the identities the rows name:
    // one a user pool's id token proves, one a public provider's access token proves, and a
    // guest identity holding a token this service minted for it.
    let registered_user = "the-registered-backend-user";
    let registered = registered_identity(&emulator, &pool_id, registered_user).await;
    let user_pool_login = user_pool_token("the-proof-user-pool-user");
    let user_pool_proven = federated_identity(
        &emulator,
        &pool_id,
        Some(json!({USER_POOL_PROVIDER: &user_pool_login})),
    )
    .await;
    let facebook_login = "the-proof-facebook-access-token";
    let facebook_proven = federated_identity(
        &emulator,
        &pool_id,
        Some(json!({PUBLIC_PROVIDER: facebook_login})),
    )
    .await;
    let guest = federated_identity(&emulator, &pool_id, None).await;
    let guest_token = minted_token(&emulator, &guest).await;
    // A developer user identifier registered under an account an identity provider's login
    // proves too — the joined account one call makes of an unbound public-provider login and
    // a developer user identifier — which is the link's other side where the join the call
    // asks for is of two federated accounts.
    let joined_user = "the-proof-joined-accounts-backend-user";
    let joined_users_token = user_pool_token("the-proof-joined-account-user-pool-user");
    emulator
        .ok(
            OPERATION,
            &token_request(
                &pool_id,
                &[
                    (USER_POOL_PROVIDER, joined_users_token.as_str()),
                    (DEVELOPER_PROVIDER, joined_user),
                ],
                None,
                None,
            ),
        )
        .await;

    // (case, the IdentityId sent, the logins presented, the shape owed, the sentence owed)
    type ProofRow<'a> = (
        &'a str,
        &'a str,
        Vec<(&'a str, &'a str)>,
        &'a str,
        &'a str,
    );
    let rows: Vec<ProofRow<'_>> = vec![
        (
            "the proof is a token this service minted for a guest identity",
            guest.as_str(),
            vec![
                (COGNITO_IDENTITY_PROVIDER, guest_token.as_str()),
                (DEVELOPER_PROVIDER, registered_user),
            ],
            CONFLICT,
            CANNOT_MERGE_IDENTITIES,
        ),
        (
            "the proof is a public provider's access token, with an identifier registered \
             under another federated account",
            facebook_proven.as_str(),
            vec![
                (PUBLIC_PROVIDER, facebook_login),
                (DEVELOPER_PROVIDER, joined_user),
            ],
            CONFLICT,
            CANNOT_MERGE_IDENTITIES,
        ),
        (
            "two presented logins are bound to other identities at once",
            user_pool_proven.as_str(),
            vec![
                (USER_POOL_PROVIDER, user_pool_login.as_str()),
                (PUBLIC_PROVIDER, facebook_login),
                (DEVELOPER_PROVIDER, registered_user),
            ],
            CONFLICT,
            CANNOT_MERGE_IDENTITIES,
        ),
        (
            "the developer identifier is the identity's own, and another login is bound \
             elsewhere",
            registered.as_str(),
            vec![
                (PUBLIC_PROVIDER, facebook_login),
                (DEVELOPER_PROVIDER, registered_user),
            ],
            CONFLICT,
            CANNOT_MERGE_IDENTITIES,
        ),
        // The boundary of the merge sentence: with a BRAND-NEW identifier the pool holds no
        // second identity to merge, so the same proof and the same conflicting login carry
        // the link sentence.
        (
            "a proven identity, a brand-new identifier, and a login bound elsewhere",
            user_pool_proven.as_str(),
            vec![
                (USER_POOL_PROVIDER, user_pool_login.as_str()),
                (PUBLIC_PROVIDER, facebook_login),
                (DEVELOPER_PROVIDER, "a-brand-new-proof-backend-user"),
            ],
            CONFLICT,
            CANNOT_LINK_LOGINS,
        ),
        // And the boundary of the proof: the same request without a login of the identity it
        // names has shown no authority over it.
        (
            "the identity named with none of its logins, and the registered identifier",
            user_pool_proven.as_str(),
            vec![(DEVELOPER_PROVIDER, registered_user)],
            NOT_AUTHORIZED,
            UNMATCHED_LOGINS,
        ),
    ];

    for (case, identity_id, presented, shape, sentence) in &rows {
        let (status, body, error_type) = answer(
            &emulator,
            &token_request(&pool_id, presented, Some(identity_id), None),
        )
        .await;
        assert_eq!(
            (status, body_shape(case, &body).as_str()),
            (REFUSED_STATUS, *shape),
            "case {case:?}: expected ({REFUSED_STATUS}, {shape}), got ({status}, {body})"
        );
        assert_eq!(
            error_type.as_deref(),
            Some(format!("{shape}:").as_str()),
            "case {case:?}: expected the header to name {shape}, got {error_type:?}"
        );
        let message = error_message(case, &body);
        assert_eq!(
            message, *sentence,
            "case {case:?}: expected the fixed sentence {sentence:?}, got {body}"
        );
        // Fixed text: no identity, provider name or login value of the request is in it.
        for named in [
            registered.as_str(),
            user_pool_proven.as_str(),
            facebook_proven.as_str(),
            guest.as_str(),
            DEVELOPER_PROVIDER,
            USER_POOL_PROVIDER,
            PUBLIC_PROVIDER,
            registered_user,
            facebook_login,
        ] {
            assert!(
                !message.contains(named),
                "case {case:?}: the refusal must not name {named:?}, got {body}"
            );
        }
    }

    // None of those refusals wrote anything, and the served boundary still links: the guest
    // identity takes a developer login of its own, read back through the two readers of that
    // link, and no identity is minted for it.
    let identities_before = identity_count(&emulator, &pool_id).await;
    for (case, identity_id, owed) in [
        ("the identity the identifier is registered under", registered.as_str(), 1),
        (
            "the identity a user-pool id token proves",
            user_pool_proven.as_str(),
            1,
        ),
        ("the guest identity", guest.as_str(), 0),
    ] {
        assert_eq!(
            described_logins(&emulator, identity_id).await.len(),
            owed,
            "case {case:?}: a refused call linked no provider onto it"
        );
    }
    let served = emulator
        .ok(
            OPERATION,
            &token_request(
                &pool_id,
                &[
                    (COGNITO_IDENTITY_PROVIDER, guest_token.as_str()),
                    (DEVELOPER_PROVIDER, "the-guests-backend-id"),
                ],
                Some(guest.as_str()),
                None,
            ),
        )
        .await;
    assert_eq!(
        served["IdentityId"],
        json!(guest),
        "a call presenting a token of the identity it names links into it, got {served}"
    );
    let mut linked = described_logins(&emulator, &guest).await;
    linked.sort();
    let mut owed_providers = vec![
        COGNITO_IDENTITY_PROVIDER.to_string(),
        DEVELOPER_PROVIDER.to_string(),
    ];
    owed_providers.sort();
    assert_eq!(
        linked, owed_providers,
        "the guest identity carries both logins the served call linked onto it"
    );
    assert_eq!(
        resolved_identity(&emulator, &pool_id, "the-guests-backend-id").await,
        guest,
        "the new identifier resolves to the identity it was registered against"
    );
    assert_eq!(
        identity_count(&emulator, &pool_id).await,
        identities_before,
        "the served link mints no identity, expected {identities_before} of them"
    );
}
