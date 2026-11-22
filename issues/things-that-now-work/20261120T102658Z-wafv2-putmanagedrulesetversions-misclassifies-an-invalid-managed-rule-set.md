Issue-Source: trace-sweep-divergence
Issue-Trace: wafv2-managed-rule-set-vendor-operations-on-fresh-account
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wafv2-managed-rule-set-vendor-operations-on-fresh-account
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2|PutManagedRuleSetVersions|4xx->4xx|$.Field
Issue-Key: wafv2|PutManagedRuleSetVersions|4xx->4xx|$.Parameter
Issue-Key: wafv2|PutManagedRuleSetVersions|4xx->4xx|$.Reason
Issue-Key: wafv2|PutManagedRuleSetVersions|4xx->4xx|$.__type
Issue-Key: wafv2|PutManagedRuleSetVersions|4xx->4xx|$.message
Issue-Commit: 4fa6913c0425f15c711952dd2e16101a0c8e32bc

# wafv2 PutManagedRuleSetVersions misclassifies an invalid managed-rule-set reference as WAFNonexistentItemException instead of WAFInvalidParameterException

> _Note: this issue was identified against the code base at commit `4fa6913c0425f15c711952dd2e16101a0c8e32bc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutManagedRuleSetVersions

## Description
This is ONE issue with ONE cause.

Cause 1 of 1: PutManagedRuleSetVersions, called on an account that owns no managed rule set, refuses with the wrong error class and drops every structured detail member of the refusal.

AWS: Smithy (com.amazonaws.wafv2#AWSWAF_20190729): PutManagedRuleSetVersions takes required Name (EntityName, @length 1..128, @pattern ^[\w\-]+$), Scope, Id (EntityId, @length 1..36, @pattern ^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$) and LockToken, and models WAFInvalidParameterException, WAFNonexistentItemException, WAFInvalidOperationException, WAFOptimisticLockException (all client, httpError 400) plus WAFInternalErrorException (500). WAF resolves the Name/Scope/Id triple into the managed rule set's resource ARN and validates that ARN's shape BEFORE any lookup, so a triple that does not form a valid managed-rule-set reference is a parameter-shape failure, not a lookup miss. Recorded refusal: HTTP 400, __type `WAFInvalidParameterException`, with the detail members that exception carries — Field = the fixed enum token `RESOURCE_ARN`, Parameter = the resolved ARN (fully variable: it is built from the request's Scope, the caller's account and region, and the request's Name and Id), Reason = the fixed sentence `The ARN isn't valid. A valid ARN begins with arn: and includes other information separated by colons or slashes.`, and message = the fixed scaffolding `Error reason: ` + that sentence + `, field: RESOURCE_ARN, parameter: ` + that same ARN. Only the ARN is a request/state-derived value; the Field token, the Reason sentence and the `Error reason: ... , field: ..., parameter: ...` framing are fixed text.

Emulator: HTTP 400 (the status is right) but __type is `WAFNonexistentItemException`, and the members Field, Parameter, Reason and message are all absent from the body — the handler goes straight to a managed-rule-set lookup, misses, and raises the generic not-found class with only its Message.

Why: a client distinguishes "the request names something that cannot exist — fix the request" from "the item is not there yet — create it or retry" purely by the exception class. Vendor tooling that publishes managed rule set versions sends WAFNonexistentItemException into a create-or-retry loop that can never converge, while the real fault (a malformed/unresolvable Name+Scope+Id reference) is never surfaced. The missing Field/Parameter/Reason members also remove the only machine-readable statement of WHICH parameter was wrong, so the caller cannot tell the bad reference from any other 400.

Fix: the reference-resolution step in the PutManagedRuleSetVersions handler's validation — derive the managed-rule-set ARN from Name/Scope/Id and classify an invalid reference as a RESOURCE_ARN WAFInvalidParameterException (with Field, Parameter, Reason and the composed message) before the state lookup that currently produces WAFNonexistentItemException. The same resolution/classification order is what the open behavioral issue 20261030T002309Z-wafv2-refusals-carry-the-wrong-class-or-not describes for GetTopPathStatisticsByTraffic (Cause 3 there); that issue is behavioral and carries none of this wire replay key, and 20261026T120000Z-wafv2-two-refusals-carry-the-right-class-but-not-wafs-own-sentence covers only PutManagedRuleSetVersions' invalid-version-key clause, so neither covers this refusal.

Traces: wafv2-managed-rule-set-vendor-operations-on-fresh-account#task#3.

## Evidence
Replay of simparity-wire-aws-v1 against the booted emulator (emulator_up=true, /__admin/health 200; 254/292 steps passed, score 87.0%).

Cluster service=wafv2|PutManagedRuleSetVersions|status=400->400|keys=$.Field,$.Parameter,$.Reason,$.__type,$.message, verdict=mismatch, fixture wafv2-managed-rule-set-vendor-operations-on-fresh-account step task#3, expected_status 400, actual_status 400, affected_count 1, owned_by empty, stub_path empty:

  $.__type: value_mismatch (expected 'WAFInvalidParameterException', got 'WAFNonexistentItemException')
  $.Field: missing (expected 'RESOURCE_ARN', got None)
  $.Parameter: missing (expected 'arn:aws:wafv2:us-east-1:000000000000:regional/rulegroup/does-not-exist/00000000-0000-0000-0000-000000000000', got None)
  $.Reason: missing (expected "The ARN isn't valid. A valid ARN begins with arn: and includes other information separated by colons or slashes.", got None)
  $.message: missing (expected "Error reason: The ARN isn't valid. A valid ARN begins with arn: and includes other information separated by colons or slashes., field: RESOURCE_ARN, parameter: arn:aws:wafv2:us-east-1:000000000000:regional/rulegroup/does-not-exist/00000000-0000-0000-0000-000000000000", got None)

The ARN above is this corpus's resolved value (account 000000000000, us-east-1, Name 'does-not-exist', Id all-zero UUID); the requirement is the fixed Field token `RESOURCE_ARN`, the fixed Reason sentence and the fixed message framing, with the ARN resolved from the request and the caller's account/region.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Re-replay fixture wafv2-managed-rule-set-vendor-operations-on-fresh-account, step task#3: on an account that owns no managed rule set, call PutManagedRuleSetVersions with Scope=REGIONAL, Name='does-not-exist', Id='00000000-0000-0000-0000-000000000000', a LockToken, and a VersionsToPublish/RecommendedVersion payload.
3. Expect HTTP 400 with __type 'WAFInvalidParameterException', Field 'RESOURCE_ARN', Parameter = the ARN resolved from the request (arn:aws:wafv2:<region>:<account>:regional/rulegroup/<Name>/<Id>), Reason "The ARN isn't valid. A valid ARN begins with arn: and includes other information separated by colons or slashes." and message "Error reason: <Reason>, field: RESOURCE_ARN, parameter: <that ARN>".
4. Observed: HTTP 400 with __type 'WAFNonexistentItemException' and no Field/Parameter/Reason/message members at all.
