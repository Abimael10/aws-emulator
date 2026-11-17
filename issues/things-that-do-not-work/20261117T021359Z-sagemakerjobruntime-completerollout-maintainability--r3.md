Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: c57dff367a65e304f9274fadba3272377fe8966a

# The malformed-ARN refusal path holds its own value echo instead of the helper the crate documents as "THE ONE place" it does so

_Filed: 2026-11-17T02:13:59Z_
_Operation: CompleteRollout | Class: maintainability_

> _Note: this issue was identified against the code base at commit `c57dff367a65e304f9274fadba3272377fe8966a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::service::each_held_within_the_message_maximum` is documented as the single place a refusal of this service holds a caller-sized value ("THE ONE place a refusal of this service holds a caller-sized value, whichever path words it"). Two of the three refusal paths do route through it: `held_within_the_message_maximum` (the framework's collected violations) and `authorization::held_to_what_the_member_leaves` (the 403 sentence). The third path — the single-clause malformed-ARN wording inside `refusal_message` — re-implements the helper's body inline instead: it computes `LARGEST_MESSAGE_BYTES.saturating_sub(<prose>)` itself and calls `service_common::validation::echo_held_to` itself, which is exactly the helper's one-value case. So the rule "a quoted value is held to the member maximum less the clause prose" is written twice in this crate's own code and the two copies can drift: a change to the budget arithmetic in `each_held_within_the_message_maximum` (for example how the share is computed, or a different holding function) silently does not apply to the malformed-ARN sentence, even though its own doc comment claims it applies "whichever path words it".

The fix is behaviour-preserving: for one value the helper computes `budget = LARGEST_MESSAGE_BYTES - prose`, `left = 1`, `share = budget / 1 = budget`, then `echo_held_to(value, share)` — byte-for-byte what lines 230-232 already produce. Every response and every existing test stays unchanged.

## Evidence

The documented rule, services/service-com-amazonaws-sagemakerjobruntime-agenticrftruntimeservice/src/service.rs:324-330:
```
/// THE ONE place a refusal of this service holds a caller-sized value, whichever path words
/// it: the values the framework's clauses quote ([`held_within_the_message_maximum`]) and the
/// principal and resource the authorization gate's own sentence names
/// ([`authorization`]) — whose principal ARN is read out of the IAM store and is therefore
/// bounded by THAT model rather than by this one, so a sentence carrying it is not inside
/// this member by construction the way one interpolating only this model's own members is.
```
and the helper it sits on, src/service.rs:341-355:
```
pub(crate) fn each_held_within_the_message_maximum(prose: usize, quoted: &[&str]) -> Vec<String> {
    let mut shortest_first: Vec<usize> = (0..quoted.len()).collect();
    shortest_first.sort_by_key(|at| quoted[*at].len());

    let mut budget = LARGEST_MESSAGE_BYTES.saturating_sub(prose);
    ...
        let value = service_common::validation::echo_held_to(quoted[at], share);
```

The path that does NOT call it, src/service.rs:227-234 (inside `refusal_message`, the one renderer both refusal paths of this service reach):
```
    if let [only] = violations {
        if let Some(value) = nameable_malformed_arn(only) {
            // One clause, one value: its room is the whole member less this wording's prose.
            let room = LARGEST_MESSAGE_BYTES.saturating_sub(malformed_arn_prose_bytes());
            return Some(malformed_arn_message(
                &service_common::validation::echo_held_to(value, room),
            ));
        }
    }
```

The sibling paths that DO call it — src/service.rs:301 `let mut held_values = each_held_within_the_message_maximum(prose, &quoted).into_iter();` and src/service/authorization.rs:273-278:
```
fn held_to_what_the_member_leaves(worded_without_values: &str, values: &[&str]) -> HeldValues {
    HeldValues(crate::service::each_held_within_the_message_maximum(
        worded_without_values.len(),
        values,
    ))
}
```

Test coverage that pins the current output (and must stay green unchanged): src/service.rs test `a_refusal_of_any_clause_count_fits_the_carrying_member` ("the malformed-ARN wording this service puts in place of the ARN pattern clause is measured beside them", src/service.rs:695-696); tests/all/refusal_fits_the_modelled_message_member.rs:444 `message.contains("Input arn ") && message.ends_with(" is malformed")`; tests/all/refusal_names_the_value_it_states.rs:193 `let wording = format!("Input arn {job_arn} is malformed");`; tests/all/rejected_value_echo.rs:323.

## Steps to Reproduce

1. Open services/service-com-amazonaws-sagemakerjobruntime-agenticrftruntimeservice/src/service.rs and read the doc comment at lines 320-340 on `each_held_within_the_message_maximum`: it states it is "THE ONE place a refusal of this service holds a caller-sized value, whichever path words it".
2. Read lines 227-234 of the same file: the single-clause malformed-ARN branch of `refusal_message` computes `LARGEST_MESSAGE_BYTES.saturating_sub(malformed_arn_prose_bytes())` and calls `service_common::validation::echo_held_to` directly, never reaching that helper.
3. Compare with the two paths that do reach it — src/service.rs:301 and src/service/authorization.rs:273-278 — confirming the budget arithmetic plus `echo_held_to` call exists twice in this crate.
4. Confirm the duplication is observable-behaviour-neutral: with one value the helper yields `share = budget / 1 = budget`, i.e. the same `room` the inline copy computes, so `refusal_message` would return the same bytes for every input.
