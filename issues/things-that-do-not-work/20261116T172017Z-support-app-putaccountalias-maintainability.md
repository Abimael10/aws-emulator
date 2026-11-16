Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 16dfe53738d662d0dc392dd1f50e487d5b567c22

# Replace the crate's hand-rolled `fn unanchored` with `service_common::validation::unanchored`

_Filed: 2026-11-16T17:20:17Z_
_Operation: PutAccountAlias | Class: maintainability_

> _Note: this issue was identified against the code base at commit `16dfe53738d662d0dc392dd1f50e487d5b567c22`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`src/service.rs` renders every boundary `@pattern` refusal of this service through a private, hand-rolled anchor-stripping function (`fn unanchored`, service.rs:78), including the escaped-trailing-`$` special case. The workspace crate `service-common` — already a DIRECT dependency of this crate, and already called from this crate for the sibling rendering helper `validation::quoted_echo` — owns that exact rendering as `service_common::validation::unanchored`, and that module's own documentation states the inlined per-service copies are to be deleted in favour of it and that nothing new should inline the rule again.

Two copies of one rendering rule can drift (the local copy strips a leading `^` even when there is no trailing `$`, and for an escaped trailing `$` returns the body with the `^` already removed, while the shared one drops the pair POSITIONALLY and returns the regex verbatim unless BOTH ends carry an unescaped anchor). Every `@pattern` this service's model declares is double-anchored with an unescaped `$` and carries no top-level alternation — `^\S+$` (teamId/channelId/paginationToken), `^.+$` (channelName/teamName), `^[\w\- ]+$` (awsAccountAlias), `^arn:aws:iam::[0-9]{12}:role/(.+)$` (roleArn) — so the two implementations agree on every string this service can render. Switching `as_stated` to call the shared function (adapting `&str` -> `String`) leaves every response byte and every existing test unchanged and removes the second copy of the rule.

## Evidence

The local copy — services/service-com-amazonaws-supportapp-supportapp/src/service.rs:78-91:
```rust
fn unanchored(pattern: &str) -> &str {
    let body = pattern.strip_prefix('^').unwrap_or(pattern);
    match body.strip_suffix('$') {
        Some(stripped) => {
            let backslashes = stripped.len() - stripped.trim_end_matches('\\').len();
            if backslashes % 2 == 0 { stripped } else { body }
        }
        None => body,
    }
}
```
reached from the service seam, services/service-com-amazonaws-supportapp-supportapp/src/service.rs:65-72: `ViolationKind::Pattern(pattern) => Violation { kind: ViolationKind::Pattern(unanchored(pattern).to_string()), ..violation.clone() }`, which `override_refusal` renders every boundary refusal through (service.rs:162-168).

The crate API that replaces it — services/common/src/validation.rs:1554-1562:
```rust
pub fn unanchored(regex: &str) -> String {
    let Some(body) = regex.strip_prefix('^') else { return regex.to_string(); };
    match body.strip_suffix('$') {
        Some(inner) if !ends_with_escape(inner) => inner.to_string(),
        _ => regex.to_string(),
    }
}
```
and that module's own migration note, services/common/src/validation.rs:145-152: "LANDED here, while ~26 service crates still carry their own inlined copy (`grep -rln 'fn unanchored' services/*/src`), each pinned by that service's own recordings and tests; deleting those copies in favour of this one is the migration's own change, and nothing new should inline the rule again."

The dependency is already present and already used for the SAME module: services/service-com-amazonaws-supportapp-supportapp/Cargo.toml:33 `service-common = { path = "../common" }`, and services/service-com-amazonaws-supportapp-supportapp/src/state.rs:594 `value: Some(::service_common::validation::quoted_echo(role_arn).into_owned()),`.

Equivalence over this service's whole pattern set, as its own tests spell it — services/service-com-amazonaws-supportapp-supportapp/tests/all/error_message_templates.rs:66-74: "`awsAccountAlias`'s `@pattern`, with the anchors the model spells it with stripped ... Model value: `^[\w\- ]+$`" / `const ACCOUNT_ALIAS_PATTERN: &str = r"[\w\- ]+";`, `const SLACK_ID_PATTERN: &str = r"\S+";` (model `^\S+$`), `const CHANNEL_NAME_PATTERN: &str = r".+";` (model `^.+$`). All double-anchored with an unescaped `$`, so both implementations return the identical inner body.

Tests that cover the span and must stay green unchanged: tests/all/error_message_templates.rs:470 `a_pattern_violation_states_the_model_pattern_without_its_anchors` (a row per patterned member, over put-account-alias, create-slack-channel-configuration and delete-slack-channel-configuration), tests/all/error_message_templates.rs:586 `every_other_constraint_class_keeps_the_standard_clause`, and tests/all/slack_channel_configurations.rs:1364 `an_alias_outside_the_modelled_constraints_is_refused`.

## Steps to Reproduce

1. Read services/service-com-amazonaws-supportapp-supportapp/src/service.rs:65-91: `as_stated` rewrites a `ViolationKind::Pattern` through the private `fn unanchored` declared just below it.
2. Read services/common/src/validation.rs:1554-1562 (`pub fn unanchored`) and the module note at lines 145-152 stating the inlined service copies are to be deleted in favour of it and that nothing new should inline the rule again: the same anchor-strip rule, written twice.
3. Confirm the dependency needs no addition: services/service-com-amazonaws-supportapp-supportapp/Cargo.toml:33 already lists `service-common`, and src/state.rs:594 already calls `::service_common::validation::quoted_echo` from the same module.
4. Enumerate every `@pattern` in the model (`smithy_query(query="shape", shape="teamId,channelId,paginationToken,channelName,teamName,awsAccountAlias,roleArn")`): all are `^…$` with an unescaped `$` and no top-level alternation, so the two implementations return the same string for each.
5. Point `as_stated` at `::service_common::validation::unanchored(pattern)` (returns `String`), delete the private copy, and run `cargo test -p service-com-amazonaws-supportapp-supportapp`: `a_pattern_violation_states_the_model_pattern_without_its_anchors`, `every_other_constraint_class_keeps_the_standard_clause` and `an_alias_outside_the_modelled_constraints_is_refused` must pass byte-for-byte unchanged.
