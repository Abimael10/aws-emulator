Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: d64d8a3b93e2dce2fbfb15e5f8c9c90ac149db57

# A record created on first touch is seeded with its root address unconditionally, so two accounts can end up holding the same primary email the crate's own rule forbids

_Filed: 2026-11-21T00:11:47Z_
_Operation: StartPrimaryEmailUpdate | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `d64d8a3b93e2dce2fbfb15e5f8c9c90ac149db57`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states one rule for primary email addresses: an address another account already holds is NOT available, and a change to it is refused 409 (`primary_email_in_use_by_other` / `primary_email_already_in_use`, decided by both `StartPrimaryEmailUpdate` and `AcceptPrimaryEmailUpdate`). The rule is only ever asked about the records that exist at the instant of the request.

The record CREATION path never asks it. `ComAmazonawsAccountAccountState::account_mut` creates a record on first touch and `Account::new` seeds `primary_email: initial_primary_email(&account_id)` — `root+<account_id>@example.com` — without consulting `primary_email_in_use_by_other`, even though the state module already holds that helper and the six create-on-first-touch handlers (PutAccountName, PutAlternateContact, PutContactInformation, EnableRegion, DisableRegion, StartPrimaryEmailUpdate) all reach creation through it.

So the two halves disagree about the same address. `StartPrimaryEmailUpdate`/`AcceptPrimaryEmailUpdate` let account B take `root+<A>@example.com` while this emulator holds no record of A (nobody holds it, so there is no conflict), and B's accepted update puts it on B once the clock settles. The FIRST touch of account A afterwards — any of the six creating operations, e.g. `PutAccountName{AccountId: A}` — then seeds A with that very address. From that point two account records carry the identical `primary_email`: `GetPrimaryEmail` reports the same root user email address for A and for B, and a third account asking for it is refused "the email address is already in use as the primary email address of an account" — the exact state the rule exists to exclude. The creation also silently changes B's own answers: an address B holds is now also "in use by another account", so B can no longer re-affirm it (`primary_email_in_use_by_other` skips only the target, and A is not the target).

The crate's own suite pins the OPPOSITE order only (the holder record is created first, and the rival is then refused), and its comment says exactly why the order matters: "The record has to EXIST for its seeded root address to be in use". Nothing covers a record created after its seeded address was taken.

## Evidence

The rule, stated once — services/service-com-amazonaws-account-account/src/state.rs:1295-1322:

```
    /// Whether `address` is already in use by an account OTHER than
    /// `account_id`: another account holds it, or another account's update is
    /// ACCEPTED for it, so its completion is already owed.
...
    /// Settle first ([`settle_primary_email_updates`](Self::settle_primary_email_updates)):
    /// an address a completed update put on an account is in use from the
    /// instant that completion came due.
    pub fn primary_email_in_use_by_other(&self, address: &str, account_id: &str) -> bool {
        self.accounts
            .values()
            .filter(|account| account.account_id != account_id)
            .any(|account| {
                account.primary_email == address
```

and the refusal it earns — state.rs:1709-1716:

```
pub fn primary_email_already_in_use() -> ConflictException {
    ConflictException {
        message: "The request could not be processed because the email address is already in \
                  use as the primary email address of an account"
            .to_string(),
```

The creation path that never asks it — state.rs:1275-1284:

```
    pub fn account_mut(&mut self, account_id: &str, now: SystemTime) -> Option<&mut Account> {
        if !self.accounts.contains_key(account_id) && self.accounts.len() >= MAX_ACCOUNTS {
            return None;
        }
        Some(
            self.accounts
                .entry(account_id.to_string())
                .or_insert_with(|| Account::new(account_id.to_string(), now)),
        )
    }
```

and state.rs:1064-1069:

```
    pub fn new(account_id: String, now: SystemTime) -> Self {
        Self {
            account_name: initial_account_name(&account_id),
            contact_information: initial_contact_information(&account_id),
            primary_email: initial_primary_email(&account_id),
```

with state.rs:378-380:

```
pub fn initial_primary_email(account_id: &str) -> String {
    format!("{INITIAL_PRIMARY_EMAIL_LOCAL_PART}{account_id}{INITIAL_PRIMARY_EMAIL_DOMAIN}")
}
```

The handler that enforces the rule and then creates through that same unchecked path — src/ops/start_primary_email_update.rs:238-249:

```
        if store.primary_email_in_use_by_other(&input.primary_email, &account_id) {
            return Err(StartPrimaryEmailUpdateErrorModeled::ConflictException(
                crate::state::primary_email_already_in_use(),
            )
            .into());
        }
        let Some(account) = store.account_mut(&account_id, now) else {
```

and the sibling creating handler used in the repro — src/ops/put_account_name.rs:199-205:

```
        let Some(account) = store.account_mut(&account_id, now) else {
            return Err(PutAccountNameErrorModeled::AccessDeniedException(
                crate::state::account_store_full(&account_id),
            )
            .into());
        };
        account.account_name = input.account_name;
```

Test coverage, which pins only the other order — tests/all/primary_email_update_address_in_use.rs:296-302:

```
        Row {
            case: "the seeded root address of another record",
            holder: MEMBER_ACCOUNTS[0],
            held_because: HeldBecause::SeededRootAddress,
            address: initial_primary_email(MEMBER_ACCOUNTS[0]),
            rival: MEMBER_ACCOUNTS[1],
        },
```

and :342-350, whose comment states the dependence on creation order:

```
        HeldBecause::SeededRootAddress => {
            // The record has to EXIST for its seeded root address to be in use,
            // and naming it on a write that has nothing to do with email is
            // what brings it into existence.
            let named = call(
                router,
                "/putAccountName",
                json!({"AccountId": row.holder, "AccountName": "A member account"}),
            )
```

No row of that table, and no case in tests/all/primary_email_update_self_reaffirmation.rs or tests/all/primary_email_read.rs, creates a record AFTER another account has taken the address `initial_primary_email` would seed it with.

## Steps to Reproduce

1. Boot a router carrying only EmulatedService over a fresh engine::InMemoryEngine, as tests/all/primary_email_update_address_in_use.rs does (request authentication off).
2. Pick two member ids the store holds no record of, A = "200000000001" and B = "200000000002" (neither is engine::emulator_account_id()).
3. POST /startPrimaryEmailUpdate {"AccountId": "200000000002", "PrimaryEmail": "root+200000000001@example.com"} — 200 PENDING: nothing holds that address, because this emulator holds no record of A yet. (The address is exactly what state::initial_primary_email("200000000001") mints, and it satisfies PrimaryEmailAddress's @length(5,64) and the model's email expression.)
4. Read B's outstanding one-time passcode out of the store through crate::state::service_state (the way tests/all/one_time_passcode_confirmation.rs does) and POST /acceptPrimaryEmailUpdate {"AccountId": "200000000002", "PrimaryEmail": "root+200000000001@example.com", "Otp": <that passcode>} — 200 ACCEPTED.
5. Poll POST /getPrimaryEmailUpdateStatus {"AccountId": "200000000002"} until it reports COMPLETED (PRIMARY_EMAIL_UPDATE_COMPLETION is 500 ms), then POST /getPrimaryEmail {"AccountId": "200000000002"} — it reports root+200000000001@example.com, so that address is now in use per primary_email_in_use_by_other.
6. POST /putAccountName {"AccountId": "200000000001", "AccountName": "A member account"} — 200. account_mut creates the record and Account::new seeds it with initial_primary_email("200000000001"), i.e. the address B already holds; no in-use check runs.
7. POST /getPrimaryEmail {"AccountId": "200000000001"} and POST /getPrimaryEmail {"AccountId": "200000000002"} — both report root+200000000001@example.com: two account records now hold one primary email address.
8. POST /startPrimaryEmailUpdate {"AccountId": "200000000002", "PrimaryEmail": "root+200000000001@example.com"} — now refused 409 "the email address is already in use as the primary email address of an account", even though it is the address account B itself already holds (tests/all/primary_email_update_self_reaffirmation.rs pins that re-affirming one's own address must NOT be that conflict).
