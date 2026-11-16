// SPDX-License-Identifier: CC0-1.0

//! Resolving a presented bearer token does not depend on when the clock's own reclaim was
//! materialised.
//!
//! RULE: a sign-in record this store is owed to reclaim — its own expiry passed and no role
//! credential still inside its duration issued to it — answers nothing, so what a presented
//! access token names is the same before and after that reclaim runs. The expiry sweep is
//! normalisation the clock already owed, not information: whoever asks first (the
//! service's authentication gate, which decides a request's fate before any handler is
//! reached) and whoever asks after sweeping (every handler) therefore reach the same
//! verdict for the same token and instant.
//!
//! The first test asserts the invariance itself, over every state a record can be in when
//! the question is asked, crossed with every state the sign-in the rest of the emulator
//! holds for the same token can be in. The second asserts it where it is observable on the
//! wire: the gate serves a request it would once have refused 401 while the handler behind
//! it would have served it, and still refuses the one whose record carries the thing only
//! this store knows — a `Logout`.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use http::StatusCode;
use serde_json::Value;

use service_com_amazonaws_sso_swbportalservice::state::{
    self, Account, ComAmazonawsSsoSwbportalserviceState, RoleCredentialRecord, SignedIn,
    INVALID_SESSION_MESSAGE,
};

use crate::minted_sign_in::{
    assigned_emulator, error_code, granted, reported_accounts, reported_roles, role_name, text,
    Emulator,
};

/// The token every case presents. One string, held by this store and by the emulator's own
/// token mint alike — which is what makes the two sources' disagreement observable.
const TOKEN: &str = "tok-held-by-both";

/// Who the record THIS store holds signed in as, and the account and role its own
/// assignment names.
const RECORD_SUBJECT: &str = "recorded@example.com";
const RECORD_ACCOUNT: &str = "111111111111";
const RECORD_ROLE: &str = "RecordedAccess";

/// Who the sign-in resolved from the REST of the emulator is, with its own account and
/// role, so an answer that came from the mint cannot be confused with one that came from
/// the record.
const MINT_SUBJECT: &str = "minted@example.com";
/// The sign-in session identifier the mint's record carries for [`TOKEN`] — a value of the
/// mint's own, so it is neither the token nor anything this store holds.
const MINT_SIGN_IN_SESSION: &str = "sign-in-session-of-the-mint";
const MINT_ACCOUNT: &str = "222222222222";
const MINT_ROLE: &str = "MintedAccess";

/// A margin wide enough that nothing in a case depends on how fast the host runs: an
/// expiry put this far out is in the future for the whole case, and one put this far back
/// is in the past for all of it.
const MARGIN: Duration = Duration::from_secs(3600);

/// The state of the record this store holds under [`TOKEN`] when the question is asked.
#[derive(Debug, Clone, Copy)]
enum Held {
    /// No record at all.
    Nothing,
    /// A sign-in still inside its own expiry.
    Live,
    /// One whose own expiry has passed, with no role credential issued to it: a record the
    /// clock has made spent.
    Expired,
    /// One whose expiry has passed with a role credential issued to it that is still inside
    /// its own duration — the credential this record is the identity of, so the record is
    /// kept whatever its expiry.
    ExpiredWithLiveCredential,
    /// The same, with a credential whose own duration has run out too: nothing holds the
    /// record back any more.
    ExpiredWithSpentCredential,
    /// A sign-in `Logout` ended, still inside its own expiry.
    LoggedOut,
    /// A sign-in `Logout` ended whose own expiry has passed since, with no credential: the
    /// record is spent, and the veto it carried lasted exactly as long as the record did.
    LoggedOutAndExpired,
}

/// The state of the sign-in the REST of the emulator holds for the same token.
#[derive(Debug, Clone, Copy)]
enum Minted {
    /// A sign-in inside its own lifetime.
    Live,
    /// One whose own lifetime has run out.
    Expired,
    /// None at all: no other service of the emulator holds this token.
    Nothing,
}

/// What the presented token must resolve to.
#[derive(Debug, Clone, Copy)]
enum Resolves {
    /// The sign-in resolved from the rest of the emulator.
    Mint,
    /// The sign-in this store was given.
    Record,
    /// No sign-in: the `UnauthorizedException` case.
    Nothing,
}

/// A sign-in's subject and its accounts with the roles in each — the whole of what
/// `signed_in` answers, in a form two answers can be compared by.
type Resolved = Option<(String, Vec<(String, Vec<String>)>)>;

fn projected(signed_in: Option<&SignedIn>) -> Resolved {
    signed_in.map(|sign_in| {
        (
            sign_in.subject.clone(),
            sign_in
                .accounts
                .iter()
                .map(|(account_id, account)| {
                    (
                        account_id.clone(),
                        account.roles.iter().cloned().collect::<Vec<String>>(),
                    )
                })
                .collect(),
        )
    })
}

/// What each [`Resolves`] expects, spelled out rather than recomputed from the store.
fn expected(resolves: Resolves) -> Resolved {
    match resolves {
        Resolves::Mint => Some((
            MINT_SUBJECT.to_string(),
            vec![(MINT_ACCOUNT.to_string(), vec![MINT_ROLE.to_string()])],
        )),
        Resolves::Record => Some((
            RECORD_SUBJECT.to_string(),
            vec![(RECORD_ACCOUNT.to_string(), vec![RECORD_ROLE.to_string()])],
        )),
        Resolves::Nothing => None,
    }
}

/// The sign-in the rest of the emulator holds for [`TOKEN`], as `federated_sign_in`
/// resolves one: its own subject, its own assigned account, and the expiry `minted` puts on
/// it.
fn federated(minted: Minted, now: SystemTime) -> Option<SignedIn> {
    let expires_at = match minted {
        Minted::Live => now + MARGIN,
        Minted::Expired => now - MARGIN,
        Minted::Nothing => return None,
    };
    let mut roles = BTreeSet::new();
    roles.insert(MINT_ROLE.to_string());
    Some(SignedIn {
        subject: MINT_SUBJECT.to_string(),
        // The sign-in session the mint would name for a token of its own chain; this suite
        // never paginates, so only its presence matters here.
        sign_in_session: MINT_SIGN_IN_SESSION.to_string(),
        expires_at,
        accounts: [(
            MINT_ACCOUNT.to_string(),
            Account {
                account_id: MINT_ACCOUNT.to_string(),
                account_name: None,
                email_address: None,
                roles,
            },
        )]
        .into_iter()
        .collect(),
    })
}

/// A role credential issued to [`TOKEN`] for its recorded account and role, expiring at
/// `expires_at`.
fn credential(expires_at: SystemTime) -> ((String, String, String), RoleCredentialRecord) {
    (
        (
            TOKEN.to_string(),
            RECORD_ACCOUNT.to_string(),
            RECORD_ROLE.to_string(),
        ),
        RoleCredentialRecord {
            // Placeholders: this case needs a credential to EXIST for the token, and
            // nothing reads its id or its secrets. A credential a caller is handed is
            // minted by the emulator's own credential store, never written by hand.
            access_key_id: "role-credential-of-the-record".to_string(),
            secret_access_key: "unread".to_string(),
            session_token: "unread".to_string(),
            expiration_millis: state::epoch_millis(expires_at),
        },
    )
}

/// A store holding `held`'s record of [`TOKEN`] at `now`.
fn store_holding(held: Held, now: SystemTime) -> ComAmazonawsSsoSwbportalserviceState {
    let mut store = ComAmazonawsSsoSwbportalserviceState::default();
    // The account the recorded sign-in's own assignment names: a seeded sign-in reports
    // only accounts this store holds a record of.
    store.put_account(RECORD_ACCOUNT, "recorded", "recorded@example.com");
    if matches!(held, Held::Nothing) {
        return store;
    }
    store.put_session(
        TOKEN,
        RECORD_SUBJECT,
        [(RECORD_ACCOUNT, [RECORD_ROLE])],
        MARGIN,
    );
    // Ending the sign-in is what only this store knows, and it is recorded while the record
    // is still usable, exactly as `Logout` does it.
    if matches!(held, Held::LoggedOut | Held::LoggedOutAndExpired) {
        assert!(
            store.invalidate_session(TOKEN, now),
            "the fixture must be able to end the sign-in it just seeded"
        );
    }
    // Then the clock: an expiry in the past is the state time itself reaches.
    if !matches!(held, Held::Live | Held::LoggedOut) {
        store
            .sessions
            .get_mut(TOKEN)
            .expect("the fixture just seeded the record")
            .expires_at = now - MARGIN;
    }
    match held {
        Held::ExpiredWithLiveCredential => {
            let (key, record) = credential(now + MARGIN);
            store.issued_credentials.insert(key, record);
        }
        Held::ExpiredWithSpentCredential => {
            let (key, record) = credential(now - MARGIN);
            store.issued_credentials.insert(key, record);
        }
        _ => {}
    }
    store
}

/// RULE: a sign-in record the clock has made spent — its own expiry passed and no role
/// credential still inside its duration issued to it — answers nothing, so the sign-in a
/// presented token names is the same whether the reclaim that record is owed has already
/// run or is still outstanding; and the reclaim drops exactly the records resolution
/// disregards.
#[test]
fn resolving_a_token_does_not_depend_on_the_sweep_having_run() {
    // Rows: every state the record under the presented token can be in, crossed with every
    // state the sign-in the rest of the emulator holds for that same token can be in. The
    // last column is whether the reclaim keeps the record, which is the same question
    // resolution reads it by.
    let rows: &[(Held, Minted, Resolves, bool)] = &[
        (Held::Nothing, Minted::Live, Resolves::Mint, false),
        (Held::Nothing, Minted::Expired, Resolves::Nothing, false),
        (Held::Nothing, Minted::Nothing, Resolves::Nothing, false),
        // A record still inside its own expiry answers; the sign-in the rest of the
        // emulator holds is reported as it stands when it is usable.
        (Held::Live, Minted::Live, Resolves::Mint, true),
        (Held::Live, Minted::Expired, Resolves::Record, true),
        (Held::Live, Minted::Nothing, Resolves::Record, true),
        // Spent: the record answers nothing, so the token resolves exactly as it would once
        // the reclaim had dropped it.
        (Held::Expired, Minted::Live, Resolves::Mint, false),
        (Held::Expired, Minted::Expired, Resolves::Nothing, false),
        (Held::Expired, Minted::Nothing, Resolves::Nothing, false),
        // Not spent: a credential inside its own duration keeps the record, and a record
        // that is kept and unusable refuses the token.
        (
            Held::ExpiredWithLiveCredential,
            Minted::Live,
            Resolves::Nothing,
            true,
        ),
        (
            Held::ExpiredWithLiveCredential,
            Minted::Expired,
            Resolves::Nothing,
            true,
        ),
        (
            Held::ExpiredWithLiveCredential,
            Minted::Nothing,
            Resolves::Nothing,
            true,
        ),
        // A credential whose own duration has run out holds nothing back: it is itself a
        // record the clock has made spent.
        (
            Held::ExpiredWithSpentCredential,
            Minted::Live,
            Resolves::Mint,
            false,
        ),
        (
            Held::ExpiredWithSpentCredential,
            Minted::Expired,
            Resolves::Nothing,
            false,
        ),
        (
            Held::ExpiredWithSpentCredential,
            Minted::Nothing,
            Resolves::Nothing,
            false,
        ),
        // The one thing only this store knows vetoes the token for as long as the record
        // lasts — which is until the clock makes it spent, and no longer.
        (Held::LoggedOut, Minted::Live, Resolves::Nothing, true),
        (Held::LoggedOut, Minted::Expired, Resolves::Nothing, true),
        (Held::LoggedOut, Minted::Nothing, Resolves::Nothing, true),
        (
            Held::LoggedOutAndExpired,
            Minted::Live,
            Resolves::Mint,
            false,
        ),
        (
            Held::LoggedOutAndExpired,
            Minted::Expired,
            Resolves::Nothing,
            false,
        ),
        (
            Held::LoggedOutAndExpired,
            Minted::Nothing,
            Resolves::Nothing,
            false,
        ),
    ];

    for (held, minted, resolves, kept) in rows {
        let case = format!("{held:?} in this store, {minted:?} elsewhere");
        let now = SystemTime::now();
        let mut store = store_holding(*held, now);
        let federated = federated(*minted, now);

        // Asked with the reclaim still outstanding, as the service's authentication gate
        // asks it.
        let before = projected(store.signed_in(TOKEN, federated.as_ref(), now).as_ref());
        assert_eq!(
            before,
            expected(*resolves),
            "case {case:?}: the token must resolve to {resolves:?} with the reclaim still \
             outstanding, got {before:?}"
        );

        // The reclaim itself: what `end_expired_credentials` materialises in this store,
        // which every handler runs before it resolves anything.
        store.expire_credentials(now);
        store.expire_sessions(now);
        assert_eq!(
            store.sessions.contains_key(TOKEN),
            *kept,
            "case {case:?}: the reclaim must {} the record, holding {:?}",
            if *kept { "keep" } else { "drop" },
            store.sessions.get(TOKEN)
        );

        // And asked again afterwards, as a handler asks it.
        let after = projected(store.signed_in(TOKEN, federated.as_ref(), now).as_ref());
        assert_eq!(
            after, before,
            "case {case:?}: the same token and instant must resolve to the same sign-in \
             across the reclaim, got {before:?} before and {after:?} after"
        );
    }
}

/// What this store has been made to hold for the token a request presents, before that
/// request is dispatched.
#[derive(Debug, Clone, Copy)]
enum Seeded {
    /// Nothing: the sign-in is the emulator's own, resolved afresh on every request.
    Nothing,
    /// A record the clock has made spent: its expiry passed and no credential issued to it.
    Spent,
    /// A record `Logout` ended, still inside its own expiry.
    LoggedOut,
}

/// One portal operation, as a request a case dispatches.
#[derive(Debug, Clone, Copy)]
enum Operation {
    ListAccounts,
    ListAccountRoles,
    GetRoleCredentials,
    Logout,
}

/// Seed this store with `seeded`'s record of `token`, the way whatever stands an emulator up
/// hands a sign-in to the portal directly.
fn seed(emulator: &Emulator, seeded: Seeded, token: &str) {
    let now = SystemTime::now();
    let store = emulator.portal_store();
    let mut store = store.lock();
    match seeded {
        Seeded::Nothing => {}
        Seeded::Spent => {
            // No assignment of its own, so an answer served from THIS record could not
            // report the accounts the assignments elsewhere in the emulator grant.
            store.put_session(
                token,
                RECORD_SUBJECT,
                Vec::<(String, Vec<String>)>::new(),
                MARGIN,
            );
            store
                .sessions
                .get_mut(token)
                .expect("the fixture just seeded the record")
                .expires_at = now - MARGIN;
        }
        Seeded::LoggedOut => {
            store.put_session(
                token,
                RECORD_SUBJECT,
                Vec::<(String, Vec<String>)>::new(),
                MARGIN,
            );
            assert!(
                store.invalidate_session(token, now),
                "the fixture must be able to end the sign-in it just seeded"
            );
        }
    }
}

/// RULE (on the wire): the authentication gate that decides a request's fate before any
/// handler runs resolves the presented token exactly as the handler behind it does, so it
/// serves every request a handler would serve — including one whose record this store is
/// owed to reclaim — and refuses only what the handler refuses.
#[tokio::test]
async fn the_gate_serves_what_the_handler_would_serve() {
    let rows = granted();
    let assigned = assigned_emulator(&rows).await;
    let emulator = &assigned.emulator;
    let account_id = rows[0].account_id.clone();
    let assigned_role = role_name(rows[0].permission_sets[0], &account_id);
    let mut assigned_accounts: Vec<String> =
        rows.iter().map(|row| row.account_id.clone()).collect();
    assigned_accounts.sort();
    let mut assigned_roles: Vec<String> = rows[0]
        .permission_sets
        .iter()
        .map(|name| role_name(name, &account_id))
        .collect();
    assigned_roles.sort();

    // Rows: what this store holds for the presented token, and whether the request is
    // served. Each is dispatched once per operation, on a sign-in of its own, with the
    // record re-seeded for each — the first request through sweeps it away.
    let dispatched: &[(Seeded, bool)] = &[
        (Seeded::Nothing, true),
        (Seeded::Spent, true),
        (Seeded::LoggedOut, false),
    ];
    let operations = [
        Operation::ListAccounts,
        Operation::ListAccountRoles,
        Operation::GetRoleCredentials,
        Operation::Logout,
    ];

    for (seeded, served) in dispatched {
        for operation in operations {
            let case = format!("{seeded:?}, {operation:?}");
            // A sign-in of its own per request, so nothing one case ends is what the next
            // one presents.
            let token = emulator.minted_token(&assigned.identity_store_id).await;
            seed(emulator, *seeded, &token);

            let (status, body) = match operation {
                Operation::ListAccounts => emulator.list_accounts(&token).await,
                Operation::ListAccountRoles => {
                    emulator.list_account_roles(&token, &account_id).await
                }
                Operation::GetRoleCredentials => {
                    emulator
                        .role_credentials(&token, &account_id, &assigned_role)
                        .await
                }
                Operation::Logout => emulator.logout(&token).await,
            };

            if !*served {
                assert_eq!(
                    (
                        status,
                        error_code(&body),
                        body["message"].as_str().unwrap_or_default().to_string()
                    ),
                    (
                        StatusCode::UNAUTHORIZED,
                        "UnauthorizedException".to_string(),
                        INVALID_SESSION_MESSAGE.to_string()
                    ),
                    "case {case:?}: the request must be refused as unauthorized, got \
                     {status} {body}"
                );
                continue;
            }

            assert_eq!(
                status,
                StatusCode::OK,
                "case {case:?}: the request must be served, got {status} {body}"
            );
            // And served from the sign-in the rest of the emulator holds, not from the
            // record this store was owed to reclaim: the accounts and roles reported are
            // the assignments an administrator made.
            match operation {
                Operation::ListAccounts => assert_eq!(
                    reported_accounts(&body),
                    assigned_accounts,
                    "case {case:?}: every assigned account must be reported, got {body}"
                ),
                Operation::ListAccountRoles => assert_eq!(
                    reported_roles(&body),
                    assigned_roles,
                    "case {case:?}: every assigned role must be reported, got {body}"
                ),
                Operation::GetRoleCredentials => {
                    let credentials = body["roleCredentials"].clone();
                    assert!(
                        !text(&credentials, "accessKeyId").is_empty()
                            && !text(&credentials, "secretAccessKey").is_empty()
                            && !text(&credentials, "sessionToken").is_empty(),
                        "case {case:?}: the minted role credential must carry an id and \
                         both secrets, got {credentials}"
                    );
                    assert!(
                        credentials["expiration"].as_i64().unwrap_or_default()
                            > state::epoch_millis(SystemTime::now()),
                        "case {case:?}: the minted credential must not be expired already, \
                         got {credentials}"
                    );
                }
                Operation::Logout => assert_eq!(
                    body,
                    Value::Null,
                    "case {case:?}: Logout answers no body, got {body}"
                ),
            }
        }
    }
}
