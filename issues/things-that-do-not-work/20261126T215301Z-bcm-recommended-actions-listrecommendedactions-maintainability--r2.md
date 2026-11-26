Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 497ec0a6492c1c68bf7d8cf0c18562e5a9656324

# Handler re-implements service_common's value elision and violation cap instead of calling the shared crate

_Filed: 2026-11-26T21:53:01Z_
_Operation: ListRecommendedActions | Class: maintainability_

> _Note: this issue was identified against the code base at commit `497ec0a6492c1c68bf7d8cf0c18562e5a9656324`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`src/ops/list_recommended_actions.rs` hand-rolls two spans that `service-common` — already a dependency of this crate, and already called from this very file — publishes as public API, so the copies can drift from the shared ones:

1. `fn echoed` (the character-boundary-safe elision of a caller value at 256 bytes, rendered `<prefix>... [<n> bytes]`) is byte-for-byte the same computation as `service_common::validation::quoted_echo_up_to(value, 256)` plus its private `char_prefix`. The shared module states it is meant to be the single such span: "so there is one character-boundary-safe elision in the tree ([`char_prefix`])".
2. `const MAX_VIOLATIONS: usize = 100;` restates `service_common::validation::MAX_VIOLATIONS`, whose own doc names exactly this kind of copy as outstanding work: "rather than a literal of its own that can drift from it (`grep -rn 'MAX_VIOLATIONS' services/*/src` lists the copies left to migrate here)".

The crate already depends on `service-common` (Cargo.toml) and already reaches into it from this same file for the requirement sentence (`service_common::validation_message::requirement`), so nothing new is linked. `quoted_echo_up_to` produces the identical string for every input (same `<= max` short-circuit, same decrementing `is_char_boundary` walk, same `"{}... [{} bytes]"` format), and the shared `MAX_VIOLATIONS` is the same value 100 — so every response body and every existing test stays exactly as it is.

## Evidence

Local copy 1 — services/service-com-amazonaws-bcmrecommendedactions-awsbillingandcostmanagementrecommendedactions/src/ops/list_recommended_actions.rs:531-541:
```
fn echoed(value: &str) -> String {
    const MAX_ECHOED_VALUE: usize = 256;
    if value.len() <= MAX_ECHOED_VALUE {
        return value.to_string();
    }
    let mut end = MAX_ECHOED_VALUE;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... [{} bytes]", &value[..end], value.len())
}
```

Shared API it duplicates — services/common/src/validation.rs:1174-1183:
```
pub fn quoted_echo_up_to(value: &str, max_echo: usize) -> ::std::borrow::Cow<'_, str> {
    if value.len() <= max_echo {
        return ::std::borrow::Cow::Borrowed(value);
    }
    ::std::borrow::Cow::Owned(format!(
        "{}... [{} bytes]",
        char_prefix(value, max_echo),
        value.len()
    ))
}
```
and services/common/src/validation.rs:1304-1313:
```
fn char_prefix(value: &str, max: usize) -> &str {
    if value.len() <= max {
        return value;
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
```
with services/common/src/validation.rs:1193 stating the intent: "so there is one character-boundary-safe elision in the tree ([`char_prefix`])".

Local copy 2 — src/ops/list_recommended_actions.rs:565: `const MAX_VIOLATIONS: usize = 100;`
Shared API it duplicates — services/common/src/validation.rs:201: `pub const MAX_VIOLATIONS: usize = 100;`, whose doc at services/common/src/validation.rs:198-200 says: "so a service that bounds its own rendering needs the same number the walk used, rather than a literal of its own that can drift from it (`grep -rn 'MAX_VIOLATIONS' services/*/src` lists the copies left to migrate here)."

The dependency is already in place — Cargo.toml:40: `service-common = { path = "../common" }`, and src/ops/list_recommended_actions.rs:411: `service_common::validation_message::requirement(violation.kind.clone())`.

Tests that cover both spans and must stay unchanged:
- tests/all/validation_message_wording.rs:340 `an_enum_refusal_lists_the_value_set_in_the_order_the_service_states_it` and :436 `an_enum_refusal_is_the_same_whatever_value_the_member_carried`, whose case table at tests/all/validation_message_wording.rs:197 (`let long = "K".repeat(300);`) is described at :193 as "a value longer than a refusal echoes" — i.e. it drives the elision span.
- tests/all/list_recommended_actions.rs:912 `a_refusal_names_at_most_the_capped_number_of_members` — the `MAX_VIOLATIONS` cap.
- tests/all/unreadable_input.rs — the `cannotParse` path, the other caller of `echoed` (via `cannot_parse_input`, src/ops/list_recommended_actions.rs:435-447).

## Steps to Reproduce

1. Open services/service-com-amazonaws-bcmrecommendedactions-awsbillingandcostmanagementrecommendedactions/src/ops/list_recommended_actions.rs and read `fn echoed` (lines 531-541) and `const MAX_VIOLATIONS` (line 565).
2. Open services/common/src/validation.rs and read `pub fn quoted_echo_up_to` (lines 1174-1183) with `fn char_prefix` (lines 1304-1313), and `pub const MAX_VIOLATIONS` (line 201) with its doc at lines 191-200.
3. Confirm the two renderings are the same computation and the two constants the same value, so the handler's copies add nothing but a second place to change.
4. Confirm the dependency is already present (Cargo.toml:40) and already used from this file (src/ops/list_recommended_actions.rs:411), so replacing the local copies with the shared ones links nothing new and changes no response byte.
5. Run the named tests (tests/all/validation_message_wording.rs, tests/all/list_recommended_actions.rs, tests/all/unreadable_input.rs) before and after: they must be identical.
