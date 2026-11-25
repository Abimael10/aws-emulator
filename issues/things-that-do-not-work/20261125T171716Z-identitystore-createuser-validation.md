Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: f9465bc2186e307f05476a9cadaf16956fa09a02

# CreateUser accepts the reserved UserName values the model says cannot be used

_Filed: 2026-11-25T17:17:16Z_
_Operation: CreateUser | Class: validation_

> _Note: this issue was identified against the code base at commit `f9465bc2186e307f05476a9cadaf16956fa09a02`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The modelled documentation of `CreateUserRequest$UserName` states that two names are reserved and cannot be used. The handler applies no such rule: the only name check it makes is the per-store uniqueness claim against `user_name_index`, so a `CreateUser` naming a reserved value is stored like any other and a later `DescribeUser`/`GetUserId` reports it. A caller can therefore occupy a name the model declares unusable, and the uniqueness claim then blocks nothing else from being created under it either.

## Evidence

Smithy — `smithy_query(query="doc",shape="CreateUserRequest",member="UserName")`, as reproduced verbatim in the handler's own modelled-contract header, services/service-com-amazonaws-identitystore-awsidentitystore/src/ops/create_user.rs:96-101:
```text
//! * `UserName` (UserName, len 1..128, pattern ^[\p{L}\p{M}\p{S}\p{N}\p{P}]+$) — A unique
//!   string used to identify the user. ... Administrator and AWSAdministrators are reserved names and
//!   can't be used for users or groups.
```

The handler's only check on the name, src/ops/create_user.rs:256-270:
```rust
// A `UserName` is a uniqueness claim within the identity store, so a second
// create naming one that is taken conflicts rather than storing a duplicate.
if input
    .user_name
    .as_deref()
    .is_some_and(|user_name| store.user_name_index.contains_key(user_name))
{
    return Err(CreateUserErrorModeled::ConflictException(...));
}
```
and the value is then persisted unconditionally (create_user.rs:278 `user_name: input.user_name,`, :317-319 `store.user_name_index.insert(user_name, user_id.clone());`). `grep -n 'Administrator' src/` in this crate matches only the two doc headers (create_user.rs:100, create_group.rs:56) — no code path reads the reserved names, and no test sends one.

## Steps to Reproduce

1. Boot the emulator and resolve the seeded IdentityStoreId.
2. Call CreateUser with `UserName: "Administrator"` → HTTP 200 with a minted UserId (the model says this name "can't be used").
3. Call DescribeUser for that UserId → `UserName: "Administrator"` is reported back as stored.
4. Repeat with `UserName: "AWSAdministrators"` → also accepted.
