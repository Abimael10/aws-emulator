Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2.set-rule-priorities-duplicate-fails
Issue-Shape-Id: com.amazonaws.elasticloadbalancingv2#ElasticLoadBalancing_v10
Issue-Key: elbv2.set-rule-priorities-duplicate-fails|run#22|AssertionError|assert code == "ValidationError", f"expected validation error, got {code}"
Issue-Commit: 22429b7a0c0ae7d0c87a7901239a937fd78a2091

# elbv2: SetRulePriorities answers the modeled PriorityInUse for an IN-REQUEST duplicate priority, where AWS refuses at input validation with ValidationError

> _Note: this issue was identified against the code base at commit `22429b7a0c0ae7d0c87a7901239a937fd78a2091`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetRulePriorities

## Description
One net-new root cause on the ELBv2 control plane, grounded in the single failing behavioral check the replay report lists under `### elbv2` that it marks neither ALREADY FILED nor OWNED. Open issues that look adjacent but do NOT carry this check's replay key, and so do not cover it: 20261113T153150Z-elbv2-3-net-new-causes-the-setrulepriorities-priorityinuse.md (its Cause 1 is the MESSAGE of a genuine cross-rule PriorityInUse collision — a different input shape and a different requirement than the error CLASS for an in-request duplicate; its trace ids are elbv2.set-rule-priorities-conflict-with-existing-fails and elbv2.set-rule-priorities-duplicate-priority-fails, not this check) and 20261030T195554Z-elasticloadbalancing-elbv2-setrulepriorities-response-emits-an-empty-transforms-element.md (the success-path response shape). The resolved 20261025T020832Z-elastic-load-balancing-v2-setrulepriorities-stub.md (stub implementation) does not cover it either: the operation IS implemented here and the check's positive-control call succeeded.

=== Cause 1 of 1: a SetRulePriorities request that names the SAME priority twice is refused with the modeled PriorityInUse instead of the generic ValidationError ===
Failing check: elbv2.set-rule-priorities-duplicate-fails.
AWS: Smithy com.amazonaws.elasticloadbalancingv2#SetRulePriorities (input member RulePriorities -> RulePriorityList, required) models exactly three errors — OperationNotPermittedException (awsQueryError OperationNotPermitted, 400), PriorityInUseException (awsQueryError PriorityInUse, 400, doc 'The specified priority is in use.') and RuleNotFoundException (awsQueryError RuleNotFound, 400). The model states no cross-member uniqueness rule for the list, so for this input shape: recorded behavior only. The recording shows AWS rejecting the duplicate inside ONE request at input-validation time, BEFORE any rule or listener state is consulted: HTTP 400, Error.Code 'ValidationError' (the awsQuery generic validation refusal, not one of the three modeled shapes), Error.Message "Priority '<priority>' was provided multiple times". In that message the fixed text is `Priority '` ... `' was provided multiple times`; the only variable part is the duplicated priority value taken verbatim from the request's RulePriorities[*].Priority (the recording's 30 is a request value filling that slot, not part of the template). The modeled PriorityInUse is reserved for a priority some OTHER rule on the same listener still holds — a state-dependent conflict that can be made valid by freeing that priority — whereas an in-request duplicate is unsatisfiable by construction and so is a request-shape error. The rejected call must leave every priority exactly as it was.
Emulator: the call is refused, but with the wrong error class: HTTP 400 with Error.Code 'PriorityInUse' (the modeled PriorityInUseException), so a client looking for ValidationError sees a state conflict instead of a malformed request. The check's preceding positive control passed — a SetRulePriorities moving rule A to 22 and rule B to 23 took effect and DescribeRules reported 22/23 — so only the duplicate-priority classification diverges. The emulator's handler deliberately maps this input to the modeled conflict: services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10/src/ops/set_rule_priorities.rs lines 204-210 return `priority_in_use()` when a second pair repeats a priority already asked for in the same list, while the sibling in-request duplicate (the same RuleArn twice, lines 199-203) is correctly refused with validation_error.
Why: the two refusals mean opposite things to a client. PriorityInUse is retryable/resolvable — a deploy or blue-green tool frees or re-numbers the conflicting rule and retries the identical request — while a request that names one priority twice can never succeed no matter what state changes. Against the emulator such a tool loops or backs off on a request it should have reported as a programming error, and a client that branches on ValidationError to surface 'bad input' never takes that branch. Conversely, a client that treats PriorityInUse as 'someone else holds this priority' is pointed at state that is in fact fine.
Fix: SetRulePriorities' request-level input validation — the in-request duplicate-priority branch in the operation's handler must refuse with the generic ValidationError (HTTP 400) and AWS's message template above, formatted with the duplicated priority from the request, rather than constructing the modeled PriorityInUseException. The state-dependent collision check (a priority held by a rule outside the request) keeps answering PriorityInUse and is not part of this cause; the no-partial-effect behavior already holds and must be preserved.
Traces: elbv2.set-rule-priorities-duplicate-fails.

## Evidence
Replay report, service elbv2 — the only failing check under `### elbv2` not marked ALREADY FILED or OWNED:

[mismatch] elbv2.set-rule-priorities-duplicate-fails
  check_elbv2_set_rule_priorities_duplicate_fails.py, line 192 (in run)
  AssertionError: expected validation error, got PriorityInUse
    assert code == "ValidationError", f"expected validation error, got {code}"

The check's own statement of AWS behavior (docstring): "SetRulePriorities assigning the same priority twice in ONE request must fail ... Real AWS rejects the in-request duplicate at input-validation time (ValidationError) rather than with the modeled PriorityInUse."

Request under test (one call, two pairs, same priority):
  set_rule_priorities(RulePriorities=[{RuleArn: <ruleA>, Priority: 30}, {RuleArn: <ruleB>, Priority: 30}])
Expected (AWS): HTTP 400, Error.Code 'ValidationError', Error.Message "Priority '30' was provided multiple times" — fixed text `Priority '`/`' was provided multiple times`, variable part = the duplicated Priority from the request.
Actual (emulator): HTTP 400, Error.Code 'PriorityInUse' (modeled PriorityInUseException); the check stops at the code assertion, so the message assertion that follows was not reached.
The check then re-reads priorities and requires rule A == 22 and rule B == 23 (no partial effect); the positive-control SetRulePriorities to 22/23 immediately before the duplicate call succeeded against the emulator.

Smithy citations (services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10/smithy-model/com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10.json):
  SetRulePriorities: input {RulePriorities -> RulePriorityList, required}; output {Rules}; errors OperationNotPermittedException (OperationNotPermitted/400), PriorityInUseException (PriorityInUse/400), RuleNotFoundException (RuleNotFound/400). No ValidationError is modeled — it is the awsQuery generic validation refusal — and no uniqueness constraint is declared on RulePriorityList, so the in-request duplicate's classification is recorded behavior.

Emulator source (for orientation only):
  services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10/src/ops/set_rule_priorities.rs:208-210 — `if asked.iter().any(|(_, held)| *held == priority) { return Err(priority_in_use()); }`
  same file:199-203 — the duplicate-RuleArn sibling case already answers `Scaffolded::validation_error(...)`, which is the shape this cause needs.
(ARNs, rule ids and load-balancer/target-group names in the recording are per-run values and are not part of the requirement.)

## Steps to Reproduce
1. Boot the emulator. Create a VPC with two subnets in distinct AZs, an internal application load balancer and an HTTP:80 listener with a fixed-response default action.
2. CreateRule twice on that listener: rule A at Priority 20 and rule B at Priority 21 (any path-pattern condition, fixed-response action).
3. Positive control: SetRulePriorities(RulePriorities=[{rule A, 22}, {rule B, 23}]) — it must succeed and DescribeRules must report 22 and 23. (It does today.)
4. Call SetRulePriorities(RulePriorities=[{RuleArn: ruleA, Priority: 30}, {RuleArn: ruleB, Priority: 30}]) — the same priority twice in one request. Observe HTTP 400 with Error.Code 'PriorityInUse'; AWS answers HTTP 400 with Error.Code 'ValidationError' and Error.Message "Priority '30' was provided multiple times" (the 30 comes from the request).
5. DescribeRules again and confirm the refusal applied nothing: rule A is still 22 and rule B is still 23.
6. Re-replay elbv2.set-rule-priorities-duplicate-fails.
