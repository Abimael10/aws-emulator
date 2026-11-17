Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 09a4de48f58372094bfcb9a0bb2c4d192074b72e

# A sign-in held in this store loses every assigned account the store has no AccountRecord for, so an assigned account is reported as nonexistent (404) while the federated path reports the very same case

_Filed: 2026-11-14T13:49:40Z_
_Operation: ListAccountRoles | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `09a4de48f58372094bfcb9a0bb2c4d192074b72e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ComAmazonawsSsoSwbportalserviceState::signed_in` resolves a sign-in from one of two sources, and the two answer the SAME question — "the account this assignment names has no account record" — in opposite ways.

* The federated source (`sign_in::named_accounts`) KEEPS the account and reports it with `accountName`/`emailAddress` absent, because both members are optional in the model.
* The store's own source (`ComAmazonawsSsoSwbportalserviceState::seeded_sign_in`) DROPS the account entirely: `let record = self.accounts.get(account_id)?;` inside a `filter_map`, so an assignment the caller of `put_session` made is silently discarded unless `put_account` was also called for that account id.

Both construct the same `crate::state::Account`, whose own member documentation states the rule the first path follows and the second one makes unreachable: `account_name` is "`None` when it holds no record of this account at all … An account nothing in the emulator has a record of has no name to report, and reporting none is what that is". `seeded_sign_in` never produces that documented value — it removes the account instead.

Symptoms of this one cause, all on a sign-in whose assignment DOES name the account:

* `ListAccountRoles` answers 404 `ResourceNotFoundException` ("no account {id} is assigned to this sign-in session") for an account that IS assigned to it — `signed_in.account(&input.account_id)` is `None` only because `seeded_sign_in` dropped it (src/ops/list_account_roles.rs:181-191).
* `ListAccounts` omits the account from `accountList`, although the operation is documented as "Lists all AWS accounts assigned to the user" and `AccountInfo` needs nothing but the `accountId` the store already holds on the assignment.
* `GetRoleCredentials` answers 404 for every role of that assignment (src/ops/get_role_credentials.rs:184-199).

The skip is asserted by one row of `tests/all/portal_session.rs`, whose stated reason is "which there is nothing to report for it" — a reason the sibling path and the model both contradict: the sibling suite requires that exact account to be reported (`"every assigned account must be reported"`), and `accountId` is reportable on its own because `accountName`/`emailAddress` carry no `@required`.

## Evidence

Smithy, `ListAccounts` (smithy_query query="doc" shape="ListAccounts"):
> "Lists all AWS accounts assigned to the user. These AWS accounts are assigned by the administrator of the account."

and `AccountInfo` (smithy_query query="shape" shape="AccountInfo") — nothing but optional members beside the id:
> "accountId": {"target": "AccountIdType", "doc": "The identifier of the AWS account that is assigned to the user."}, "accountName": {"target": "AccountNameType", …}, "emailAddress": {"target": "EmailAddressType", …}

The crate's own rule for this exact case — services/service-com-amazonaws-sso-swbportalservice/src/state.rs:160-171:
```rust
    pub account_id: String,
    /// The account's display name, as the emulator's records of the ACCOUNT hold it, and
    /// `None` when it holds no record of this account at all.
    ///
    /// `AccountInfo$accountName` is an optional member, and the portal is not where an
    /// account's name lives: it reports the name the organization's own records carry (see
    /// [`sign_in`]). An account nothing in the emulator has a record of has no name to
    /// report, and reporting none is what that is — never a name derived from the id.
    pub account_name: Option<String>,
```

Side 1 — the federated path HONOURS it, src/state/sign_in.rs:210-218:
```rust
            let member = organization
                .as_ref()
                .and_then(|organization| organization.accounts.get(&account_id));
            let account = Account {
                account_id: account_id.clone(),
                account_name: member.map(|member| member.name.clone()),
                email_address: member.map(|member| member.email.clone()),
                roles,
            };
```

Side 2 — the store's own path DROPS the account, src/state/sign_in.rs:282-297:
```rust
            accounts: session
                .assignments
                .iter()
                .filter_map(|(account_id, roles)| {
                    let record = self.accounts.get(account_id)?;
                    Some((
                        account_id.clone(),
```
(the `?` on line 286 is the whole defect: no record ⇒ the assigned account vanishes)

Where that loss becomes a 404 for an assigned account — src/ops/list_account_roles.rs:179-191:
```rust
        // An account the presented session is not assigned to does not exist as far as
        // that session is concerned.
        let Some(account) = signed_in.account(&input.account_id) else {
            return Err(ListAccountRolesErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    message: Some(format!(
                        "no account {} is assigned to this sign-in session",
                        input.account_id
                    )),
```

The two suites pin opposite answers to the one condition:
* federated — tests/all/minted_sign_in.rs:661-676:
```rust
        .find(|account| text(account, "accountId") == unknown_to_the_organization.account_id)
        .unwrap_or_else(|| panic!("every assigned account must be reported: {body}"));
    assert_eq!(
        (
            reported.get("accountName"),
            reported.get("emailAddress")
        ),
        (None, None),
        "an account the emulator holds no record of carries no name and no email, got \
         {reported}"
```
* seeded — tests/all/portal_session.rs:267-294, whose stated reason is what the model and the sibling contradict:
```rust
    // Rows differ in how many accounts a session is assigned to (none, one, several,
    // all) and include an assignment naming an account the service holds no record of,
    // which there is nothing to report for.
…
        let mut expected: Vec<&str> = assigned
            .iter()
            .copied()
            .filter(|id| every_account().contains(id))
            .collect();
```

The `signed_in` doc states the skip as a design note (src/state/sign_in.rs:237-241, "skipping an assignment whose account this store holds no record of, since there is nothing to report for it") — i.e. the crate carries BOTH rules in one module, and they disagree.

## Steps to Reproduce

1. Stand the service up as tests/all/portal_session.rs:85-99 does, but seed only SOME accounts: `store.put_account("111111111111", "Production", "production@example.com")` and nothing for `999999999999`.
2. Seed a sign-in whose assignment names both: `store.put_session("tok", "user@example.com", [("111111111111", vec!["ReadOnly"]), ("999999999999", vec!["ReadOnly"])], ttl)` — `put_session` is documented (src/state.rs:303-312) as the seam an administrator's assignments arrive through, and it accepts the assignment without complaint.
3. `GET /assignment/accounts` with `x-amz-sso_bearer_token: tok`. Observed: `accountList` holds `111111111111` only; `999999999999` is absent. Expected per the Smithy sentence "Lists all AWS accounts assigned to the user" and src/state.rs:161-167: it is reported with its `accountId` and no `accountName`/`emailAddress`, exactly as the federated path does (tests/all/minted_sign_in.rs:661-676).
4. `GET /assignment/roles?account_id=999999999999` with the same token. Observed: 404 `ResourceNotFoundException` "no account 999999999999 is assigned to this sign-in session" — although the sign-in's assignment names that account and grants `ReadOnly` in it. Cause: src/state/sign_in.rs:286 dropped it before src/ops/list_account_roles.rs:181 looked.
5. `GET /federation/credentials?account_id=999999999999&role_name=ReadOnly` with the same token. Observed: 404 `ResourceNotFoundException` for a role the seeded assignment grants (src/ops/get_role_credentials.rs:184-199).
6. Contrast the other source of the identical condition: mount the portal beside the OIDC, IAM Identity Center and Organizations services as tests/all/minted_sign_in.rs:107-119 does and assign a permission set on an account Organizations holds no record of — `ListAccounts` reports that account with `accountName`/`emailAddress` absent (tests/all/minted_sign_in.rs:668-676) and `ListAccountRoles` for it answers 200 (tests/all/minted_sign_in.rs:678-684). One condition, two opposite answers inside one module.
