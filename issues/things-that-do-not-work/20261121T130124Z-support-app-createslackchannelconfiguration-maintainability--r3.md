Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 1e08a8708938a4686fd628aae65515cd67c91098

# Build the handler-raised single-violation validation envelope in one place instead of once per member

_Filed: 2026-11-21T13:01:24Z_
_Operation: CreateSlackChannelConfiguration | Class: maintainability_

> _Note: this issue was identified against the code base at commit `1e08a8708938a4686fd628aae65515cd67c91098`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Two rules in this crate's own `state.rs` apply a MODELLED constraint by hand (because `skip_validation`/the walk's regex engine left it to the handler) and each one then re-implements the same composition to word the refusal: construct one `::aws_smithy_schema_server::constraints::Violation` with a member path, a `ViolationKind`, the rejected value echoed through `::service_common::validation::quoted_echo(..).into_owned()` and `sensitive: false`, then render it with `::aws_smithy_schema_server::constraints::envelope(::std::slice::from_ref(&violation))`.

That composition IS the rule ("a handler-raised refusal for a declared constraint is the framework's one-violation envelope, over a value quoted under the shared echo ceiling, not sensitive") and both doc comments state it in prose — yet the code exists twice, in `channel_name_refusal` and in `channel_role_arn_length_refusal`, so the copies can drift: one could lose `quoted_echo` (and then size a message by the request), or set `sensitive` differently, or stop sharing the renderer, while the other keeps the stated rule. The crate holds every other shared rule of this service in exactly one place by design, and this service's own TEST target already holds this envelope in one helper for the same reason.

The fix is mechanical: one private helper in `state.rs` taking `(path, kind, value)` and returning `envelope(..)`, called by both rules. Every response byte and every existing test stays unchanged — the two call sites differ only in the member path and the `ViolationKind` they pass.

## Evidence

Copy 1 — `channelName`'s pattern rule, services/service-com-amazonaws-supportapp-supportapp/src/state.rs:680-693:
```
pub fn channel_name_refusal(channel_name: &str) -> Option<String> {
    if !carries_pattern_dot_excluded_char(channel_name) {
        return None;
    }
    let violation = ::aws_smithy_schema_server::constraints::Violation {
        path: CHANNEL_NAME_PATH.to_string(),
        kind: ::aws_smithy_schema_server::constraints::ViolationKind::Pattern(
            ::service_common::validation::unanchored(CHANNEL_NAME_PATTERN),
        ),
        value: Some(::service_common::validation::quoted_echo(channel_name).into_owned()),
        sensitive: false,
    };
    ::aws_smithy_schema_server::constraints::envelope(::std::slice::from_ref(&violation))
}
```

Copy 2 — `channelRoleArn`'s length rule, services/service-com-amazonaws-supportapp-supportapp/src/state.rs:734-747:
```
fn channel_role_arn_length_refusal(role_arn: &str) -> Option<String> {
    if role_arn.chars().count() <= MAX_CHANNEL_ROLE_ARN_LEN {
        return None;
    }
    let violation = ::aws_smithy_schema_server::constraints::Violation {
        path: CHANNEL_ROLE_ARN_PATH.to_string(),
        kind: ::aws_smithy_schema_server::constraints::ViolationKind::MaxLength(
            i64::try_from(MAX_CHANNEL_ROLE_ARN_LEN).unwrap_or(i64::MAX),
        ),
        value: Some(::service_common::validation::quoted_echo(role_arn).into_owned()),
        sensitive: false,
    };
    ::aws_smithy_schema_server::constraints::envelope(::std::slice::from_ref(&violation))
}
```

Both doc comments state the SAME single rule the two copies implement —
state.rs:670-679 (`channel_name_refusal`): "the standard validation envelope over one clause naming the rejected value, the member path and the expression, built by the framework's own renderer ([`::aws_smithy_schema_server::constraints::envelope`]) ... The value is echoed through the tree's shared [`::service_common::validation::quoted_echo`], so a handler-worded clause quotes a value under the same ceiling as one the walk restored."
state.rs:724-733 (`channel_role_arn_length_refusal`): "the standard validation envelope over one clause naming the rejected value, the member path and the bound, built by the framework's own renderer ([`::aws_smithy_schema_server::constraints::envelope`]) ... The value is echoed through the tree's shared [`::service_common::validation::quoted_echo`], so a handler-worded clause quotes a value under the same ceiling as one the walk restored and no message is sized by the request."

The crate states the one-place principle for exactly this kind of rule, state.rs:5-18:
"Besides the state struct itself this module holds the few rules the Support App handlers share ... the part of a `.`-based `@pattern` the request boundary's walk cannot apply (`channelName`'s among them) ... so the operation files agree on one implementation of each instead of carrying copies that can drift apart."

This service's own test target already keeps this envelope in ONE helper for that reason — tests/all/error_message_templates.rs:99-108:
```
/// The single-violation validation envelope for a `@pattern` violation.
///
/// `pub(crate)` so the sibling suites in this test target state the same envelope
/// instead of each spelling it out.
pub(crate) fn pattern_refusal(value: &str, member: &str, pattern: &str) -> String {
```

Tests that pin both copies' current output (must keep passing byte for byte):
- tests/all/channel_name_pattern.rs:44 (`use crate::error_message_templates::pattern_refusal;`) — the `channelName` clause for every line terminator, on create and on update.
- tests/all/channel_role_refusal_order.rs:82-83 — "1 validation error detected: Value '{submitted}' at 'channelRoleArn' failed to satisfy constraint: Member must have length less than or equal to {MAX_CHANNEL_ROLE_ARN_LEN}".
- tests/all/channel_role_pass_role_form.rs:339-341 — the same length clause over the wire.
- tests/all/handler_validation_refusal_shape.rs:1-29 — the shape/status every handler-raised 400 of this service carries.

## Steps to Reproduce

1. Open services/service-com-amazonaws-supportapp-supportapp/src/state.rs and read `channel_name_refusal` (lines 680-693) and `channel_role_arn_length_refusal` (lines 734-747).
2. Observe the identical five-step composition in both: build one `Violation` with a member path and a `ViolationKind`, set `value: Some(::service_common::validation::quoted_echo(..).into_owned())`, set `sensitive: false`, then call `::aws_smithy_schema_server::constraints::envelope(::std::slice::from_ref(&violation))`. Only the path and the kind differ.
3. Note that both doc comments above them state that same rule in prose (state.rs:670-679 and 724-733), and that the test target already holds the envelope in one helper (tests/all/error_message_templates.rs:99-108).
4. Change one copy only — e.g. drop `quoted_echo` from `channel_role_arn_length_refusal` and echo the raw value — and see that nothing forces the other copy to follow: the two handler-raised 400s of this service then word the same class of refusal differently.
5. Verify the fix is behaviour-preserving: fold both into one private helper taking `(path, kind, value)` and re-run `cargo test -p service-com-amazonaws-supportapp-supportapp` — tests/all/channel_name_pattern.rs, channel_role_refusal_order.rs, channel_role_pass_role_form.rs and handler_validation_refusal_shape.rs must pass unchanged.
