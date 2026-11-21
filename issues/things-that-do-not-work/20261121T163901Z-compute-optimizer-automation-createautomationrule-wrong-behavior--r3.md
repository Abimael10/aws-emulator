Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: c554cb44f930369d634e98958e7954aa18dc122a

# CreateAutomationRule's client-token fingerprint ignores the defaults the handler materialises, so a retry that spells an omitted optional member as that default is refused 409 instead of replayed

_Filed: 2026-11-21T16:39:01Z_
_Operation: CreateAutomationRule | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `c554cb44f930369d634e98958e7954aa18dc122a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The handler takes its idempotency fingerprint over the raw request with only `clientToken` removed, and its comment asserts that nothing else is normalised. One hundred lines further down the same handler DOES normalise three members when it stores the rule: an absent `description` becomes `DEFAULT_RULE_DESCRIPTION` (the empty string), an absent `priority` becomes `DEFAULT_RULE_PRIORITY` ("1"), and absent `tags` becomes the empty list. Two requests that differ only in whether such a member is omitted or spelled as exactly that default are therefore acted on IDENTICALLY (same stored rule, byte-identical response built by the single `output_for`), yet they digest differently, so the second one presented under the same client token is classified `TokenOutcome::ParameterMismatch` and refused `IdempotentParameterMismatchException` (409) instead of replaying the first call's rule.

That is the opposite of the rule the shared helper's own documentation states ("what the ledger has to tell apart is requests the service acts on differently ... every member the handler NORMALISES before acting ... carry the normalised value in the probe ... Two spellings of one request therefore replay"), and the handler's probe closure is exactly the seam where that normalisation was meant to be declared — `UntagResource` uses it for `tagKeys` and both association handlers use it for `accountIds`, so the mechanism already exists in this crate and this one path skips it.

The practical case is an ordinary retry: a client that creates a rule, reads the response (which reports `description: ""`, `priority: "1"`, `tags: []`), and retries the create from the values it read back under the same client token gets a 409 conflict for a request the service would have stored identically.

## Evidence

The handler claims no member is normalised — services/service-com-amazonaws-computeoptimizerautomation-computeoptimizerautomationservice/src/ops/create_automation_rule.rs:239-246:
```
        // The request as THIS handler reads it, which is what its client token
        // ties (`crate::state::request_fingerprint`): only the token itself is
        // left out. Every other member is STORED as sent and reported back in
        // that spelling, so none is normalised here and any difference in one
        // is a different request.
        let request_fingerprint = crate::state::request_fingerprint(&input, |probe| {
            probe.client_token = None;
        });
```

The same handler normalising three of those members — create_automation_rule.rs:344-362:
```
            // `description` and `priority` are optional in the request but the
            // service reports both for every rule it holds, so a create that
            // named neither stores the values the service itself materialises
...
            description: input
                .description
                .unwrap_or_else(|| crate::state::DEFAULT_RULE_DESCRIPTION.to_string()),
...
            priority: input
                .priority
                .unwrap_or_else(|| crate::state::DEFAULT_RULE_PRIORITY.to_string()),
...
            tags: input.tags.unwrap_or_default(),
```

The rule the fingerprint helper states — services/service-com-amazonaws-computeoptimizerautomation-computeoptimizerautomationservice/src/state.rs:510-520:
```
/// A client token exists so a retry of ONE request is replayed rather than acted
/// on twice, so what the ledger has to tell apart is requests the service acts
/// on differently ([`TokenOutcome::ParameterMismatch`]). `as_read` is where a
/// handler states that reading of its own input: the client token itself, which
/// is not part of the request it ties, and every member the handler NORMALISES
/// before acting ... carry the normalised value in the probe. Two
/// spellings of one request therefore replay
```

Both spellings pass validation, so both reach the ledger: `RuleDescription` is `@length(min 0)` with pattern `^[a-zA-Z0-9_\-\s@\.]*$` (empty admitted), `priority_is_valid` accepts "1" (state.rs:4076-4079 `Ok(1) => fractional.is_none_or(...)`), and `tag_list_violation(&[])` is `None`.

Test coverage: tests/all/client_token_request_spellings.rs:15-21 enumerates the normalised members as only "`accountIds` ... and the `tagKeys` of `UntagResource`", so no test pins the create case; tests/all/automation_rule_lifecycle.rs:1388-1395 only varies `status`, a member the service really does act on differently, so fixing this breaks no existing test.

## Steps to Reproduce

1. Opt the caller in: UpdateEnrollmentConfiguration {"status":"Active"}.
2. CreateAutomationRule with {"name":"r1","ruleType":"AccountRule","recommendedActionTypes":["UpgradeEbsVolumeType"],"schedule":{"scheduleExpression":"30 12 * * ? *"},"status":"Active","clientToken":"t1"} — no description, no priority, no tags. Observe 200 with description "", priority "1", tags [].
3. Retry the same create under the same clientToken "t1", this time spelling the members exactly as the response reported them: add "description":"", "priority":"1", "tags":[].
4. Expected (state.rs:510-520): the retry is a replay — 200 with the rule the first call minted. Actual: 409 IdempotentParameterMismatchException, although storing the request would have produced the identical rule.
