Issue-Severity: LOW
Issue-Gap-Class: validation
Issue-Commit: 3a71fbf666403ef651a16a3f891bc8ef488168a9

# CreateUser persists attribute extensions under any name, though the model states only aws:identitystore:enterprise is supported

_Filed: 2026-11-26T18:29:46Z_
_Operation: CreateUser | Class: validation_

> _Note: this issue was identified against the code base at commit `3a71fbf666403ef651a16a3f891bc8ef488168a9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateUserRequest$Extensions` is documented as supporting exactly ONE extension name. The handler takes the whole map as it arrived and stores it verbatim, with no check of the keys at all (`extensions: input.extensions.unwrap_or_default()`), so a create naming `aws:identitystore:other` (or any other name the broader modelled `ExtensionName` pattern `^aws:identitystore:[a-z]{1,20}$` admits) answers 200 and the emulator persists extension data under a name the model says is not supported. `DescribeUser`/`ListUsers` then report that extension back when a request asks for it, so the unsupported name is observable state rather than a refused request.

Nothing anywhere in the crate reads the supported extension name: `grep -rn 'enterprise' services/service-com-amazonaws-identitystore-awsidentitystore/src` matches only the two generated doc headers (create_user.rs:65, describe_user.rs:56) and never any code. Note the model is itself looser than its prose — the `ExtensionName` `@pattern` admits more names than the sentence does — which is why this is filed as the narrow write-door gap (a create storing an unsupported extension name) and at LOW severity; the read door is pinned by an existing test row that expects 200 for a request naming `aws:identitystore:other`.

## Evidence

Smithy — `smithy_query(query="doc",shape="CreateUserRequest",member="Extensions")`:
```text
A map with additional attribute extensions for the user. Each map key corresponds to an extension name, while map values represent extension data in Document type (not supported by Java V1, Go V1 and older versions of the CLI). aws:identitystore:enterprise is the only supported extension name.
```
The same sentence is quoted verbatim in the handler's own modelled-contract header, services/service-com-amazonaws-identitystore-awsidentitystore/src/ops/create_user.rs:62-65:
```rust
//! * `Extensions` (Extensions, len 1..10) — A map with additional attribute extensions for
//!   the user. Each map key corresponds to an extension name, while map values represent
//!   extension data in Document type (not supported by Java V1, Go V1 and older versions of
//!   the CLI). aws:identitystore:enterprise is the only supported extension name.
```
What the handler does with it — the map is stored unexamined, services/service-com-amazonaws-identitystore-awsidentitystore/src/ops/create_user.rs:315:
```rust
            extensions: input.extensions.unwrap_or_default(),
```
and the stored map is handed straight back to a reader that asks for the name, services/service-com-amazonaws-identitystore-awsidentitystore/src/state.rs:386-395 (`User::requested_extensions`):
```rust
        let names = requested?;
        let selected: HashMap<String, ::aws_smithy_types::Document> = names
            .iter()
            .filter_map(|name| {
                self.extensions
                    .get(name)
                    .map(|value| (name.clone(), value.clone()))
            })
```
Modelled key constraint, `smithy_query(query="shape",shape="Extensions")`: `"key": {"target": "ExtensionName", "length": {"min": 1, "max": 50}, "pattern": "^aws:identitystore:[a-z]{1,20}$"}` — the only check any layer applies, and it admits names the sentence above excludes.

Test coverage: the one extension suite, services/service-com-amazonaws-identitystore-awsidentitystore/tests/all/list_users.rs:618-651, creates ONLY the supported name and uses `aws:identitystore:other` solely as a requested name on the read side:
```rust
    // `aws:identitystore:enterprise` is the extension name the operation's own doc names
    // as the only supported one; the modelled `ExtensionName` pattern is
    // `^aws:identitystore:[a-z]{1,20}$`
    let extension = "aws:identitystore:enterprise";
...
        (
            "an extension the user does not carry",
            Some(vec!["aws:identitystore:other"]),
            None,
        ),
```
so no test creates a user carrying an unsupported extension name, and nothing in the crate justifies storing one.

## Steps to Reproduce

1. Boot the emulator and use the seeded IdentityStoreId (`state::SEED_IDENTITY_STORE_ID`, `d-9067f3a1c2`).
2. POST / with `X-Amz-Target: AWSIdentityStore.CreateUser` and body `{"IdentityStoreId":"d-9067f3a1c2","UserName":"alice","Extensions":{"aws:identitystore:other":{"CostCenter":"4100"}}}` → HTTP 200 with a minted UserId (expected: the request names an extension the model states is not supported).
3. POST / with `X-Amz-Target: AWSIdentityStore.DescribeUser` and body `{"IdentityStoreId":"d-9067f3a1c2","UserId":"<id>","Extensions":["aws:identitystore:other"]}` → HTTP 200 with `"Extensions": {"aws:identitystore:other": {"CostCenter":"4100"}}`, proving the unsupported extension name was persisted and is reported.
4. Repeat step 2 with `"aws:identitystore:enterprise"` → also 200, i.e. the handler treats the supported and the unsupported name identically.
