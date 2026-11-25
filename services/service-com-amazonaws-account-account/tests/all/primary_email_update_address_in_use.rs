// SPDX-License-Identifier: CC0-1.0

//! `StartPrimaryEmailUpdate` against an address that is already in use, end to
//! end over HTTP.
//!
//! The rule under test: a root user email change to an address already in use as
//! an account's primary email address is refused with the modelled HTTP 409
//! `ConflictException` — the conflict this service's `ConflictException`
//! documents by example — at the step that ASKS for the change as much as at the
//! step that confirms it, and the refusal records nothing. An address is in use
//! when an account OTHER than the one being updated holds it, is owed it by
//! an accepted update whose completion the clock already owes, or is owed it as
//! the root address its own id names while this emulator has not materialised
//! its record yet; an address nobody else holds is admitted, so the refusal is
//! the address and never the account.
//!
//! The table spans the class rather than one request: its rows differ in HOW the
//! address came to be in use (the seeded root address of another record, an
//! address a completed flow put on another account, an address another account's
//! accepted update is still owed), in the address's length and form (both
//! published `@length` bounds included) and in the accounts involved.
//!
//! The rule is one address, one account, whichever order the two halves happen
//! in: the later suites below take the opposite order from the table — the
//! address is asked for BEFORE the record that would be seeded with it exists —
//! and assert, over every flow, that no two records the store holds end up
//! carrying one primary email address.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_account_account::state::{
    default_account_id, initial_primary_email, service_state,
};
use service_com_amazonaws_account_account::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The status an update carries while it waits for the one-time passcode.
const PENDING: &str = "PENDING";
/// The status an accepted update carries until its change has been applied.
const ACCEPTED: &str = "ACCEPTED";
/// The terminal status of an accepted update.
const COMPLETED: &str = "COMPLETED";

/// `PrimaryEmailAddress`'s modelled `@length`, from
/// `smithy_query(query="operation", operation="StartPrimaryEmailUpdate")`.
const PRIMARY_EMAIL_MIN: usize = 5;
const PRIMARY_EMAIL_MAX: usize = 64;

/// How long a test waits for a status the clock owes before giving up.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

/// A SigV4 credential scope naming this service: it is what routes a request to
/// the `account` service (its sigv4 signing name).
const SIGNED_FOR_US_EAST_1: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/account/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// Member account ids, each satisfying `AccountId`'s `^\d{12}$` and none of them
/// the caller's own account.
const MEMBER_ACCOUNTS: [&str; 8] = [
    "911111111111",
    "922222222222",
    "933333333333",
    "944444444444",
    "955555555555",
    "966666666666",
    "977777777777",
    "988888888888",
];

/// A passcode inside `Otp`'s modelled `^[a-zA-Z0-9]{6}$` that nothing sent,
/// for a case with no outstanding passcode to present.
const WELL_FORMED_PASSCODE: &str = "a1B2c3";

/// One emulator: a router carrying only this service over one shared engine,
/// plus that engine, so a case can read the passcode a start sent out of the
/// store — the way the owner of the mailbox reads it off the message.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// The passcode outstanding for the update of `account_id`, or `None` once
    /// it has been consumed (or while no update was ever started).
    fn passcode(&self, account_id: &str) -> Option<String> {
        let state = service_state(&*self.engine).expect("the account store is mounted");
        let store = state.lock();
        store
            .account(account_id)
            .and_then(|account| account.primary_email_update.as_ref())
            .and_then(|update| update.outstanding_otp().map(str::to_string))
    }

    /// Every account record this emulator holds, as (account id, the primary
    /// email address it carries as of now) — read where the addresses are
    /// persisted, which is what the one-address-one-account rule is about.
    fn held_primary_emails(&self) -> Vec<(String, String)> {
        let now = SystemTime::now();
        let state = service_state(&*self.engine).expect("the account store is mounted");
        let store = state.lock();
        store
            .accounts
            .values()
            .map(|account| {
                (
                    account.account_id.clone(),
                    account.settled_primary_email(now).to_string(),
                )
            })
            .collect()
    }
}

/// What the emulator answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The wire error code (`__type`), or the empty string on a success.
    fn error_code(&self) -> &str {
        self.body["__type"].as_str().unwrap_or_default()
    }

    /// The error `message`, or the empty string when there is none.
    fn message(&self) -> &str {
        self.body["message"].as_str().unwrap_or_default()
    }

    /// The `Status` member of a success body.
    fn reported_status(&self) -> &str {
        self.body["Status"].as_str().unwrap_or_default()
    }

    /// The `PrimaryEmail` member of a success body.
    fn primary_email(&self) -> &str {
        self.body["PrimaryEmail"].as_str().unwrap_or_default()
    }
}

async fn call(router: &Emulator, uri: &str, body: Value) -> Answer {
    let payload = body.to_string();
    let request = ::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", "account.us-east-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_US_EAST_1)
        .header("content-type", "application/json")
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "POST {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

async fn start(router: &Emulator, account_id: &str, address: &str) -> Answer {
    call(
        router,
        "/startPrimaryEmailUpdate",
        json!({"AccountId": account_id, "PrimaryEmail": address}),
    )
    .await
}

/// Accept the update of `account_id` with the passcode that update was started
/// with — the only value that confirms it — read out of the store. A case with
/// no outstanding passcode presents a well-formed value nothing sent, so the
/// request is still one the boundary admits.
async fn accept(router: &Emulator, account_id: &str, address: &str) -> Answer {
    let passcode = router
        .passcode(account_id)
        .unwrap_or_else(|| WELL_FORMED_PASSCODE.to_string());
    call(
        router,
        "/acceptPrimaryEmailUpdate",
        json!({"AccountId": account_id, "PrimaryEmail": address, "Otp": passcode}),
    )
    .await
}

async fn status_read(router: &Emulator, account_id: &str) -> Answer {
    call(
        router,
        "/getPrimaryEmailUpdateStatus",
        json!({"AccountId": account_id}),
    )
    .await
}

async fn primary_email_read(router: &Emulator, account_id: &str) -> Answer {
    call(router, "/getPrimaryEmail", json!({"AccountId": account_id})).await
}

/// Assert `answer` is the modelled HTTP 409 `ConflictException` — never the
/// scaffold's "not implemented" signal — and that the `@sensitive` address is
/// not echoed back.
fn assert_conflict(case: &str, answer: &Answer, address: &str) {
    assert_eq!(
        answer.status, 409,
        "case {case:?}: expected HTTP 409, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ConflictException",
        "case {case:?}: expected ConflictException, got {:?}",
        answer.body
    );
    assert!(
        !answer.message().contains("not implemented"),
        "case {case:?}: the refusal must be the modelled one, got {:?}",
        answer.message()
    );
    assert!(
        !answer.body.to_string().contains(address),
        "case {case:?}: a sensitive address must not be echoed, got {:?}",
        answer.body
    );
}

/// Assert a start was admitted and recorded as `PENDING`.
fn assert_started_pending(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: StartPrimaryEmailUpdate must answer 200, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.reported_status(),
        PENDING,
        "case {case:?}: expected a started update to be {PENDING}, got {:?}",
        answer.body
    );
}

/// The status the emulator reports for `account_id`, asserting the read
/// succeeded.
async fn reported_status(router: &Emulator, case: &str, account_id: &str) -> String {
    let answer = status_read(router, account_id).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the status read must answer 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.reported_status().to_string()
}

/// Poll the status of `account_id` until it leaves `transient`, and answer what
/// it settled to.
async fn settled_status(
    router: &Emulator,
    case: &str,
    account_id: &str,
    transient: &str,
) -> String {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        let status = reported_status(router, case, account_id).await;
        if status != transient {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: status is still {transient} after {SETTLE_TIMEOUT:?} — \
             a transition the clock owes must come due on its own"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// An address of exactly `length` characters, in the shape of an email address.
fn address_of_length(length: usize) -> String {
    let domain = "@example.com";
    format!("{}{domain}", "a".repeat(length - domain.len()))
}

/// How the address a row tests came to be in use.
#[derive(Debug, Clone, Copy)]
enum HeldBecause {
    /// It is the root address the holder's record came up with.
    SeededRootAddress,
    /// A completed update put it on the holder.
    AnUpdateCompleted,
    /// The holder's accepted update is still owed it.
    AnUpdateIsOwedIt,
}

/// One row: the account that holds the address, how it came to hold it, the
/// address, and the account that asks for it afterwards.
struct Row {
    case: &'static str,
    holder: &'static str,
    held_because: HeldBecause,
    address: String,
    rival: &'static str,
}

fn rows() -> Vec<Row> {
    let rows = vec![
        Row {
            case: "the seeded root address of another record",
            holder: MEMBER_ACCOUNTS[0],
            held_because: HeldBecause::SeededRootAddress,
            address: initial_primary_email(MEMBER_ACCOUNTS[0]),
            rival: MEMBER_ACCOUNTS[1],
        },
        Row {
            case: "an ordinary address a completed flow put on another account",
            holder: MEMBER_ACCOUNTS[2],
            held_because: HeldBecause::AnUpdateCompleted,
            address: "ada@example.com".to_string(),
            rival: MEMBER_ACCOUNTS[3],
        },
        Row {
            case: "the shortest address the bound admits, held by a completed flow",
            holder: MEMBER_ACCOUNTS[4],
            held_because: HeldBecause::AnUpdateCompleted,
            address: "a@b.c".to_string(),
            rival: MEMBER_ACCOUNTS[5],
        },
        Row {
            case: "the longest address the bound admits, owed to an accepted update",
            holder: MEMBER_ACCOUNTS[6],
            held_because: HeldBecause::AnUpdateIsOwedIt,
            address: address_of_length(PRIMARY_EMAIL_MAX),
            rival: MEMBER_ACCOUNTS[7],
        },
    ];
    assert_eq!(
        rows[2].address.chars().count(),
        PRIMARY_EMAIL_MIN,
        "the shortest row must sit on the published minimum"
    );
    assert_eq!(
        rows[3].address.chars().count(),
        PRIMARY_EMAIL_MAX,
        "the longest row must sit on the published maximum"
    );
    rows
}

/// Put `address` in use on `holder`, the way the row names, and leave the store
/// there.
async fn put_in_use(router: &Emulator, case: &str, row: &Row) {
    match row.held_because {
        HeldBecause::SeededRootAddress => {
            // The record has to EXIST for its seeded root address to be in use,
            // and naming it on a write that has nothing to do with email is
            // what brings it into existence.
            let named = call(
                router,
                "/putAccountName",
                json!({"AccountId": row.holder, "AccountName": "A member account"}),
            )
            .await;
            assert_eq!(
                named.status, 200,
                "case {case:?}: PutAccountName must answer 200 so the record exists, got {} \
                 with {:?}",
                named.status, named.body
            );
            assert_eq!(
                primary_email_read(router, row.holder).await.primary_email(),
                row.address,
                "case {case:?}: the holder must report the seeded address the row tests"
            );
        }
        HeldBecause::AnUpdateCompleted => {
            assert_started_pending(case, &start(router, row.holder, &row.address).await);
            let accepted = accept(router, row.holder, &row.address).await;
            assert_eq!(
                accepted.reported_status(),
                ACCEPTED,
                "case {case:?}: the holder's acceptance must answer {ACCEPTED}, got {:?}",
                accepted.body
            );
            assert_eq!(
                settled_status(router, case, row.holder, ACCEPTED).await,
                COMPLETED,
                "case {case:?}: the holder's update must settle to {COMPLETED}"
            );
            assert_eq!(
                primary_email_read(router, row.holder).await.primary_email(),
                row.address,
                "case {case:?}: the completed update must leave the holder holding the address"
            );
        }
        HeldBecause::AnUpdateIsOwedIt => {
            assert_started_pending(case, &start(router, row.holder, &row.address).await);
            let accepted = accept(router, row.holder, &row.address).await;
            assert_eq!(
                accepted.reported_status(),
                ACCEPTED,
                "case {case:?}: the holder's acceptance must answer {ACCEPTED}, got {:?}",
                accepted.body
            );
            // The change is still in flight — asserted, not assumed — so the
            // address is in use because its completion is OWED, not because any
            // record carries it yet.
            assert_eq!(
                reported_status(router, case, row.holder).await,
                ACCEPTED,
                "case {case:?}: the holder's change must still be in flight"
            );
        }
    }
}

/// A start for an address another account already holds, or is already owed, is
/// refused with the modelled HTTP 409 `ConflictException` and records nothing.
#[tokio::test]
async fn starting_an_update_for_an_address_in_use_conflicts() {
    for row in rows() {
        // Each row gets its own emulator, so the accounts it names are fresh.
        let router = Emulator::new();
        let case = row.case;
        put_in_use(&router, case, &row).await;

        let refused = start(&router, row.rival, &row.address).await;
        assert_conflict(case, &refused, &row.address);

        // The refused start recorded nothing — not even the account record a
        // stored update would have created — so the rival is still an account
        // this emulator has never been told about.
        let read = status_read(&router, row.rival).await;
        assert_eq!(
            read.status, 403,
            "case {case:?}: a refused start must record nothing, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.error_code(),
            "AccessDeniedException",
            "case {case:?}: expected AccessDeniedException, got {:?}",
            read.body
        );

        // The refusal is about the ADDRESS and not about the rival: an address
        // nobody holds is admitted for that very account.
        let free_address = format!("free-{}@example.com", row.rival);
        assert_started_pending(case, &start(&router, row.rival, &free_address).await);
        assert_eq!(
            reported_status(&router, case, row.rival).await,
            PENDING,
            "case {case:?}: the admitted start must be the update reported"
        );

        // And the refusal took nothing from the holder: it keeps the address,
        // once the change it was owed has been applied.
        let holder_address = match row.held_because {
            HeldBecause::AnUpdateIsOwedIt => {
                assert_eq!(
                    settled_status(&router, case, row.holder, ACCEPTED).await,
                    COMPLETED,
                    "case {case:?}: the holder's accepted update must still settle to {COMPLETED}"
                );
                primary_email_read(&router, row.holder)
                    .await
                    .primary_email()
                    .to_string()
            }
            _ => primary_email_read(&router, row.holder)
                .await
                .primary_email()
                .to_string(),
        };
        assert_eq!(
            holder_address, row.address,
            "case {case:?}: the holder must keep the address, got {holder_address:?}"
        );
    }
}

/// The address a merely PENDING request asked for is NOT in use: nothing has
/// been changed for it, so another account may still ask for it — and whichever
/// account confirms it first takes it, after which the other cannot even ask.
#[tokio::test]
async fn an_address_only_asked_for_is_not_in_use() {
    // (case, the first account to ask, the second, the address) — the rows
    // differ in the address's length and form.
    let cases: Vec<(&str, &str, &str, String)> = vec![
        (
            "an ordinary address",
            MEMBER_ACCOUNTS[0],
            MEMBER_ACCOUNTS[1],
            "grace@example.com".to_string(),
        ),
        (
            "the shortest address the bound admits",
            MEMBER_ACCOUNTS[2],
            MEMBER_ACCOUNTS[3],
            "a@b.c".to_string(),
        ),
        (
            "a tagged address on a two-label domain",
            MEMBER_ACCOUNTS[4],
            MEMBER_ACCOUNTS[5],
            "ada+aws@example.co.uk".to_string(),
        ),
    ];

    for (case, first, second, address) in cases {
        let router = Emulator::new();

        assert_started_pending(case, &start(&router, first, &address).await);
        // A pending request takes the address from nobody, so the second
        // account may ask for it too.
        assert_started_pending(case, &start(&router, second, &address).await);

        // The first account confirms it, which is what puts it in use.
        let accepted = accept(&router, first, &address).await;
        assert_eq!(
            accepted.reported_status(),
            ACCEPTED,
            "case {case:?}: the first acceptance must answer {ACCEPTED}, got {:?}",
            accepted.body
        );
        assert_eq!(
            settled_status(&router, case, first, ACCEPTED).await,
            COMPLETED,
            "case {case:?}: the first update must settle to {COMPLETED}"
        );

        // From there the second account cannot ask for it again, and the
        // request it had recorded is left exactly where the refusal found it.
        let refused = start(&router, second, &address).await;
        assert_conflict(case, &refused, &address);
        assert_eq!(
            reported_status(&router, case, second).await,
            PENDING,
            "case {case:?}: a refused start must leave the recorded update {PENDING}"
        );
    }
}

/// An account asking again for the address it ALREADY holds, or is already owed,
/// is no conflict: that address is taken from nobody. The refusal is reserved
/// for an address ANOTHER account holds.
#[tokio::test]
async fn an_account_may_ask_again_for_the_address_it_holds() {
    let router = Emulator::new();

    // (case, the account, the address it ends up holding)
    let cases: Vec<(&str, &str, String)> = vec![
        (
            "the seeded root address of its own record",
            MEMBER_ACCOUNTS[0],
            initial_primary_email(MEMBER_ACCOUNTS[0]),
        ),
        (
            "an ordinary address a completed flow put there",
            MEMBER_ACCOUNTS[1],
            "ada@example.com".to_string(),
        ),
        (
            "the longest address the bound admits",
            MEMBER_ACCOUNTS[2],
            address_of_length(PRIMARY_EMAIL_MAX),
        ),
    ];

    for (case, account_id, address) in cases {
        // The record has to exist before its own address can be read, and
        // naming it on a write that has nothing to do with email is what brings
        // it into existence.
        let named = call(
            &router,
            "/putAccountName",
            json!({"AccountId": account_id, "AccountName": "A member account"}),
        )
        .await;
        assert_eq!(
            named.status, 200,
            "case {case:?}: PutAccountName must answer 200 so the record exists, got {} with {:?}",
            named.status, named.body
        );

        let held = primary_email_read(&router, account_id)
            .await
            .primary_email()
            .to_string();
        if held != address {
            // The account does not hold the address yet: one whole flow puts it
            // there.
            assert_started_pending(case, &start(&router, account_id, &address).await);
            let accepted = accept(&router, account_id, &address).await;
            assert_eq!(
                accepted.reported_status(),
                ACCEPTED,
                "case {case:?}: the acceptance must answer {ACCEPTED}, got {:?}",
                accepted.body
            );
            assert_eq!(
                settled_status(&router, case, account_id, ACCEPTED).await,
                COMPLETED,
                "case {case:?}: the update must settle to {COMPLETED}"
            );
        }
        assert_eq!(
            primary_email_read(&router, account_id).await.primary_email(),
            address,
            "case {case:?}: the account must hold the address before it asks for it again"
        );

        // Asking again for the address it already holds is admitted.
        assert_started_pending(case, &start(&router, account_id, &address).await);
        assert_eq!(
            reported_status(&router, case, account_id).await,
            PENDING,
            "case {case:?}: the re-asked update must be the one reported"
        );
    }
}

/// Account ids this emulator holds no record of until something names them,
/// spanning the shapes `AccountId`'s `^\d{12}$` admits: leading zeros, twelve
/// distinct digits and the top of the range. None of them is the caller's own
/// account.
const UNTOUCHED_ACCOUNTS: [&str; 3] = ["000000000001", "123456789012", "999999999999"];

/// The address `account_id` reports. `GetPrimaryEmail` models `AccountId` as
/// required and the member may not name the caller's OWN account, so that one
/// record's address is read where it is persisted instead.
async fn held_address(router: &Emulator, account_id: &str) -> String {
    if account_id == default_account_id() {
        return router
            .held_primary_emails()
            .into_iter()
            .find(|(held_by, _)| held_by == account_id)
            .map(|(_, address)| address)
            .expect("the store holds the caller's own account record");
    }
    let answer = primary_email_read(router, account_id).await;
    assert_eq!(
        answer.status, 200,
        "the primary email read of {account_id} must answer 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.primary_email().to_string()
}

/// Bring the record of `account_id` into existence, by naming it on a write that
/// has nothing to do with email. The caller's own account is one the store holds
/// from the start, so there is nothing to touch for it.
async fn bring_into_existence(router: &Emulator, case: &str, account_id: &str) {
    if account_id == default_account_id() {
        return;
    }
    let named = call(
        router,
        "/putAccountName",
        json!({"AccountId": account_id, "AccountName": "A member account"}),
    )
    .await;
    assert_eq!(
        named.status, 200,
        "case {case:?}: PutAccountName must answer 200 so the record of {account_id} exists, \
         got {} with {:?}",
        named.status, named.body
    );
}

/// Run one WHOLE update flow on `account_id` for `address` — the start, the
/// acceptance carrying the passcode that start sent, and the completion the
/// clock owes — and assert it leaves the account holding the address.
async fn complete_update(router: &Emulator, case: &str, account_id: &str, address: &str) {
    assert_started_pending(case, &start(router, account_id, address).await);
    let accepted = accept(router, account_id, address).await;
    assert_eq!(
        accepted.reported_status(),
        ACCEPTED,
        "case {case:?}: the acceptance for {account_id} must answer {ACCEPTED}, got {:?}",
        accepted.body
    );
    assert_eq!(
        settled_status(router, case, account_id, ACCEPTED).await,
        COMPLETED,
        "case {case:?}: the update of {account_id} must settle to {COMPLETED}"
    );
    assert_eq!(
        held_address(router, account_id).await,
        address,
        "case {case:?}: the completed update must leave {account_id} holding the address"
    );
}

/// Assert no two account records this emulator holds carry one primary email
/// address — the invariant the in-use refusal exists to keep, asserted over the
/// whole store rather than over the two accounts a case names.
fn assert_each_address_held_once(case: &str, router: &Emulator) {
    let held = router.held_primary_emails();
    let mut by_address: BTreeMap<&str, &str> = BTreeMap::new();
    for (account_id, address) in &held {
        let first_holder = by_address.insert(address.as_str(), account_id.as_str());
        assert!(
            first_holder.is_none(),
            "case {case:?}: accounts {:?} and {account_id:?} both hold one primary email \
             address; the store holds {held:?}",
            first_holder.unwrap_or_default()
        );
    }
}

/// The root address an account id names is in use by THAT account from the
/// start, whether or not this emulator has materialised its record yet: a
/// change asking for it is refused with the modelled HTTP 409
/// `ConflictException`, so the first touch of that account cannot seed a second
/// record with an address somebody else was handed meanwhile.
#[tokio::test]
async fn the_root_address_an_account_is_owed_is_in_use_before_its_record_exists() {
    // (case, the account the address belongs to, the account that asks for it)
    let rows: [(&str, &str, &str); 4] = [
        (
            "an id with leading zeros this emulator has never been told about",
            UNTOUCHED_ACCOUNTS[0],
            MEMBER_ACCOUNTS[0],
        ),
        (
            "an id of twelve distinct digits this emulator has never been told about",
            UNTOUCHED_ACCOUNTS[1],
            MEMBER_ACCOUNTS[1],
        ),
        (
            "an id at the top of the range this emulator has never been told about",
            UNTOUCHED_ACCOUNTS[2],
            MEMBER_ACCOUNTS[2],
        ),
        (
            "the caller's own account, whose record the store holds from the start",
            default_account_id(),
            MEMBER_ACCOUNTS[3],
        ),
    ];

    for (case, owner, rival) in rows {
        // Each row gets its own emulator, so the accounts it names are fresh.
        let router = Emulator::new();
        let address = initial_primary_email(owner);

        let refused = start(&router, rival, &address).await;
        assert_conflict(case, &refused, &address);

        // The refused start recorded nothing, so the rival is still an account
        // this emulator has never been told about.
        let read = status_read(&router, rival).await;
        assert_eq!(
            read.status, 403,
            "case {case:?}: a refused start must record nothing, got {} with {:?}",
            read.status, read.body
        );

        // The owner keeps the address its id names: the first touch of its
        // record reports exactly that address, and no other record carries it.
        bring_into_existence(&router, case, owner).await;
        assert_eq!(
            held_address(&router, owner).await,
            address,
            "case {case:?}: {owner} must hold the root address its own id names"
        );
        assert_each_address_held_once(case, &router);

        // And the owner can still re-affirm the address it holds, which is no
        // conflict at all — the symptom a second holder of that address would
        // have produced. (The caller's own account cannot be the target of a
        // change, so the re-affirmation applies to a member account.)
        if owner != default_account_id() {
            assert_started_pending(case, &start(&router, owner, &address).await);
            assert_eq!(
                reported_status(&router, case, owner).await,
                PENDING,
                "case {case:?}: {owner} must be able to re-affirm the address it holds"
            );
        }

        // The refusal is about the ADDRESS and not about the rival: the root
        // address the RIVAL's own id names is admitted for it, and so is an
        // address nobody is owed.
        let rival_router = Emulator::new();
        assert_started_pending(
            case,
            &start(&rival_router, rival, &initial_primary_email(rival)).await,
        );
        assert_started_pending(
            case,
            &start(&rival_router, rival, &format!("free-{rival}@example.com")).await,
        );
    }
}

/// An address that only RESEMBLES a root address this emulator mints is nobody's
/// and is admitted: the form is the local part, exactly twelve digits and the
/// domain (the source is `state::initial_primary_email`), so a middle part of
/// another length or carrying a non-digit, another domain and another local part
/// are all free addresses a whole flow can take.
#[tokio::test]
async fn an_address_that_only_resembles_a_root_address_is_free() {
    // (case, the address) — every row is inside `PrimaryEmailAddress`'s
    // `@length(min: 5, max: 64)` and satisfies the model's email expression.
    let rows: [(&str, &str); 6] = [
        ("eleven digits in the middle", "root+12345678901@example.com"),
        (
            "thirteen digits in the middle",
            "root+1234567890123@example.com",
        ),
        (
            "twelve characters in the middle that are not all digits",
            "root+00000000000a@example.com",
        ),
        (
            "twelve digits in the middle, on another domain",
            "root+100000000001@example.org",
        ),
        (
            "twelve digits in the middle, under another local part",
            "admin+100000000001@example.com",
        ),
        (
            "twelve digits in the middle, with nothing before them",
            "100000000001@example.com",
        ),
    ];

    for (case, address) in rows {
        let router = Emulator::new();
        let taker = MEMBER_ACCOUNTS[0];
        assert!(
            address.chars().count() >= PRIMARY_EMAIL_MIN
                && address.chars().count() <= PRIMARY_EMAIL_MAX,
            "case {case:?}: the row must sit inside the published bounds, got {} characters",
            address.chars().count()
        );

        complete_update(&router, case, taker, address).await;
        assert_each_address_held_once(case, &router);
    }
}

/// A root address the account it names has MOVED OFF is free: nothing seeds an
/// existing record again, so the address a completed update left behind is
/// available to another account, exactly as any other address a completed update
/// leaves behind.
#[tokio::test]
async fn a_root_address_its_own_account_moved_off_may_be_taken() {
    // (case, the account that moves off its root address, the address it moves
    // to, the account that takes the address it left)
    let rows: [(&str, &str, String, &str); 2] = [
        (
            "an ordinary address to move to",
            MEMBER_ACCOUNTS[0],
            "ada@example.com".to_string(),
            MEMBER_ACCOUNTS[1],
        ),
        (
            "the longest address the bound admits to move to",
            MEMBER_ACCOUNTS[2],
            address_of_length(PRIMARY_EMAIL_MAX),
            MEMBER_ACCOUNTS[3],
        ),
    ];

    for (case, owner, moved_to, taker) in rows {
        let router = Emulator::new();
        let left_behind = initial_primary_email(owner);

        bring_into_existence(&router, case, owner).await;
        assert_eq!(
            held_address(&router, owner).await,
            left_behind,
            "case {case:?}: {owner} must start out holding the address its own id names"
        );

        complete_update(&router, case, owner, &moved_to).await;

        // The address the owner left is held by nobody now, so the taker may
        // have it — and both records end up with one address each.
        complete_update(&router, case, taker, &left_behind).await;
        assert_eq!(
            held_address(&router, owner).await,
            moved_to,
            "case {case:?}: {owner} must keep the address it moved to"
        );
        assert_each_address_held_once(case, &router);
    }
}

/// The root address an id names is that account's OWN, so asking for it is no
/// conflict even before this emulator holds a record of the account: the address
/// is taken from nobody.
#[tokio::test]
async fn an_account_may_ask_for_the_root_address_its_own_id_names() {
    for account_id in UNTOUCHED_ACCOUNTS {
        let router = Emulator::new();
        let case = format!("the root address of {account_id}, before its record exists");
        let address = initial_primary_email(account_id);

        complete_update(&router, &case, account_id, &address).await;
        assert_each_address_held_once(&case, &router);
    }
}
