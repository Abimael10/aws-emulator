Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 7e081c0d0a956f09d8276e607b12ee6e057271df

# Refusal messages quote up to 16 KiB of echoed input, 16x past the modelled @length(0, 1024) on the member that carries them

_Filed: 2026-11-16T17:48:06Z_
_Operation: ListAccountActivities | Class: error-response_

> _Note: this issue was identified against the code base at commit `7e081c0d0a956f09d8276e607b12ee6e057271df`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every `message` this service puts in a refusal is carried by a member the model caps at 1024: `ValidationException$message` (and `ResourceNotFoundException$message`, `AccessDeniedException$message`) target `GenericString`, `@length(min 0, max 1024)`, `@required`. Nothing enforces an OUTPUT constraint at runtime — this crate says so itself (`src/ops/types/date_time_wire.rs:36-40`: "The validating walk that enforces them runs on operation INPUTS only") — so the only thing keeping a refusal inside that bound is the ceiling the service states for the value a clause echoes.

This crate states none, so it inherits the shared default of 16 KiB. Both of its refusal paths quote the submitted value through `service_common::validation::quoted_echo` / `refusing_violations_with_lists`, whose ceiling is `MAX_RESTORED_ECHO = 16 * 1024`: `src/service/validation.rs:311` (the clause writer for a boundary violation and for a handler re-check) and `src/state.rs:935` (`enum_element_refusal`, whose only caller is `ListAccountActivities`, quoting the WHOLE list the caller sent), plus `src/service.rs:248`, which calls `refusing_violations_with_lists::<O>` rather than the `..._quoting_up_to` variant the shared crate provides for exactly this case. A `filterActivityStatuses` carrying one 5 000-character element, or a `nextToken` of 9 000 characters (past the modelled `@length(1, 8192)`), is therefore refused with a `ValidationException` whose `message` quotes the value in full — a message several times longer than the member that has to hold it. At 64 KiB of submitted value the clause is elided at 16 KiB, which is still 16x the modelled maximum.

The shared crate documents the hook this service skips in both directions — "A service whose model states a ceiling of its own asks `quoted_echo_up_to` instead" — and this crate already treats the same 1024 bound as binding for its OTHER error shape: `src/service/authorization.rs:182-186` reasons that the IAM refusal "stays inside the modelled bound of `AccessDeniedException$message` ... by construction", and `tests/all/identity_authorization.rs:536-542` asserts "the message must satisfy the modelled @length(0, 1024)". The validation path's own size test caps at 32 KiB instead (`tests/all/enum_value_set_wording.rs:72`), so it passes with a 16 KiB `message`: the two error shapes of one crate are held to bounds that differ by a factor of sixteen, though the model declares the same `GenericString` for both.

Filed once, against the one operation whose handler reaches the `quoted_echo` call site in `src/state.rs:935`; the boundary path in `src/service.rs:248` is shared by all five operations.

## Evidence

The model's bound on the member every refusal is carried in — `smithy_query(query="shape", shape="ValidationException")`:
```
"ValidationException": {"type": "structure", "errorKind": "client", "httpError": 400, "members": {"message": {"target": "GenericString", "required": true, "length": {"min": 0, "max": 1024}, "pattern": "^[\\S\\s]*$"}}}
```
(`ResourceNotFoundException$message` and `AccessDeniedException$message` are the same `GenericString`: `length {min 0, max 1024}`.)

Nothing checks an output member at runtime — this crate's own statement, `services/service-com-amazonaws-freetier-awsfreetierservice/src/ops/types/date_time_wire.rs:36-40`:
```
//! * Constraint traits (`@length`, `@pattern`, …). The validating walk that enforces them
//!   runs on operation INPUTS only (`smithy_server_runtime` validates an operation's
//!   `INPUT_SCHEMA` before the handler runs). Every structure built with these macros is
//!   output-only
```

The ceiling this service inherits — `services/common/src/validation.rs:1065`:
```
const MAX_RESTORED_ECHO: usize = 16 * 1024;
```
and the hook it documents for a model like this one, `services/common/src/validation.rs:933-950`:
```
/// A service whose model states a ceiling of its own asks [`quoted_echo_up_to`] instead.
#[must_use]
pub fn quoted_echo(value: &str) -> ::std::borrow::Cow<'_, str> {
    quoted_echo_up_to(value, MAX_RESTORED_ECHO)
}
...
/// direction: a member declared longer than [`MAX_RESTORED_ECHO`] so a value can be quoted
/// in full, and one declared shorter so a clause fits the member that has to hold it.
```

This service asks for the UNBOUNDED variants at all three sites. `src/state.rs:916-935` (`enum_element_refusal`, called only by `ListAccountActivities`, `src/ops/list_account_activities.rs:186-192`):
```
/// The list is quoted under the same ceiling as a clause restored from that walk
/// (`service_common::validation::quoted_echo`), so no message is sized by the
/// request.
...
        &::service_common::validation::quoted_echo(&rendered),
```
`src/service/validation.rs:301-311`:
```
/// The value is quoted under the SAME ceiling as a clause restored from that walk
/// (`service_common::validation::quoted_echo`), so neither refusal path of this service
/// states a message sized by the request.
fn enum_violation(path: String, value: &str, declared: &[&str]) -> Violation {
    Violation { path, kind: ViolationKind::EnumValue(..), value: Some(quoted_echo(value).into_owned()), sensitive: false }
```
`src/service.rs:247-249`:
```
    let (violations, lists) =
        service_common::validation::refusing_violations_with_lists::<O>(protocol, req);
```
(the shared crate's `refusing_violations_with_lists_quoting_up_to`, `services/common/src/validation.rs:267-274`, is the variant that takes the service's own `max_echo`).

The same crate treating the identical 1024 bound as binding for its other error shape — `src/service/authorization.rs:182-186`:
```
/// The sentence stays inside the modelled bound of `AccessDeniedException$message` (a
/// `GenericString` of `@length(0, 1024)`, `smithy_query(query="shape",
/// shape="AccessDeniedException")`) by construction
```
and `tests/all/identity_authorization.rs:55-59` + `:536-542`:
```
const MESSAGE_MAX_CHARS: usize = 1024;
...
            assert!(
                message.chars().count() <= MESSAGE_MAX_CHARS,
                "case {case:?}: the message must satisfy the modelled @length(0, \
                 {MESSAGE_MAX_CHARS}), got {} characters: {message:?}",
```

The validation path's size test, which permits a 16 KiB message — `tests/all/enum_value_set_wording.rs:68-72` and `:298-326`:
```
const CLAUSE_SIZE_CAP: usize = 32 * 1_024;
...
    for length in [64 * 1_024, 1_024 * 1_024] {
        let submitted = "Z".repeat(length);
...
            assert!(
                message.len() < CLAUSE_SIZE_CAP,
```
It drives a 64 KiB and a 1 MiB value over the wire and asserts only that the message is under 32 KiB, so a `message` of ~16 KiB — 16x the modelled maximum — passes today.

## Steps to Reproduce

1. Read the model's bound on the member every refusal is carried in: smithy_query(query="shape", shape="ValidationException") reports message as GenericString with length {min 0, max 1024}, @required.
2. Confirm nothing enforces it at runtime: src/ops/types/date_time_wire.rs:36-40 states the validating walk runs on operation INPUTS only, so an output member's @length is never checked.
3. Boot the emulator with this service mounted (tests/all/free_tier.rs::harness).
4. Call ListAccountActivities with {"filterActivityStatuses": ["A" repeated 5000 times]}: observe HTTP 400 ValidationException whose message quotes the whole 5000-character list (the clause writer at src/service/validation.rs:311 / src/state.rs:935 quotes up to MAX_RESTORED_ECHO = 16 KiB, services/common/src/validation.rs:1065), i.e. a message far longer than 1024 characters.
5. Call ListAccountActivities with a nextToken of 9000 characters (past the modelled @length(1, 8192)): the boundary clause (src/service.rs:248) echoes the submitted token, again producing a message over 1024 characters.
6. Call UpgradeAccountPlan with an accountPlanType of 64 KiB — exactly what tests/all/enum_value_set_wording.rs:298-326 already drives — and measure the message: it is elided at 16 KiB, and the test's only size assertion is < 32 KiB (tests/all/enum_value_set_wording.rs:72).
7. Compare with the crate's other error shape, held to the model: src/service/authorization.rs:182-186 and tests/all/identity_authorization.rs:536-542 assert the AccessDeniedException message satisfies the same modelled @length(0, 1024).
