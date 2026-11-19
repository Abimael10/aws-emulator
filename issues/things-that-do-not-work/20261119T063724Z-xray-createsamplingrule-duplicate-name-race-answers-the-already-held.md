Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: xray.concurrent-create-sampling-rule-duplicate-name
Issue-Shape-Id: com.amazonaws.xray#AWSXRay
Issue-Key: xray.concurrent-create-sampling-rule-duplicate-name|run#12|AssertionError|assert all((m or "").startswith(prefix) for m in messages), messages
Issue-Commit: 4fe54b35237bf2f896eb4fbbc78086fa4d0fefe7

# CreateSamplingRule duplicate-name race answers the already-held wording instead of the lost-race "different internalId" refusal

> _Note: this issue was identified against the code base at commit `4fe54b35237bf2f896eb4fbbc78086fa4d0fefe7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateSamplingRule

## Description
Cause 1 of 1: a set of CreateSamplingRule calls racing for the SAME RuleName are all refused with the service's arrival-read wording, never with the store's uniqueness-constraint wording that AWS answers a create that LOST the race.

AWS: Smithy: CreateSamplingRule(SamplingRule required; errors InvalidRequestException (HTTP 400, "The request is missing required parameters or has invalid parameters"), RuleLimitExceededException, ThrottledException). The model does not word the duplicate-name message, so this is recorded behavior only: X-Ray refuses a duplicate RuleName from two layers. A request whose RuleName the account ALREADY held when the request arrived is refused by the service's own read of the name with the fixed text "Sampling rule already exists" (no variable part). A request that read the name as FREE and then lost the write — i.e. every loser of a concurrent race for one name — is refused by the store's uniqueness constraint with InvalidRequestException and the message "A resource with the same resourceName but a different internalId already exists: <internalId>": the fixed text is "A resource with the same resourceName but a different internalId already exists: " and the only variable part is the internal id of the resource that WON the name, read from service state (no modelled member exposes it). Exactly one racer succeeds and the rule then exists exactly once.

Emulator: with 5 concurrent CreateSamplingRule calls carrying one identical RuleName, exactly one wins and the four losers are refused 400 InvalidRequestException (status and code correct), but all four messages are "Sampling rule already exists" — the arrival-read wording. The lost-race wording is never produced, so the four losers are reported as if the name had already been held before any of them arrived. Replay: AssertionError: ['Sampling rule already exists', 'Sampling rule already exists', 'Sampling rule already exists', 'Sampling rule already exists'] against the required prefix "A resource with the same resourceName but a different internalId already exists: ".

Why: a client cannot tell a pre-existing rule from a lost race. Control-plane provisioners that run N workers against one rule set use the two wordings differently — the already-held wording means "someone created this earlier, reconcile/adopt it", while the lost-race wording identifies, by internal id, the concurrently created resource that now owns the name and is a DIFFERENT resource from the one this worker tried to create (a name re-used after a delete gets a new id). Collapsing both into the already-held text makes a racing worker treat a concurrent sibling's resource as a stale pre-existing one, and loses the only identifier of the winning resource AWS ever discloses.

Fix: the CreateSamplingRule handler's concurrency seam — the arrival-time name read and the point at which another in-flight create is allowed to be served, in that operation's handler (the on_arrival value handed to the store insert). The two wordings and the selection between them already exist in the sampling-rule state module; what diverges is that every racing request's arrival read observes the winner's committed rule, so on_arrival is Held for all of them and the state's Free branch is unreachable. Requests racing on one name must each take their name read before any of them commits, so a loser is refused by the store's uniqueness constraint and reports the winner's internal id.

Traces: xray.concurrent-create-sampling-rule-duplicate-name (5 concurrent CreateSamplingRule calls with one RuleName; asserts 1 success, all losers InvalidRequestException, all loser messages starting with the lost-race prefix, and the rule present exactly once).

## Evidence
Check: xray.concurrent-create-sampling-rule-duplicate-name [mismatch]

Expected (recorded AWS): each of the 4 losing CreateSamplingRule calls -> HTTP 400, Code=InvalidRequestException, Message="A resource with the same resourceName but a different internalId already exists: <internalId of the winning rule>" (fixed text + the winner's internal id from service state).

Actual (emulator): each of the 4 losing calls -> HTTP 400, Code=InvalidRequestException (correct), Message="Sampling rule already exists".

Replay output:
  File "check_xray_concurrent_create_sampling_rule_duplicate_name.py", line 99, in run
    assert all((m or "").startswith(prefix) for m in messages), messages
  AssertionError: ['Sampling rule already exists', 'Sampling rule already exists', 'Sampling rule already exists', 'Sampling rule already exists']

The success count (1) and the error code (InvalidRequestException) assertions passed; only the message class diverges. Note the emulator already models both wordings (SAMPLING_RULE_EXISTS_MESSAGE vs SAMPLING_RULE_EXISTS_PREFIX / sampling_rule_exists_message, selected by SamplingRuleNameOnArrival in services/service-com-amazonaws-xray-awsxray/src/state/sampling_rules.rs, handed over from services/service-com-amazonaws-xray-awsxray/src/ops/create_sampling_rule.rs), so the divergence is that the Free/lost-race branch is never selected under a real race.

## Steps to Reproduce
1. Pick an unused RuleName N.
2. Issue 5 CreateSamplingRule calls CONCURRENTLY, each with SamplingRule={RuleName: N, ResourceARN: "*", Priority: <any 1..9999>, FixedRate: 0.05, ReservoirSize: 1, ServiceName: "*", ServiceType: "*", Host: "*", HTTPMethod: "*", URLPath: "*", Version: 1} and one tag.
3. Collect the outcomes: exactly 1 succeeds (correct) and 4 fail with 400 InvalidRequestException (correct).
4. Inspect the 4 failure messages. AWS: every one starts with "A resource with the same resourceName but a different internalId already exists: " followed by the winning rule's internal id. Emulator: every one is "Sampling rule already exists".
5. GetSamplingRules confirms the rule exists exactly once (both correct here).
