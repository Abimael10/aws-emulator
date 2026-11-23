Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: c80d66a23f690e017421222dc6c5a4f355562a75

# The empty-`Values` refusal treats a MODELLED @length(min 1) as a service-enforced condition, breaking this crate's own rule 7/rule 8 split

_Filed: 2026-11-23T21:42:45Z_
_Operation: GetFreeTierUsage | Class: error-response_

> _Note: this issue was identified against the code base at commit `c80d66a23f690e017421222dc6c5a4f355562a75`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`check_dimension` re-checks three things about one `Dimensions` comparison before any record is read. Two of them — `Key`'s enum membership and `MatchOptions`' element enum — are correctly recognised as MODELLED constraints and are restated through the boundary walk's own clause writer, unwrapped (`stated_verbatim`). The third, `dimensions.values.is_empty()`, is a modelled constraint too: `DimensionValues$Values` targets the list shape `Values`, which declares `@length(min: 1)`, and the framework's constraint walk enforces that bound over the built input before `handle` runs (it is a struct-member list, not a collection element, so `walk_list`'s nested-list carve-out does not apply). The handler nevertheless words it as a condition of its own: a hand-written sentence pushed through `validation(..)`, i.e. `crate::state::stated_validation`, whose own doc says that envelope "is NOT the envelope of the refusal the framework renders in front of a handler for a constraint the model declares", and whose constant's doc grounds the rule in the `Values` prose rather than in the `@length` the model actually declares.

Two consequences, both inside this crate: (1) the branch is dead — the boundary walk refuses an empty `Values` with the AWS envelope first, so the hand-written sentence can never reach a client, yet it sits in the code as the crate's stated answer for that condition; (2) if it ever were reached (a future change to the walk, or a handler path that builds a comparison without the walk in front of it) one modelled constraint would be reported in two different envelopes by the same crate — exactly the drift rule 8 exists to prevent, and exactly what the two sibling re-checks three lines above and twelve lines below it avoid. The fix is to drop the hand-written condition or to route it through `crate::service::validation::outside_declared_constraints`, the way `GetAccountActivity` re-checks its own modelled `activityId` constraints.

## Evidence

The model declares the bound — `smithy_query(query="shape", shape="Values")`:
```
"Values": {"shape_id": "com.amazonaws.freetier#Values", "type": "list", "length": {"min": 1}, "member": {"target": "Value", "length": {"min": 1, "max": 20}, "pattern": "^[ a-zA-Z0-9\\-\\:\\.\\_\\/\\,\\$\\(\\)]*$"}}
```
and `DimensionValues$Values` carries it as `"required": true, "length": {"min": 1}`.

The crate's own rule for a MODELLED constraint a handler re-checks — `services/service-com-amazonaws-freetier-awsfreetierservice/src/service/validation.rs:39-51`:
```
//! 8. a MODELLED constraint a handler re-checks for itself, so its refusal stays this
//!    operation's 400 whatever the boundary does with the value, is stated through the
//!    very clause writer the walk's own refusal goes through
//!    ([`outside_enum`], [`outside_element_enum`], [`outside_declared_constraints`]) — so
//!    no second rendering of a value
//!    set, of a member path or of a requirement exists in this crate to drift from the
//!    one above
```
and its rule 7, for a bound the SERVICE enforces instead — `src/service/validation.rs:36-38`:
```
//! 7. a bound the SERVICE enforces for itself — one the model does not declare, so the
//!    constraint walk never sees it — is stated in the same envelope and the same
//!    requirement sentence, and quotes NO value ([`above_ceiling`]);
```

The envelope the handler actually uses, and its own statement that it is the wrong one for a modelled constraint — `src/state.rs:727-731` and `:745-747`:
```
/// sentence through this. It is NOT the envelope of the refusal the framework
/// renders in front of a handler for a constraint the model declares: that one
/// is the per-member validation envelope
/// (`crate::service::validation`), which this service's recordings show
/// unwrapped.
...
pub fn stated_validation(sentence: &str) -> String {
    format!("[Message = {sentence}]")
}
```

The offending code, between the two re-checks that get it right — `src/ops/get_free_tier_usage.rs:555-564` and `:574-581`:
```
    let key_member = member_path(path, DIMENSION_KEY_MEMBER);
    if let Some(message) = crate::service::validation::declared_enum_values(&key_member)
        .and_then(|declared| crate::state::enum_value_refusal(&key_member, key, declared))
    {
        return Err(stated_verbatim(&message));
    }

    if dimensions.values.is_empty() {
        return Err(validation(AT_LEAST_ONE_VALUE));
    }
```
```
    let options_member = member_path(path, MATCH_OPTIONS_MEMBER);
    if let Some(message) =
        crate::service::validation::declared_enum_values(&options_member).and_then(|declared| {
            crate::state::enum_element_refusal(&options_member, &dimensions.match_options, declared)
        })
    {
        return Err(stated_verbatim(&message));
    }
```
where `validation` wraps and `stated_verbatim` does not — `src/ops/get_free_tier_usage.rs:705-710` and `:720-725`:
```
fn validation(message: impl AsRef<str>) -> GetFreeTierUsageError {
    GetFreeTierUsageErrorModeled::ValidationException(ValidationException {
        message: crate::state::stated_validation(message.as_ref()),
    })
    .into()
}
```
```
fn stated_verbatim(message: &str) -> GetFreeTierUsageError {
    GetFreeTierUsageErrorModeled::ValidationException(ValidationException {
        message: message.to_string(),
    })
    .into()
}
```
The constant's doc, which grounds the rule in prose and never mentions the declared `@length` — `src/ops/get_free_tier_usage.rs:295-302`:
```
/// What the service states when a comparison carries no value to compare against
/// (`DimensionValues$Values`: "the results all match at least one of the specified
/// values", so a comparison against nothing matches nothing).
...
const AT_LEAST_ONE_VALUE: &str = "At least one value must be provided";
```

The sibling that handles a modelled constraint correctly — `src/ops/get_account_activity.rs:186-192`:
```
        if let Some(message) = crate::service::validation::outside_declared_constraints(
            &input,
            Self::INPUT_SCHEMA,
            ACTIVITY_ID_MEMBER,
        ) {
            return Err(refusal(message));
        }
```

That the walk really does reach this list: the macro reads a member's constraints off the TARGET shape (`vendor/smithy-macros/src/codegen.rs:628-651`, "Extract every supported constraint trait for a member: the target shape's, plus any the MEMBER declares itself"), `DIMENSION_VALUES_SCHEMA` is one of the seven shapes the service walks (`src/service/validation.rs:525-537`), and a list inside a struct is shielded from the nested-list carve-out by `CollectionFrame::Struct` (`vendor/smithy-schema-server/src/constraints.rs:1024`, `:1218`).

Test coverage: no test in the crate drives an empty `Values` — `grep -rn 'At least one value\|values: vec!\[\]' services/service-com-amazonaws-freetier-awsfreetierservice` matches only the constant's definition and its single use — while the two neighbouring re-checks are pinned over the wire by `tests/all/match_option_cardinality.rs` and `tests/all/enum_value_set_wording.rs`. So nothing pins the current wording, and the fix changes no asserted response.

## Steps to Reproduce

1. Read the model's bound: smithy_query(query="shape", shape="Values") reports the list shape `Values` with "length": {"min": 1}, and DimensionValues$Values as required with that same length — i.e. an empty `Values` breaks a constraint the MODEL declares.
2. Read src/service/validation.rs:36-51: rule 7 covers a bound the SERVICE enforces (one the model does not declare) and rule 8 covers a MODELLED constraint a handler re-checks, which must be stated through the walk's own clause writer.
3. Read src/state.rs:727-731 and :745-747: the `[Message = <sentence>]` envelope `stated_validation` writes is explicitly NOT the envelope for a constraint the model declares.
4. Open src/ops/get_free_tier_usage.rs and compare the three re-checks in `check_dimension`: :555-560 (Key, a modelled enum) and :574-581 (MatchOptions, a modelled element enum) both return `stated_verbatim(..)`, while :562-564 returns `validation(AT_LEAST_ONE_VALUE)` — the `stated_validation` envelope — for the modelled @length(min 1) on `values`.
5. Confirm the branch is dead: the boundary walk enforces a struct-member list's own @length (vendor/smithy-schema-server/src/constraints.rs:1024 pushes CollectionFrame::Struct, so :1218 makes `values` a non-element list whose own @length is checked), and the macro captures the target shape's @length (vendor/smithy-macros/src/codegen.rs:628-651). POST GetFreeTierUsage with {"maxResults":10,"filter":{"Dimensions":{"Key":"REGION","Values":[],"MatchOptions":["EQUALS"]}}} and observe the AWS `<N> validation error(s) detected: …` envelope from src/service.rs:267-274, never the `[Message = At least one value must be provided]` sentence the handler holds.
6. Confirm nothing pins the handler's sentence: grep -rn 'At least one value' services/service-com-amazonaws-freetier-awsfreetierservice matches only src/ops/get_free_tier_usage.rs:302 and :563.
