Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 154c4981643d2516610ca3e1a14b7a004d2f471a

# Refusal clause for the @sensitive Tags map claims `Value null` for a map the request carried

_Filed: 2026-11-22T01:42:38Z_
_Operation: RegisterSubscriptionProvider | Class: error-response_

> _Note: this issue was identified against the code base at commit `154c4981643d2516610ca3e1a14b7a004d2f471a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This service words every boundary constraint refusal itself (`service::validation::envelope` + `Clause::render`). A clause decides between stating the offending value and stating `Value null` from `violation.value.is_none()` ALONE. That test is wrong for a member the model marks `@sensitive`: the validating walk DROPS such a value as it records the violation and marks `Violation::sensitive` instead, so the violation arrives with `value: None` even though the member was present. The clause then reports a 51-entry `Tags` map as the `null` of an ABSENT member.

This service's only `@sensitive` input members are exactly the tag selections (`Tags`/`tags`, `tagKeys`), and the crate knows it — `scaffolded.rs` says the tag refusals name nothing from the request "because every member a tag selection travels in is `@sensitive`". The handler-side and first-violation paths are covered: `tag_api_refusal` intercepts a depth-0 `MaxLength` on `tags`/`tagKeys` and answers `BadRequestException` with a constant sentence. What is NOT covered is a tag-selection violation that is not the FIRST violation, which is reachable on `RegisterSubscriptionProvider`: its input carries constrained non-sensitive members declared BEFORE the tag map (`SubscriptionProviderSource` enum, `SecretArn` `@pattern`, then `Tags`). With one of those also invalid, `tag_api_refusal` declines (the first violation sits on `secretArn`/`subscriptionProviderSource`, neither `resourceArn` nor a tag-selection member), the full modelled `ValidationException` envelope is rendered, and the `tags` clause reads `Value null at 'tags' …` for a request that supplied 51 tags.

The shared helper for exactly this case is already in use by this crate (`service_common::validation_message`), is `pub` for precisely the clause-grouping case this module is, and is not called.

## Evidence

This crate decides `absent` from the value half alone, under a comment that states the rule it breaks:

`services/service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions/src/service/validation.rs:232-234`
```
    /// Whether the first violation of this clause carried no value — an absent member,
    /// which is reported as the `null` it was.
    absent: bool,
```
`…/src/service/validation.rs:323`
```
                    absent: violation.value.is_none(),
```
`…/src/service/validation.rs:246-247` (what that renders)
```
        let subject = if self.absent {
            "Value null".to_string()
```
`Violation::sensitive` is never read anywhere in this crate (the only occurrence is a constructed `sensitive: false` in `…/src/service.rs:143`).

The shared walk this crate's refusals come from states that a withheld value is NOT an absent member, and keys the distinction on the flag this crate ignores:

`services/common/src/validation.rs:1551-1556`
```
/// A violation whose value the validating walk WITHHELD — the value of a `@sensitive`
/// member, which the walk drops as it records the violation and marks
/// [`Violation::sensitive`] for — is not an absent member: this walk re-reads the request
/// itself, so it can still state what the caller sent, for a service whose published
/// refusal quotes that value. A service that withholds it instead reads the flag in its own
/// clause builder.
```
`services/common/src/validation.rs:1563` (the shared reading of "absent")
```
    if violation.value.is_none() && !violation.sensitive {
        return SubmittedEcho::Absent;
```
`services/common/src/validation_message.rs:87-90`
```
//! The same half is dropped for a member whose value the refusal WITHHOLDS: a member
//! modelled `@sensitive` is one the service never discloses, so its clause names the
//! member and the requirement and states no value — neither the submitted bytes nor the
//! `null` the framework writes for an absent one.
```
`services/common/src/validation_message.rs:475-477` confirms the collection case this service hits (a `@sensitive` map's own `@length`): "and a COLLECTION reached with no size beside it (a `@sensitive` collection member, whose summary the walk withholds)".

The ready-made helper, `pub` for this crate's exact situation — `services/common/src/validation_message.rs:431-443`:
```
/// The clause `violation` states with its VALUE half dropped — `Value at '<member path>'
/// failed to satisfy constraint: <requirement>` …
/// `pub` for the same reason [`envelope_over`] is: a service that assembles the clauses of
/// its own report — because it GROUPS some of them, which no entry point here can do for it
/// — still drops the value half of a clause that states none …
```

The model facts that make `Tags` the withheld member and put it last in walk order — `smithy_query(query="shape", shape="Tags")`: `{"type": "map", "length": {"min": 0, "max": 50}, "sensitive": true}`; `OPERATIONS.md:85` (`RegisterSubscriptionProvider`): "**Input:** `SubscriptionProviderSource` (… required), `SecretArn` (… pattern …, required), `Tags` (map<string,string>, len 0..50)".

Test coverage: `tests/all/error_taxonomy.rs:15-16` pins only the single-violation case — "a registration past the modelled `Tags` limit is refused with the tag APIs' own `BadRequestException` and registers nothing" — and `tests/all/shared_tag_refusal.rs` / `tests/all/tag_selection_limit.rs` likewise exercise a tag selection as the only offending value. No suite sends a tag selection together with another invalid member, which is the path that renders the envelope.

## Steps to Reproduce

1. Start the emulator and point an SDK/CLI at it with `--endpoint-url`.
2. POST `/subscription/RegisterSubscriptionProvider` with a body that is invalid in two places at once: `{"SubscriptionProviderSource":"RedHat","SecretArn":"not-an-arn","Tags":{ … 51 distinct key/value pairs … }}` (the `SecretArn` breaks the modelled `@pattern`; the `Tags` map breaks `@length 0..50`).
3. Observe the refusal: because the first violation sits on `secretArn`, `service::validation::tag_api_refusal` declines (`src/service/validation.rs:378-383`) and the modelled `ValidationException` envelope is rendered instead.
4. Read the second clause of the message. It states `Value null at 'tags' failed to satisfy constraint: Member must have length less than or equal to 50` — claiming the request sent no `tags` at all, when it sent 51 pairs.
5. Expected, per `services/common/src/validation_message.rs:87-90` and the `valueless_clause` helper at `:445`: the clause names the member and the requirement with NO value half — `Value at 'tags' failed to satisfy constraint: …` — because the value was withheld (`Violation::sensitive`), not absent.
