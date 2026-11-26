// SPDX-License-Identifier: CC0-1.0

//! WHICH identity `GetOpenIdTokenForDeveloperIdentity` may register one of the backend's users
//! in, and what it does with a user the pool has already registered elsewhere.
//!
//! RULE: one call registers ONE of the backend's users, so the identity it registers a
//! developer user identifier in is one that can still become that user's — a new identity of
//! the pool, an unauthenticated one, an identity only the backend's own users reach, or one
//! only an identity provider's login proves, which a public-provider login presented beside
//! the developer one is linked into ("Supplying multiple logins will create an implicit linked
//! account",
//! `smithy_query(query="operation",operation="GetOpenIdTokenForDeveloperIdentity")`). An
//! identity that ALREADY stands for a provider's user AND one of the backend's users is the
//! joined account that implicit link made of the two, and another of the backend's users comes
//! onto it only by a JOIN of the two identities their registrations stand for, so a developer
//! user identifier the pool has registered against nothing is refused for it — the modelled
//! `ResourceConflictException` ("Thrown when a user tries to use a login which is already
//! linked to another account", `smithy_query(query="doc",shape="ResourceConflictException")`)
//! carrying "Cannot link these logins.". That JOIN is what a call presenting an identifier the
//! pool has ALREADY registered elsewhere asks for, and this operation performs it where that
//! registration stands for an account the BACKEND's own logins alone reach and the call has
//! PROVEN the identity it names: the named identity survives, answering its own id and
//! carrying the developer provider once per user it now stands for, and the joined-away
//! identity is left reached by no login at all. Where that other registration is an account an
//! identity PROVIDER's login also proves, the join is of two federated accounts, which only
//! `MergeDeveloperIdentities` performs: `ResourceConflictException` carrying "Cannot merge
//! these identities." Every sentence is fixed text, so none names an identity, a provider or a
//! login, and a refused call writes nothing.
//!
//! The rows differ along what the rule generalises over: what the identity the request names
//! holds (the joined account of a provider's user and one of the backend's, an identity only a
//! user pool's id token proves, one only a public provider's access token proves, one only the
//! backend's users reach, an unauthenticated guest), whether the presented developer user
//! identifier is new or already registered, what the identity it is registered under holds,
//! and which kind of login carries the proof of the identity named.
//!
//! The harness is `identity_pool_flow`'s: one shared engine behind the same
//! `MultiServiceRouter` the production router builds.

use serde_json::{json, Value};

use crate::developer_identity_tokens::token_request;
use crate::identity_pool_flow::{error_code, Emulator};
use crate::mapped_role_resolution::identity_token;

/// The operation under test, spelled once.
const OPERATION: &str = "GetOpenIdTokenForDeveloperIdentity";

/// The status every refusal of this service travels with, whatever `@httpError` the shape it
/// answers is annotated with (`crate::service` owns that rule).
const REFUSED_STATUS: u16 = 400;

/// The exception shape both refusals name.
const CONFLICT: &str = "ResourceConflictException";

/// The pool's `DeveloperProviderName` — the "domain" its developer logins are made under.
const DEVELOPER_PROVIDER: &str = "login.example.registrations";

/// The `Logins` provider name of the Amazon Cognito user pool the fixture pool trusts: a user
/// pool is named `cognito-idp.<region>.amazonaws.com/<UserPoolId>`
/// (`smithy_query(query="doc",shape="GetIdInput",member="Logins")`), and its login is that
/// user pool's id token.
const USER_POOL_PROVIDER: &str = "cognito-idp.us-east-1.amazonaws.com/us-east-1_register1";

/// The app client the fixture user pool's id tokens are issued to.
const CLIENT_ID: &str = "registerclient";

/// A public provider of the fixture pool: one of the names `GetId$Logins` publishes, whose
/// login is an opaque OAuth access token rather than an identity token.
const PUBLIC_PROVIDER: &str = "graph.facebook.com";

/// The two fixed sentences AWS words these refusals with, spelled out here so the wire-visible
/// text is pinned by the test rather than read back out of the crate.
const CANNOT_LINK_LOGINS: &str = "Cannot link these logins.";
const CANNOT_MERGE_IDENTITIES: &str = "Cannot merge these identities.";

/// The sentinel a read of an identity a join absorbed opens its `Logins` list with.
const DISABLED_PROJECTION: &str = "DISABLED";

/// A pool carrying a `DeveloperProviderName` AND the identity providers whose logins the rows
/// present beside the developer one, with unauthenticated identities allowed so a guest
/// identity is one of the states a row can name.
async fn registration_pool(emulator: &Emulator, name: &str) -> String {
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
        .expect("CreateIdentityPool answers an IdentityPoolId")
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

/// One `Logins` entry as a row carries it: the provider name and the login (for the developer
/// provider, the developer user identifier).
type Login<'a> = (&'a str, &'a str);

/// The `IdentityId` a served exchange answers, with its `Token` asserted non-empty.
fn served_identity(case: &str, body: &Value) -> String {
    let token = body["Token"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a served exchange answers a Token: {body}"));
    assert!(
        !token.is_empty(),
        "case {case:?}: the answered Token is not empty, got {body}"
    );
    body["IdentityId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a served exchange answers an IdentityId: {body}"))
        .to_string()
}

/// The `message` an error response carries.
fn error_message(case: &str, body: &Value) -> String {
    body["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: an error response carries a message: {body}"))
        .to_string()
}

/// The identity this operation registers `identifier` in, presenting the logins given.
async fn registered(
    emulator: &Emulator,
    case: &str,
    pool_id: &str,
    logins: &[Login<'_>],
    identity_id: Option<&str>,
) -> String {
    served_identity(
        case,
        &emulator
            .ok(OPERATION, &token_request(pool_id, logins, identity_id, None))
            .await,
    )
}

/// The provider names `DescribeIdentity` reports for one identity, in the order it reports
/// them — a provider the identity carries once per user it stands for is reported once per
/// user, so the LIST and not the set is what a row compares.
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

/// RULE: a developer user identifier the pool has registered against nothing is refused for an
/// identity that already stands for a provider's user AND one of the backend's users, and so is
/// the join of two accounts an identity provider's login proves — each as the conflict it is,
/// in the body and in the `x-amzn-errortype` header the shape travels in, and a refused call
/// writes nothing.
#[tokio::test]
async fn a_registration_an_identity_cannot_take_is_refused_as_the_conflict_it_is() {
    let emulator = Emulator::new();
    let pool_id = registration_pool(&emulator, "developer-registration-refusals").await;

    // The joined account of a provider's user and one of the backend's: what one call makes of
    // a public-provider login the pool had bound to no identity and a developer user
    // identifier.
    let joined_user = "the-joined-accounts-backend-user";
    let joined_token = user_pool_token("the-joined-accounts-user-pool-user");
    let joined = registered(
        &emulator,
        "the implicit link that makes the joined account",
        &pool_id,
        &[
            (USER_POOL_PROVIDER, joined_token.as_str()),
            (DEVELOPER_PROVIDER, joined_user),
        ],
        None,
    )
    .await;
    // A second one of those, so a row can ask for the join of two of them.
    let other_joined_user = "the-other-joined-accounts-backend-user";
    let other_joined_token = user_pool_token("the-other-joined-accounts-user-pool-user");
    let other_joined = registered(
        &emulator,
        "the implicit link that makes the other joined account",
        &pool_id,
        &[
            (USER_POOL_PROVIDER, other_joined_token.as_str()),
            (DEVELOPER_PROVIDER, other_joined_user),
        ],
        None,
    )
    .await;

    // Identities only an identity provider's login proves, of the two kinds of provider the
    // pool is configured with.
    let user_pool_login = user_pool_token("a-user-pool-user");
    let user_pool_proven = emulator
        .identity(&pool_id, Some(json!({USER_POOL_PROVIDER: &user_pool_login})))
        .await;
    let facebook_login = "a-facebook-access-token";
    let facebook_proven = emulator
        .identity(&pool_id, Some(json!({PUBLIC_PROVIDER: facebook_login})))
        .await;

    // (case, the IdentityId sent, the logins presented, the sentence owed)
    type Row<'a> = (&'a str, &'a str, Vec<Login<'a>>, &'a str);
    let rows: Vec<Row<'_>> = vec![
        // A developer user identifier the pool has registered against NOTHING, for an identity
        // that already stands for a provider's user and one of the backend's.
        (
            "the joined account, its own id token beside a new identifier",
            joined.as_str(),
            vec![
                (USER_POOL_PROVIDER, joined_token.as_str()),
                (DEVELOPER_PROVIDER, "a-brand-new-backend-user"),
            ],
            CANNOT_LINK_LOGINS,
        ),
        (
            "the joined account, a new identifier and none of its logins",
            joined.as_str(),
            vec![(DEVELOPER_PROVIDER, "a-second-brand-new-backend-user")],
            CANNOT_LINK_LOGINS,
        ),
        // The join of two accounts an identity provider's login proves, over which side the
        // request names and which kind of login proves it.
        (
            "the joined account, its own id token beside the other joined account's \
             identifier",
            joined.as_str(),
            vec![
                (USER_POOL_PROVIDER, joined_token.as_str()),
                (DEVELOPER_PROVIDER, other_joined_user),
            ],
            CANNOT_MERGE_IDENTITIES,
        ),
        (
            "an identity a user pool's id token proves, its own token beside a joined \
             account's identifier",
            user_pool_proven.as_str(),
            vec![
                (USER_POOL_PROVIDER, user_pool_login.as_str()),
                (DEVELOPER_PROVIDER, joined_user),
            ],
            CANNOT_MERGE_IDENTITIES,
        ),
        (
            "an identity a public provider's access token proves, its own token beside a \
             joined account's identifier",
            facebook_proven.as_str(),
            vec![
                (PUBLIC_PROVIDER, facebook_login),
                (DEVELOPER_PROVIDER, other_joined_user),
            ],
            CANNOT_MERGE_IDENTITIES,
        ),
    ];

    // What the pool held before the refusals, so they can be shown to have written nothing.
    let watched = [
        ("the joined account", joined.as_str()),
        ("the other joined account", other_joined.as_str()),
        ("the identity a user pool's id token proves", user_pool_proven.as_str()),
        ("the identity a public token proves", facebook_proven.as_str()),
    ];
    let mut before: Vec<(&str, &str, Vec<String>)> = Vec::new();
    for (case, identity_id) in watched {
        before.push((case, identity_id, described_logins(&emulator, identity_id).await));
    }
    let identities_before = identity_count(&emulator, &pool_id).await;

    for (case, identity_id, presented, sentence) in &rows {
        let (status, body) = emulator
            .call(
                OPERATION,
                &token_request(&pool_id, presented, Some(identity_id), None),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (REFUSED_STATUS, CONFLICT),
            "case {case:?}: expected ({REFUSED_STATUS}, {CONFLICT}), got ({status}, {body})"
        );
        let message = error_message(case, &body);
        assert_eq!(
            message, *sentence,
            "case {case:?}: expected the fixed sentence {sentence:?}, got {body}"
        );
        // Fixed text: the refusal reports no identity of the pool, not even one the caller
        // named itself.
        for (watched_case, watched_identity, _) in &before {
            assert!(
                !message.contains(watched_identity),
                "case {case:?}: the refusal must name no identity, got {watched_case:?} in \
                 {body}"
            );
        }
    }

    // A refused call wrote nothing: every identity carries the logins it carried, the pool
    // holds the identities it held, and each registered identifier still resolves to its own
    // identity.
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
    for (identifier, owed) in [(joined_user, &joined), (other_joined_user, &other_joined)] {
        assert_eq!(
            &resolved_identity(&emulator, &pool_id, identifier).await,
            owed,
            "the identifier {identifier:?} still resolves to the identity it was registered \
             under"
        );
    }
}

/// RULE: the identity a developer user identifier IS registered in — a new identity of the
/// pool, an unauthenticated one, an identity only the backend's own users reach, or one only an
/// identity provider's login proves — takes the registration and answers its own id; and where
/// the identifier is one the pool has already registered in an account the backend's own logins
/// alone reach, the call JOINS that identity into the one it proved, which survives carrying
/// the developer provider once per user it now stands for while the joined-away one is left
/// reached by no login at all.
#[tokio::test]
async fn the_identity_a_call_registers_a_developer_user_in_takes_the_registration() {
    let emulator = Emulator::new();
    let pool_id = registration_pool(&emulator, "developer-registration-targets").await;

    // The identities the rows name: two only a user pool's id token proves, one only a public
    // provider's access token proves, an unauthenticated guest, an identity only the backend's
    // own users reach, and the joined account of a provider's user and one of the backend's.
    let first_login = user_pool_token("the-first-user-pool-user");
    let first_proven = emulator
        .identity(&pool_id, Some(json!({USER_POOL_PROVIDER: &first_login})))
        .await;
    let second_login = user_pool_token("the-second-user-pool-user");
    let second_proven = emulator
        .identity(&pool_id, Some(json!({USER_POOL_PROVIDER: &second_login})))
        .await;
    let facebook_login = "the-targets-facebook-access-token";
    let facebook_proven = emulator
        .identity(&pool_id, Some(json!({PUBLIC_PROVIDER: facebook_login})))
        .await;
    let guest = emulator.identity(&pool_id, None).await;
    let backends_own_user = "the-backends-only-user";
    let backends_own = registered(
        &emulator,
        "the registration of an identity only the backend's users reach",
        &pool_id,
        &[(DEVELOPER_PROVIDER, backends_own_user)],
        None,
    )
    .await;
    let second_backends_own_user = "the-backends-second-only-user";
    let second_backends_own = registered(
        &emulator,
        "the registration of a second identity only the backend's users reach",
        &pool_id,
        &[(DEVELOPER_PROVIDER, second_backends_own_user)],
        None,
    )
    .await;
    let joined_user = "the-target-joined-accounts-backend-user";
    let joined_token = user_pool_token("the-target-joined-accounts-user-pool-user");
    let joined = registered(
        &emulator,
        "the implicit link that makes the joined account",
        &pool_id,
        &[
            (USER_POOL_PROVIDER, joined_token.as_str()),
            (DEVELOPER_PROVIDER, joined_user),
        ],
        None,
    )
    .await;

    // (case, the IdentityId sent, the logins presented, the provider names the identity it
    // answers must carry afterwards, the identity this call joins away)
    type Row<'a> = (
        &'a str,
        &'a str,
        Vec<Login<'a>>,
        Vec<&'a str>,
        Option<&'a str>,
    );
    let rows: Vec<Row<'_>> = vec![
        // A fresh identifier, for identities that hold none of the backend's users yet.
        (
            "an identity only a user pool's id token proves, its own token beside a new \
             identifier",
            first_proven.as_str(),
            vec![
                (USER_POOL_PROVIDER, first_login.as_str()),
                (DEVELOPER_PROVIDER, "the-first-user-pool-users-backend-id"),
            ],
            vec![USER_POOL_PROVIDER, DEVELOPER_PROVIDER],
            None,
        ),
        (
            "an identity only a public provider's access token proves, its own token beside a \
             new identifier",
            facebook_proven.as_str(),
            vec![
                (PUBLIC_PROVIDER, facebook_login),
                (DEVELOPER_PROVIDER, "the-facebook-users-backend-id"),
            ],
            vec![PUBLIC_PROVIDER, DEVELOPER_PROVIDER],
            None,
        ),
        (
            "an unauthenticated identity, with a new identifier",
            guest.as_str(),
            vec![(DEVELOPER_PROVIDER, "the-guests-backend-id")],
            vec![DEVELOPER_PROVIDER],
            None,
        ),
        // An identifier the pool has already registered in an account its own logins alone
        // reach: the JOIN of that identity into the one this call proved.
        (
            "an identity only a user pool's id token proves, its own token beside an \
             identifier the backend's own account holds",
            second_proven.as_str(),
            vec![
                (USER_POOL_PROVIDER, second_login.as_str()),
                (DEVELOPER_PROVIDER, backends_own_user),
            ],
            vec![USER_POOL_PROVIDER, DEVELOPER_PROVIDER],
            Some(backends_own.as_str()),
        ),
        (
            "the joined account, its own id token beside an identifier the backend's own \
             account holds",
            joined.as_str(),
            vec![
                (USER_POOL_PROVIDER, joined_token.as_str()),
                (DEVELOPER_PROVIDER, second_backends_own_user),
            ],
            // One entry per user the account now stands for: the provider's user and the two
            // of the backend's.
            vec![USER_POOL_PROVIDER, DEVELOPER_PROVIDER, DEVELOPER_PROVIDER],
            Some(second_backends_own.as_str()),
        ),
        // And the re-login of a developer user of the joined account, which joins nothing.
        (
            "the joined account, its own id token beside its own identifier",
            joined.as_str(),
            vec![
                (USER_POOL_PROVIDER, joined_token.as_str()),
                (DEVELOPER_PROVIDER, joined_user),
            ],
            vec![USER_POOL_PROVIDER, DEVELOPER_PROVIDER, DEVELOPER_PROVIDER],
            None,
        ),
    ];

    for (case, identity_id, presented, owed_logins, joined_away) in &rows {
        let (status, body) = emulator
            .call(
                OPERATION,
                &token_request(&pool_id, presented, Some(identity_id), None),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the identity takes the registration, got {status} with {body}"
        );
        assert_eq!(
            served_identity(case, &body).as_str(),
            *identity_id,
            "case {case:?}: expected the identity {identity_id:?} to answer, got {body}"
        );
        let mut owed: Vec<String> = owed_logins
            .iter()
            .map(|provider| (*provider).to_string())
            .collect();
        owed.sort();
        let mut carried = described_logins(&emulator, identity_id).await;
        carried.sort();
        assert_eq!(
            carried, owed,
            "case {case:?}: expected the identity to carry {owed:?}, got {carried:?}"
        );
        // Every developer user identifier the call presented resolves to the identity it
        // answered, whether it was registered by this call or joined into it.
        let identifier = presented
            .iter()
            .find(|(provider, _)| *provider == DEVELOPER_PROVIDER)
            .map(|(_, identifier)| *identifier)
            .expect("every row presents a developer login");
        assert_eq!(
            resolved_identity(&emulator, &pool_id, identifier).await.as_str(),
            *identity_id,
            "case {case:?}: the identifier {identifier:?} resolves to the identity it is \
             registered in"
        );
        // The identity a join took the registration FROM is reached by no login of its own
        // afterwards, so a read of it projects the sentinel that join leaves behind.
        if let Some(joined_away) = joined_away {
            let projected = described_logins(&emulator, joined_away).await;
            assert_eq!(
                projected.first().map(String::as_str),
                Some(DISABLED_PROJECTION),
                "case {case:?}: the identity {joined_away:?} the call joined away projects the \
                 sentinel, got {projected:?}"
            );
        }
    }
}
