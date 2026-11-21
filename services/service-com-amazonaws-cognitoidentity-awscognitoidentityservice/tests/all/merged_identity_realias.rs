// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the identity a developer-identity exchange resolves AFTER a merge.
//!
//! RULE: a merge makes the two identities it joins ONE account — every login of the
//! merged-away side resolves to the identity that survives it — so a login of that survivor is
//! a login of the merged-away identity's own account, and a call naming the merged-away id
//! while presenting one is a call its caller has PROVEN: it is served on the identity still
//! standing for the account, answering that identity's `IdentityId` and a token minted for it,
//! whichever developer user identifier of the merged account the `Logins` map carries. The
//! logins-don't-match refusal is what is left for a map that proves no identity of that
//! account: an identity an unlink or a merge left linked to nothing is reached through no login
//! of its own, so a call naming it and presenting a developer user identifier of another
//! account — or one the pool has registered nothing against at all — has shown no authority
//! over it and links nothing into it. What a merge does NOT take is the tokens already issued
//! for the identity it merged away: the record they were minted from is still stored, so such a
//! token keeps naming that identity and keeps vending credentials for it.
//!
//! Each case asserts that rule over a TABLE of inputs rather than one recorded call: the rows
//! differ in how many developer users are merged and in which direction, how many identifiers
//! and federated logins each side carries, how many merges deep the chain from the named id to
//! the surviving identity is, and the spelling and size of the identifiers and of the pool's
//! own developer provider name.
//!
//! The harness is `identity_pool_flow`'s: one shared engine behind the same
//! `MultiServiceRouter` the production router builds. The developer users are registered with
//! `merge_developer_identities`' own helper, so every suite drives one registration path.

use serde_json::{json, Value};

use crate::describe_identity::describe;
use crate::developer_identity_tokens::{
    identity_count, jws_header_and_claims, pool_with_developer_provider, token_request,
};
// The projection a read owes for an identity a MERGE merged away, read through the sibling
// suite that pins that rule rather than kept here as a second copy of it.
use crate::disabled_identity_projection::merged_away_projection;
use crate::identity_pool_flow::{cognito_trust_policy, error_code, seeded_role_arn, Emulator};
use crate::merge_developer_identities::user;
// The one merge call this target drives, read through the sibling suite that keeps it rather
// than kept here as a second copy.
use crate::merged_identity_retention::merge;

/// The operation under test, spelled once.
const EXCHANGE: &str = "GetOpenIdTokenForDeveloperIdentity";

/// Every refused call of this service answers HTTP 400, whatever `@httpError` the error shape
/// annotates (`src/service/modeled_error_status.rs`).
const REFUSED_STATUS: u16 = 400;

/// How AWS words the refusal of a call that names an identity while presenting logins none of
/// which is one of that identity's — spelled out here as the literal, so the wire-visible text
/// is pinned by the test rather than read back out of the crate.
const UNMATCHED_LOGINS: &str = "Logins don't match. Please include at least one valid login \
                                for this identity or identity pool.";

/// A `Logins` entry: the provider name and the value presented under it.
type Login<'a> = (&'a str, &'a str);

/// One row of the re-alias table: the pool's developer provider, its developer users (each as
/// the identifiers it is registered under), the federated logins the FIRST user is also
/// reached through, the merges to perform as `(surviving user, merged-away user)` indexes into
/// those users, and which user's identity stands for the account when they are done.
struct Row<'a> {
    case: &'a str,
    developer_provider: &'a str,
    users: Vec<Vec<&'a str>>,
    first_user_federated: Vec<Login<'a>>,
    merges: Vec<(usize, usize)>,
    survivor: usize,
}

/// One row of the token-retention table: the merged-away user's developer user identifiers, the
/// federated logins it is also reached through, and the `TokenDuration` its token is minted
/// with (absent when the row sends none).
type TokenRow<'a> = (&'a str, Vec<&'a str>, Vec<Login<'a>>, Option<i64>);

/// The `message` an error response carries.
fn error_message(case: &str, body: &Value) -> String {
    body["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: an error response carries a message: {body}"))
        .to_string()
}

/// The `Logins` list a read of one identity reports, insisting the read is served.
async fn described_logins(emulator: &Emulator, case: &str, identity_id: &str) -> Vec<String> {
    let (status, body) = describe(emulator, identity_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {identity_id} to still be described with HTTP 200, got {status} \
         with {body}"
    );
    body["Logins"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a description carries a Logins list: {body}"))
        .iter()
        .map(|login| login.as_str().unwrap_or_default().to_string())
        .collect()
}

/// The identity `LookupDeveloperIdentity` resolves one developer user identifier to.
async fn resolved_identity(
    emulator: &Emulator,
    case: &str,
    pool_id: &str,
    identifier: &str,
) -> String {
    let looked_up = emulator
        .ok(
            "LookupDeveloperIdentity",
            &json!({
                "IdentityPoolId": pool_id,
                "DeveloperUserIdentifier": identifier,
                "MaxResults": 60,
            }),
        )
        .await;
    looked_up["IdentityId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the lookup answers an IdentityId: {looked_up}"))
        .to_string()
}

/// RULE: a merged-away identity names the account the merge produced, so the exchange naming
/// it answers the identity still standing for that account — with a token minted for THAT
/// identity — for every developer user identifier of the merged account, and mints no new
/// identity.
#[tokio::test]
async fn naming_a_merged_away_identity_serves_the_surviving_identity() {
    let emulator = Emulator::new();

    // The rows differ along what the rule generalises over: how many users are merged and in
    // which direction, how many identifiers each side carries, whether the first user is also
    // reached through federated logins, how many merges deep the chain from a merged-away id
    // to the surviving identity is, and the spelling and size of the identifiers
    // (`DeveloperUserIdentifier` is modelled `length 1..1024`, so one row sits on each
    // published bound) and of the pool's own `DeveloperProviderName` (modelled
    // `length 1..128, pattern ^[\w._-]+$`, one row on its published maximum) —
    // `smithy_query(query="operation",operation="MergeDeveloperIdentities")`.
    let at_max_identifier = "x".repeat(1024);
    let at_max_provider = "p".repeat(128);
    let rows: Vec<Row<'_>> = vec![
        Row {
            case: "one identifier each, the second user surviving",
            developer_provider: "login.example.app",
            users: vec![vec!["user-dst"], vec!["user-src"]],
            first_user_federated: Vec::new(),
            merges: vec![(1, 0)],
            survivor: 1,
        },
        Row {
            case: "one identifier each, the first user surviving",
            developer_provider: "login.example.app",
            users: vec![vec!["user-src"], vec!["user-dst"]],
            first_user_federated: Vec::new(),
            merges: vec![(0, 1)],
            survivor: 0,
        },
        Row {
            case: "three identifiers on the merged-away user",
            developer_provider: "backend_1.example-app",
            users: vec![vec!["a-user", "m-user", "z-user"], vec!["survivor-user"]],
            first_user_federated: Vec::new(),
            merges: vec![(1, 0)],
            survivor: 1,
        },
        Row {
            case: "federated logins on the merged-away user",
            developer_provider: "login.example.app",
            users: vec![vec!["user-dst"], vec!["s-1", "s-2"]],
            first_user_federated: vec![
                ("accounts.google.com", "google-absorbed"),
                ("graph.facebook.com", "facebook-absorbed"),
            ],
            merges: vec![(1, 0)],
            survivor: 1,
        },
        Row {
            case: "three users merged one after another, two merges deep",
            developer_provider: "dev_provider",
            users: vec![vec!["user-a"], vec!["user-b"], vec!["user-c"]],
            first_user_federated: Vec::new(),
            merges: vec![(1, 0), (2, 1)],
            survivor: 2,
        },
        Row {
            case: "identifiers at the minimum and the maximum modelled length",
            developer_provider: at_max_provider.as_str(),
            users: vec![vec![at_max_identifier.as_str()], vec!["s"]],
            first_user_federated: Vec::new(),
            merges: vec![(1, 0)],
            survivor: 1,
        },
        Row {
            case: "identifiers with spaces, punctuation and non-ASCII",
            developer_provider: "login.example.app",
            users: vec![
                vec!["Jane Doe #7 (eu)", "ユーザー-42"],
                vec!["person@example.com"],
            ],
            first_user_federated: Vec::new(),
            merges: vec![(1, 0)],
            survivor: 1,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // One pool per row, so the rows cannot interfere through a shared login index.
        let pool_id = pool_with_developer_provider(
            &emulator,
            &format!("merge-realias-{index}"),
            Some(row.developer_provider),
        )
        .await;
        let mut minted = Vec::with_capacity(row.users.len());
        for (user_index, identifiers) in row.users.iter().enumerate() {
            let federated: &[Login<'_>] = if user_index == 0 {
                &row.first_user_federated
            } else {
                &[]
            };
            minted.push(
                user(
                    &emulator,
                    &pool_id,
                    row.developer_provider,
                    identifiers,
                    federated,
                )
                .await,
            );
        }

        for (survivor, merged_away) in &row.merges {
            let answered = merge(
                &emulator,
                case,
                &pool_id,
                row.developer_provider,
                row.users[*survivor][0],
                row.users[*merged_away][0],
            )
            .await;
            assert_eq!(
                answered, minted[*survivor],
                "case {case:?}: expected the merge to answer {}, got {answered}",
                minted[*survivor]
            );
        }
        let survivor = minted[row.survivor].clone();
        let merged_away: Vec<String> = minted
            .iter()
            .enumerate()
            .filter(|(user_index, _)| *user_index != row.survivor)
            .map(|(_, identity_id)| identity_id.clone())
            .collect();
        let every_identifier: Vec<&str> = row
            .users
            .iter()
            .flat_map(|identifiers| identifiers.iter().copied())
            .collect();
        let before = identity_count(&emulator, &pool_id).await;

        // Every merged-away id, with every developer user identifier of the merged account:
        // each call is served, answers the identity standing for the account, and mints a
        // token for THAT identity.
        for named in &merged_away {
            for identifier in &every_identifier {
                let row_case = format!("{case}: {named} with {identifier:?}");
                let (status, body) = emulator
                    .call(
                        EXCHANGE,
                        &token_request(
                            &pool_id,
                            &[(row.developer_provider, identifier)],
                            Some(named),
                            None,
                        ),
                    )
                    .await;
                assert_eq!(
                    status, 200,
                    "case {row_case:?}: expected the re-login of a merged account to be served \
                     with HTTP 200, got {status} with {body}"
                );
                assert_eq!(
                    body["IdentityId"],
                    json!(survivor),
                    "case {row_case:?}: expected the surviving identity {survivor}, got {body}"
                );
                let token = body["Token"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {row_case:?}: a served call mints a token: {body}"));
                assert!(
                    !token.is_empty(),
                    "case {row_case:?}: the token must not be empty, got {body}"
                );
                let (_, claims) = jws_header_and_claims(&row_case, token);
                assert_eq!(
                    claims["sub"],
                    json!(survivor),
                    "case {row_case:?}: expected the token to name {survivor} as its subject, got \
                     {claims}"
                );
            }

            // The re-routed call leaves the merged-away record as the merge left it: a read of
            // it is still served, still projecting the sentinel a merge leaves behind, so
            // nothing was resurrected or re-linked by the exchange being served through it.
            assert_eq!(
                described_logins(&emulator, case, named).await,
                merged_away_projection(&pool_id),
                "case {case:?}: expected {named} to keep projecting the merged-away sentinel"
            );
        }

        // And nothing new was minted: the pool still holds one identity per user it ever
        // minted, and every identifier of the merged account still resolves to the survivor.
        assert_eq!(
            identity_count(&emulator, &pool_id).await,
            before,
            "case {case:?}: resolving a merged account mints no identity"
        );
        for identifier in &every_identifier {
            assert_eq!(
                resolved_identity(&emulator, case, &pool_id, identifier).await,
                survivor,
                "case {case:?}: expected {identifier:?} to resolve to {survivor}"
            );
        }
    }
}

/// RULE (its boundary): an identity reached through NO login of its own is one no call can
/// prove and nothing can be linked into, so a call naming one and presenting a developer user
/// identifier of another account — or one the pool has registered nothing against at all — is
/// refused with the logins-don't-match sentence, and writes nothing.
#[tokio::test]
async fn an_identity_no_login_reaches_refuses_a_login_of_no_account_of_its_own() {
    let emulator = Emulator::new();
    let developer_provider = "login.example.app";
    let public_provider = "graph.facebook.com";
    let pool_id =
        pool_with_developer_provider(&emulator, "merge-realias-boundary", Some(developer_provider))
            .await;

    // A merged account: "user-dst"'s identity is merged away into "user-src"'s.
    let merged_away = user(&emulator, &pool_id, developer_provider, &["user-dst"], &[]).await;
    let survivor = user(&emulator, &pool_id, developer_provider, &["user-src"], &[]).await;
    merge(
        &emulator,
        "boundary fixture",
        &pool_id,
        developer_provider,
        "user-src",
        "user-dst",
    )
    .await;
    // A THIRD developer user of the pool, merged with nothing.
    let unmerged = user(&emulator, &pool_id, developer_provider, &["user-third"], &[]).await;
    // An identity an UNLINK — not a merge — left linked to nothing: disabled, and reached
    // through no login at all.
    let federated_login = "a-facebook-access-token";
    let unlinked = emulator
        .identity(&pool_id, Some(json!({public_provider: federated_login})))
        .await;
    emulator
        .ok(
            "UnlinkIdentity",
            &json!({
                "IdentityId": &unlinked,
                "Logins": {public_provider: federated_login},
                "LoginsToRemove": [public_provider],
            }),
        )
        .await;
    let before = identity_count(&emulator, &pool_id).await;

    // The rows differ along what the rule generalises over: WHICH identity reached through no
    // login is named (one an unlink left so, one a merge merged away), and what the identifier
    // presented beside it proves (another merged account's identity, an unmerged identity's,
    // nothing at all — plain, at the modelled minimum length, non-ASCII).
    // (case, the IdentityId named, the identifier presented)
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "an identity an unlink left linked to nothing, with a merged account's identifier",
            unlinked.as_str(),
            "user-src",
        ),
        (
            "an identity an unlink left linked to nothing, with another account's identifier",
            unlinked.as_str(),
            "user-third",
        ),
        (
            "an identity an unlink left linked to nothing, with an unregistered identifier",
            unlinked.as_str(),
            "fresh-user",
        ),
        (
            "a merged-away identity, with an unmerged account's identifier",
            merged_away.as_str(),
            "user-third",
        ),
        (
            "a merged-away identity, with an unregistered identifier",
            merged_away.as_str(),
            "fresh-user",
        ),
        (
            "a merged-away identity, with an unregistered identifier of one character",
            merged_away.as_str(),
            "u",
        ),
        (
            "a merged-away identity, with an unregistered non-ASCII identifier",
            merged_away.as_str(),
            "新しい-ユーザー",
        ),
        (
            "the surviving identity, with an unmerged account's identifier",
            survivor.as_str(),
            "user-third",
        ),
    ];

    for (case, named, identifier) in rows {
        let (status, body) = emulator
            .call(
                EXCHANGE,
                &token_request(
                    &pool_id,
                    &[(developer_provider, identifier)],
                    Some(named),
                    None,
                ),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (REFUSED_STATUS, "NotAuthorizedException"),
            "case {case:?}: expected ({REFUSED_STATUS}, NotAuthorizedException), got ({status}, \
             {body})"
        );
        assert_eq!(
            error_message(case, &body),
            UNMATCHED_LOGINS,
            "case {case:?}: expected the fixed logins-mismatch sentence, got {body}"
        );
    }

    // Every refusal left the accounts as they were: no identity was minted for a refused
    // registration, both merged identifiers still resolve to the survivor, the third user's own
    // identity still holds its identifier, and the identity an unlink disabled is still the
    // disabled record it was.
    assert_eq!(
        identity_count(&emulator, &pool_id).await,
        before,
        "a refused call must mint no identity"
    );
    for identifier in ["user-src", "user-dst"] {
        assert_eq!(
            resolved_identity(&emulator, "boundary", &pool_id, identifier).await,
            survivor,
            "expected {identifier:?} to still resolve to the survivor {survivor}"
        );
    }
    assert_eq!(
        resolved_identity(&emulator, "boundary", &pool_id, "user-third").await,
        unmerged,
        "expected the unmerged account to keep its own identifier"
    );
    assert_eq!(
        described_logins(&emulator, "boundary", &merged_away).await,
        merged_away_projection(&pool_id),
        "expected {merged_away} to keep projecting the merged-away sentinel"
    );
    assert_eq!(
        described_logins(&emulator, "boundary", &unlinked).await,
        vec!["DISABLED".to_string()],
        "expected {unlinked} to keep projecting the sentinel an unlink leaves behind"
    );
}

/// RULE: a merge takes an identity's LOGINS away, not the tokens already ISSUED for it — the
/// record those tokens were minted from is still stored — so an OpenID Connect token minted
/// for the merged-away identity before the merge still names that identity afterwards and
/// still vends credentials for it.
#[tokio::test]
async fn a_token_minted_before_the_merge_still_names_the_merged_away_identity() {
    let emulator = Emulator::new();
    let developer_provider = "login.example.app";

    // The rows differ along what the rule generalises over: how many developer user
    // identifiers and federated logins the merged-away user carries, the `TokenDuration` the
    // token was minted with (absent \u2014 the documented 15 minutes \u2014 and a window generous enough
    // to outlive every call below on any host), and which side of the merge the identity is.
    // (case, the merged-away user's identifiers, its federated logins, the TokenDuration)
    let rows: Vec<TokenRow<'_>> = vec![
        ("one identifier, no TokenDuration", vec!["user-dst"], Vec::new(), None),
        (
            "three identifiers, an explicit window",
            vec!["a-user", "m-user", "z-user"],
            Vec::new(),
            Some(3_600),
        ),
        (
            "one identifier and a federated login beside it",
            vec!["user-dst-2"],
            vec![("graph.facebook.com", "a-facebook-access-token")],
            Some(3_600),
        ),
    ];

    for (index, (case, identifiers, federated, token_duration)) in rows.iter().enumerate() {
        let pool_id = pool_with_developer_provider(
            &emulator,
            &format!("merge-token-{index}"),
            Some(developer_provider),
        )
        .await;
        // The vend below is an AUTHENTICATED exchange, so the pool is given a role that
        // trusts its own users.
        let role_arn = emulator.seed_role(
            &seeded_role_arn(&format!("merge-token-{index}"), "auth"),
            cognito_trust_policy(&pool_id),
        );
        emulator
            .ok(
                "SetIdentityPoolRoles",
                &json!({
                    "IdentityPoolId": &pool_id,
                    "Roles": {"authenticated": &role_arn, "unauthenticated": &role_arn},
                }),
            )
            .await;

        // The user whose identity the merge gives up, and the token it was handed BEFORE the
        // merge.
        let merged_away = user(
            &emulator,
            &pool_id,
            developer_provider,
            identifiers,
            federated,
        )
        .await;
        let minted = emulator
            .ok(
                EXCHANGE,
                &token_request(
                    &pool_id,
                    &[(developer_provider, identifiers[0])],
                    Some(&merged_away),
                    *token_duration,
                ),
            )
            .await;
        let token = minted["Token"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the mint answers a Token: {minted}"))
            .to_string();
        let (_, claims) = jws_header_and_claims(case, &token);
        assert_eq!(
            claims["sub"],
            json!(merged_away),
            "case {case:?}: expected the token to name {merged_away} as its subject, got {claims}"
        );

        let survivor = user(&emulator, &pool_id, developer_provider, &["user-src"], &[]).await;
        let answered = merge(
            &emulator,
            case,
            &pool_id,
            developer_provider,
            "user-src",
            identifiers[0],
        )
        .await;
        assert_eq!(
            answered, survivor,
            "case {case:?}: expected the merge to answer {survivor}, got {answered}"
        );

        // The token still names the identity it was minted for, and the vend presenting it is
        // served for that identity rather than refused or re-pointed at the survivor.
        let vended = emulator
            .ok(
                "GetCredentialsForIdentity",
                &json!({
                    "IdentityId": &merged_away,
                    "Logins": {"cognito-identity.amazonaws.com": &token},
                }),
            )
            .await;
        assert_eq!(
            vended["IdentityId"],
            json!(merged_away),
            "case {case:?}: expected the pre-merge token to keep vending for {merged_away}, got \
             {vended}"
        );
        assert!(
            vended["Credentials"]["AccessKeyId"].is_string(),
            "case {case:?}: expected the vend to answer an AccessKeyId, got {vended}"
        );
        // And the record the token names is still the disabled one the merge left behind.
        assert_eq!(
            described_logins(&emulator, case, &merged_away).await,
            merged_away_projection(&pool_id),
            "case {case:?}: expected {merged_away} to keep projecting the merged-away sentinel"
        );
    }
}
