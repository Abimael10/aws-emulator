Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: e822fa4ca12543e97642b0a71000bedf3ee319c6

# The channel-ARN refusal decision is built twice (service.rs override_refusal and validation.rs refusal_of)

_Filed: 2026-11-18T23:54:39Z_
_Operation: PutAuditEvents | Class: maintainability_

> _Note: this issue was identified against the code base at commit `e822fa4ca12543e97642b0a71000bedf3ee319c6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The rule "a pre-handler refusal whose violations include a `ChannelArn` `@pattern` violation is answered as `InvalidChannelARN` with the documented sentence" is implemented twice in this crate, in two files, each with its own `any(..)` scan over the violations and its own `Refused { code, message }` construction. The two copies agree today only because they happen to build the same struct from the same two constants; a later change to one path (a different predicate, an extra condition, a reworded selection) silently leaves the other answering differently for the same fault, which is precisely the drift `refuses_channel_arn` was factored out to prevent. Both call sites could take one shared constructor for the refusal, leaving every response byte and every existing test unchanged.

## Evidence

Copy 1 — services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/service.rs:78-80 and :172-178:

```
fn refused_channel_arn(violations: &[Violation]) -> bool {
    violations.iter().any(refuses_channel_arn)
}
...
        match refusal {
            Refusal::Invalid(violations) if refused_channel_arn(violations) => Some(Refused {
                code: INVALID_CHANNEL_ARN_CODE.to_string(),
                message: INVALID_CHANNEL_ARN_MESSAGE.to_string(),
            }),
```

Copy 2 — services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/service/validation.rs:151-159:

```
    if collected
        .iter()
        .any(|(violation, _)| refuses_channel_arn(violation))
    {
        return Some(Refused {
            code: INVALID_CHANNEL_ARN_CODE.to_string(),
            message: INVALID_CHANNEL_ARN_MESSAGE.to_string(),
        });
    }
```

Both are reached in production: `override_operation` (service.rs:135-147) serves every request through `StatingSubmittedValues`, and service.rs:168-171 keeps the `override_refusal` branch as the answer "for a refusal that somehow arrives without passing the wrapper".

Tests that cover both copies and must stay unchanged: tests/all/channel_arn_refusals.rs:245 `a_channel_reference_outside_the_modelled_pattern_is_the_services_own_refusal` and :91 `a_documented_refusal_states_its_sentence_and_echoes_no_request_value` (asserting `INVALID_CHANNEL_ARN` at lines 111-138, 285-286), tests/all/constraint_refusals.rs:288 `a_channel_reference_is_answered_before_the_payloads_own_violations`, and tests/all/channel_arn_syntax.rs:88/:277/:356.

## Steps to Reproduce

1. Open src/service.rs and read `override_refusal` (lines 149-179): the channel-ARN branch builds `Refused { code: INVALID_CHANNEL_ARN_CODE, message: INVALID_CHANNEL_ARN_MESSAGE }` after `refused_channel_arn(violations)`.
2. Open src/service/validation.rs and read `refusal_of` (lines 147-168): the same predicate is scanned again and the same `Refused` is built again.
3. Change one of the two (e.g. add a condition or reword the selection) and observe that the other path still answers the old way for the same violation set — the two copies have drifted with no test failing at the edit site.
