Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling__dynamodb__ecs.same-bounds-change-is-parameterised-by-namespace-not-special-cased
Issue-Key: application-autoscaling__dynamodb__ecs.same-bounds-change-is-parameterised-by-namespace-not-special-cased|_poll_until#61|Timeout|return last
Issue-Commit: f520e482964e34c3af4b1cbe916bb0f71c86032d

# application-autoscaling__dynamodb__ecs: the namespace-parameterised same-bounds RegisterScalableTarget scenario never reaches a verdict

> _Note: this issue was identified against the code base at commit `f520e482964e34c3af4b1cbe916bb0f71c86032d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterScalableTarget

## Description
Cause 1 of 1: a same-bounds RegisterScalableTarget exercised across two service namespaces (ecs and dynamodb) never answers within the check's budget.
AWS: Smithy (application-autoscaling): RegisterScalableTarget is a synchronous call — re-registering a target with MinCapacity == MaxCapacity equal to the current bounds returns 200 with no change, and the same code path serves every ServiceNamespace (the namespace selects the resource ID / dimension grammar, not a special case). DescribeScalableTargets then reports the registered target promptly.
Emulator: recorded behavior only — the check exhausts its 60s budget with no verdict, so neither the ecs leg nor the dynamodb leg of the same-bounds re-registration produces an observable answer.
Why: a client that re-registers an unchanged scalable target (a very common idempotent-apply pattern in IaC) gets a hang rather than a prompt 200, and cannot tell that the behaviour is namespace-independent.
Fix: the RegisterScalableTarget handler and the scalable-target state it reads back through DescribeScalableTargets, for the ecs and dynamodb namespaces alike.
Traces: application-autoscaling__dynamodb__ecs.same-bounds-change-is-parameterised-by-namespace-not-special-cased.

## Evidence
Error: Check timed out after 60.0s — the check produced no verdict. Expected the same-bounds RegisterScalableTarget to return promptly (HTTP 200) for both the ecs:service:DesiredCount and dynamodb:table:ReadCapacityUnits dimensions and DescribeScalableTargets to read the target back.

## Steps to Reproduce
1. RegisterScalableTarget for ServiceNamespace ecs, ScalableDimension ecs:service:DesiredCount with MinCapacity == MaxCapacity.
2. Re-issue the identical RegisterScalableTarget, then DescribeScalableTargets.
3. Repeat for ServiceNamespace dynamodb / dynamodb:table:ReadCapacityUnits.
4. The check never reaches a verdict inside its 60s budget.
