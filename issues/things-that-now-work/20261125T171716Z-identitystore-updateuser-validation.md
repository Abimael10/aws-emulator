Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: f9465bc2186e307f05476a9cadaf16956fa09a02

# UpdateUser stores attribute values the modelled @pattern forbids, which UpdateGroup and CreateUser both refuse

_Filed: 2026-11-25T17:17:16Z_
_Operation: UpdateUser | Class: validation_

> _Note: this issue was identified against the code base at commit `f9465bc2186e307f05476a9cadaf16956fa09a02`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateUser` reads every string `AttributeValue` through `crate::state::attribute_text_value`, which checks only the JSON type and the character LENGTH. The modelled `@pattern` of the member being set is never applied, so an `UpdateUser` can write a `UserName`/`DisplayName`/`Title`/… value the model's own character class excludes, and `DescribeUser`/`ListUsers` then report it in output members that declare that same pattern.

The sibling attribute-update handler in this crate does the opposite: `UpdateGroup` rebuilds the modelled member schemas (length AND pattern) and drives the framework's own constraint walk over the `AttributeValue` it was handed, so the identical document channel is pattern-checked for a group and unchecked for a user. `CreateUser` also refuses the very same value, because the model's trait is enforced at the request boundary for a modelled string member. The result is that a value is rejected on create and accepted on update, for one and the same stored member.

`crate::state::attribute_text_value`'s own doc claims the opposite of what it does ("so one modelled constraint has one refusal there") — the pattern constraint has no refusal anywhere on the `UpdateUser` path.

## Evidence

Smithy, the member being written: `smithy_query(query="shape",shape="UserName")` → `{"type": "string", "length": {"min": 1, "max": 128}, "pattern": "^[\\p{L}\\p{M}\\p{S}\\p{N}\\p{P}]+$", "sensitive": true}` (no whitespace admitted).

Unchecked side — services/service-com-amazonaws-identitystore-awsidentitystore/src/ops/update_user.rs:367:
```rust
"username" => user.user_name = optional_text(value, path, USER_NAME_MAX_LEN)?,
```
with `use crate::state::attribute_text_value as optional_text;` (update_user.rs:114) and the reader itself, src/state.rs:1348-1370:
```rust
pub fn attribute_text_value(value: Option<&Document>, path: &str, max_len: usize) -> Result<Option<String>, String> {
    match value {
        None | Some(Document::Null) => Ok(None),
        Some(Document::String(text)) => {
            let length = text.chars().count();
            if !(1..=max_len).contains(&length) { return Err(...) }
            Ok(Some(text.clone()))
        }
        Some(other) => Err(...),
    }
}
```
— type and length only; no `@pattern`. Its doc (state.rs:1342-1343) nevertheless states: "This is the reader `UpdateUser`'s decoders read every string attribute through, so one modelled constraint has one refusal there."

Checked side — src/ops/update_group.rs:173 and :338-353:
```rust
const GROUP_TEXT_PATTERN: &str = r"^[\p{L}\p{M}\p{S}\p{N}\p{P}\t\n\r \x{A0}\x{3000}]+$";
...
let violations: Vec<Violation> = validate(&AttributeText { schema: attribute.member_schema(), text }, &UPDATED_GROUP_ATTRIBUTES_SCHEMA)
```
and update_group.rs:319-320: "`AttributeValue` is a `Document`, so no modelled constraint was enforced at the request boundary: the type the attribute is modelled with, and its `@length` and `@pattern`, are checked here instead".

Test coverage proving the two halves diverge: tests/all/update_group.rs:550-559 refuses `description` = `"bell\u{7}value"` and `"en\u{2000}quad"` ("a value carrying a character the modelled pattern excludes"); tests/all/error_message_rules.rs:908-941 shows `CreateUser` refusing a whitespace-carrying `UserName` with `redacted_clause("userName", &pattern_requirement(USER_NAME_PATTERN))`. The UpdateUser refusal table (tests/all/update_user_attributes.rs:754-848) has rows for wrong type, length, entry counts and the enum — and no row for the character class, so nothing pins the current acceptance.

## Steps to Reproduce

1. CreateUser in the seeded identity store with `UserName: "ada"` — succeeds.
2. Call CreateUser with `UserName: "ada lovelace"` → HTTP 400 ValidationException, clause `Member must satisfy regular expression pattern: [\p{L}\p{M}\p{S}\p{N}\p{P}]+` (boundary enforcement of the modelled pattern).
3. Call UpdateUser on the created user with `Operations: [{"AttributePath": "userName", "AttributeValue": "ada lovelace"}]` → HTTP 200, revision bumped.
4. Call DescribeUser for that user → `UserName: "ada lovelace"`, a value the modelled `UserName` pattern (also declared on the output member) forbids.
5. For contrast, call UpdateGroup with `Operations: [{"AttributePath": "description", "AttributeValue": "bell\u0007value"}]` → HTTP 400 ValidationException, i.e. the sibling handler applies the pattern the user path skips.
