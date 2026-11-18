Issue-Source: trace-sweep-divergence
Issue-Trace: network-security-manager-draft-rule-lifecycle
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: network-security-manager-draft-rule-lifecycle network-security-manager-rule-lifecycle network-security-manager-rule-identifier-validation network-security-manager-aggregate-sync-status-bounds
Issue-Shape-Id: com.amazonaws.networksecuritymanager#PiccoloCustomerAPIService
Issue-Key: network-security-manager|GetRule|4xx->2xx|$.message
Issue-Key: network-security-manager|GetRule|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: network-security-manager|GetRule|4xx->2xx|status
Issue-Key: network-security-manager|GetRule|4xx->4xx|$.message
Issue-Key: network-security-manager|GetRule|4xx->4xx|$.reason
Issue-Key: network-security-manager|GetRule|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: network-security-manager|GetRule|4xx->4xx|status
Issue-Key: network-security-manager|CreateRule|2xx->2xx|$.updatedAt
Issue-Key: network-security-manager|GetRule|2xx->2xx|$.updatedAt
Issue-Key: network-security-manager|ListAggregateResourceSynchronizationStatuses|4xx->4xx|$.message
Issue-Commit: 21d8bcced886adf941fa2f237320756a3905ee2e

# network-security-manager rule read path and response serialization diverge: 4 causes — GetRule miss path answers 200, non-ARN ruleIdentifier not rejected, updatedAt serialized as epoch number, enum-value-set message mis-ordered

> _Note: this issue was identified against the code base at commit `21d8bcced886adf941fa2f237320756a3905ee2e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetRule

## Description
Four distinct, net-new defects in the Network Security Manager (PiccoloCustomerAPIService, restJson1) rule read path and in the service's shared response/error serialization, all observed replaying the simparity-wire-aws-v1 wire corpus against the emulator built from this checkout. The replay reports empty `stub_path` and empty `owned_by` for every cluster below.

Related open issues, none of which owns any cause here: '20261109T164319Z-network-security-manager-create-entity-operations-diverge-3.md' owns only the CreatePolicy WAF-config validation, the CreateRule @pattern-anchor message and CreateScope's 501 (its Cause 2 is the pattern clause of the constraint renderer, a different clause from Cause 4 below, and it says nothing about timestamp serialization or the GetRule read path); '20261117T225331Z-network-security-manager-read-filter-surface-diverges-2.md' is a BEHAVIORAL issue whose Cause 2 faults the same enum-value-set ordering clause on ListTemplates.status but carries no wire replay key, so the ListAggregateResourceSynchronizationStatuses step in Cause 4 is not covered by it and the replay reports no owner for it; '20261109T113448Z-cloudformation-...' is a CloudFormation registry issue.

=== Cause 1 of 4: GetRule still resolves a rule identifier that must no longer resolve — HTTP 200 with the rule body instead of the modeled 404 ResourceNotFoundException ===
AWS: Smithy model com.amazonaws.networksecuritymanager#GetRule — GET /rules/{ruleIdentifier}, success @http code 200; input ruleIdentifier is @httpLabel + @required (RuleIdentifier, @length 1..1010), documented "The identifier of the rule. This is the rule's Amazon Resource Name (ARN)." Modeled errors are AccessDeniedException (403), InternalServerException (500), ResourceNotFoundException (404), ThrottlingException (429), ValidationException (400). ResourceNotFoundException is documented "The specified resource was not found. Verify that the resource identifier is correct and that the resource exists, then try your request again." and carries @required `message` plus optional `resourceId`/`resourceType`. When a rule ARN names a rule that does not exist (in the recorded lifecycle, the rule the fixture's earlier teardown/delete step removed), the recorded AWS response is HTTP 404 with an x-amzn-errortype response header naming the error shape and Error.Message "Rule not found: <ruleId>" — fixed text "Rule not found: " followed by one variable part, the rule id taken from the request's ruleIdentifier (the recording's own id value is just one run's fill of that template). A rule, once removed, must never again be returned by this read.
Emulator: answers HTTP 200 and serializes the full GetRuleOutput for that identifier. The replay reports `status: expected 404, got 200`, `$.message: missing (expected 'Rule not found: <ruleId>', got None)` and `$headers.x-amzn-errortype: missing`. The rule is still present in the rule store at that point in the lifecycle, so GetRule's miss path never runs. Hypothesis: the lifecycle's rule-removal step is the unimplemented DeleteRule scaffold (it answers 501 and its own stub issue owns that operation), so nothing ever drops the entity from state; the requirement filed here is GetRule's own contract — a removed/unresolvable rule must produce the modeled 404, with AWS's message wording, not a 200 body.
Why: a client cannot tell a live rule from a deleted one. Existence probes and idempotent teardown loops ("delete, then confirm GetRule raises ResourceNotFoundException") never terminate the way they do on AWS, `except nsm.exceptions.ResourceNotFoundException` is dead code, and a caller re-reads and re-deploys a rule AWS would consider gone.
Fix: GetRule's handler and the rule entity state it reads (the lookup that resolves ruleIdentifier against the rule store must not return entities that have been removed), plus the ResourceNotFoundException message text its error path constructs.
Traces: network-security-manager-draft-rule-lifecycle#task#4 (cluster also covers network-security-manager-rule-lifecycle#task#6).

=== Cause 2 of 4: GetRule accepts a bare, non-ARN ruleIdentifier and answers 404 ResourceNotFoundException where AWS refuses it with 400 ValidationException ===
AWS: Smithy model GetRuleInput.ruleIdentifier — RuleIdentifier (@length 1..1010), @httpLabel, @required, documented "The identifier of the rule. This is the rule's Amazon Resource Name (ARN)." The identifier is therefore required to BE an ARN (the output member ruleArn carries @resourceIdentifier("ruleIdentifier") and RuleArn's @pattern is ^arn(:[a-z0-9]+([.-][a-z0-9]+)*){2}(:([a-z0-9]+([.-][a-z0-9]+)*)?){2}:(.+)$), and a value that is not an ARN is a field-validation failure, not a lookup miss. GetRule models ValidationException (@error client, @httpError 400) with @required `message`, `reason` (ValidationExceptionReason = ACCOUNT_NOT_ONBOARDED | FIELD_VALIDATION_FAILED | OTHER) and `fieldList`. For a ruleIdentifier that is a bare rule id rather than an ARN, the recorded AWS response is HTTP 400 with reason "FIELD_VALIDATION_FAILED", an x-amzn-errortype header naming the error shape, and Error.Message "ruleIdentifier must be the resource's ARN." — that message is entirely fixed text for this error class, naming the offending member; no part of it comes from the request or state.
Emulator: answers HTTP 404 with x-amzn-errortype "ResourceNotFoundException" and Error.Message 'no rule answers to the identifier "<the submitted value>"', and emits no `reason` member at all. The replay reports `status: expected 400, got 404`, `$.message: value_mismatch`, `$.reason: missing (expected 'FIELD_VALIDATION_FAILED', got None)`, `$headers.x-amzn-errortype: shape_mismatch (got 'ResourceNotFoundException')`. The identifier resolver treats any string as a candidate key (bare id or ARN) and reports a miss instead of rejecting the shape of the input.
Why: the wrong error CLASS is returned. A client that passes an id where an ARN is required gets a retryable-looking 404 "not found" and concludes the rule was deleted, instead of the 400 that tells it its argument was malformed; code branching on ValidationException.reason == FIELD_VALIDATION_FAILED to correct the argument never fires, and 404-driven create-if-missing logic creates duplicates.
Fix: GetRule's input validation (the ruleIdentifier ARN-form check that must run before the state lookup) plus the ValidationException serializer, which must emit `message`, `reason` and the x-amzn-errortype header. The same identifier resolver is shared by the other rule reads, so the check belongs where ruleIdentifier is resolved.
Traces: network-security-manager-rule-identifier-validation#task#2.

=== Cause 3 of 4: `updatedAt` is serialized as a JSON epoch-seconds number instead of the modeled date-time string ===
AWS: Smithy model shape DateTimestamp — timestamp with @timestampFormat("date-time"); CreateRuleOutput.updatedAt and GetRuleOutput.updatedAt both target it. Under restJson1 a @timestampFormat("date-time") member is serialized as an RFC-3339/ISO-8601 STRING (e.g. "2026-11-09T16:43:19.860Z"), never as the protocol-default epoch-seconds number. The recorded AWS responses for CreateRule (201) and GetRule (200) carry updatedAt in that string form (the fixture asserts its shape, not one run's instant — the instant itself is a per-run value and is not a requirement).
Emulator: emits a JSON number of epoch seconds with a fractional part (e.g. 1794971374.860032) for $.updatedAt on both CreateRule's 201 and GetRule's 200, so the member fails its date-time shape assertion in every rule response.
Why: this is a wire-type divergence, not a formatting nicety. An SDK deserializing the modeled date-time member against a JSON number either errors out or yields a wrong value, so every client reading a rule's last-modified time breaks on a response AWS would have sent as a string. Because the defect is in the shared timestamp serialization for DateTimestamp, every entity response in this service that exposes updatedAt is affected, not just the rule ones the corpus reaches.
Fix: the response serializer for DateTimestamp members (the shared timestamp-rendering path used by CreateRule's and GetRule's output serializers) must honor @timestampFormat("date-time") and emit a string; the operations' handlers may also be producing a float-valued timestamp where a date-time value is required.
Traces: network-security-manager-draft-rule-lifecycle#task#1 (CreateRule; cluster also covers network-security-manager-rule-lifecycle#task#1) and network-security-manager-draft-rule-lifecycle#task#2 (GetRule; cluster also covers network-security-manager-rule-lifecycle#task#2).

=== Cause 4 of 4: the enum-constraint ValidationException message lists the allowed values in the model's declaration order instead of AWS's rendering order ===
AWS: Smithy model ListAggregateResourceSynchronizationStatuses — GET /aggregate-sync-statuses, input member synchronizationStatus is @httpQuery("synchronizationStatus") targeting enum SynchronizationStatus; the operation models ValidationException (@httpError 400, @required message). For a query value outside the enum, AWS answers HTTP 400 / ValidationException with the standard constraint report as Error.Message: "N validation error(s) detected: Value '<rejected value>' at '<memberName>' failed to satisfy constraint: Member must satisfy enum value set: [<allowed values>]". Fixed text is that whole sentence frame; variable parts are the error count, the rejected value and the member name (both from the request) and the allowed-value list (from the model). Recorded behavior only for the ORDER of that list: AWS renders it "[NOT_APPLICABLE, OUT_OF_SYNC, IN_SYNC]", which is NOT the model's declaration order (SynchronizationStatus declares IN_SYNC, OUT_OF_SYNC, NOT_APPLICABLE), so the renderer cannot simply emit declaration order. Hypothesis: AWS emits the enum in its service-internal order rather than the published model order.
Emulator: status 400 and the ValidationException code are correct, but the message ends "...Member must satisfy enum value set: [IN_SYNC, OUT_OF_SYNC, NOT_APPLICABLE]" — the bracketed list is in model declaration order, so the message text differs from AWS's for every @enum-constrained member in this service. Everything else in the message matches.
Why: the error message is part of the contract. Clients, tests and operator-facing surfaces that match or display AWS's constraint text see a different string, and because the defect is in the shared enum-constraint clause of the constraint-violation message renderer (reached by the centralized deserialize→validate walk before any handler runs), it mis-words every out-of-enum refusal in the service.
Fix: the enum-value-set clause of the shared constraint-violation message renderer used by the central input-validation stage (the same renderer whose @pattern clause is faulted by Cause 2 of 20261109T164319Z-network-security-manager-create-entity-operations-diverge-3.md, and whose enum clause the behavioral issue 20261117T225331Z-network-security-manager-read-filter-surface-diverges-2.md faults on ListTemplates.status without carrying this replay key) must emit the allowed-value list in AWS's order.
Traces: network-security-manager-aggregate-sync-status-bounds#task#6.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true, /__admin/health 200; 55 steps, 7 passed, score 12.7%). 9 divergence clusters, all service=network-security-manager; the 4 clusters owned by '20261109T164319Z-network-security-manager-create-entity-operations-diverge-3.md' are excluded. The 5 clusters below all report stub_path "" and owned_by [].

Cause 1 — fixture network-security-manager-draft-rule-lifecycle, step task#4, operation GetRule, verdict mismatch, affected_count 2 (also network-security-manager-rule-lifecycle#task#6):
  status: expected 404, got 200
  $.message: missing (expected 'Rule not found: <ruleId>' — recorded 'Rule not found: b172e9b41e2b7935b96c57712fc28f6f1', the id being the request-derived part — got None)
  $headers.x-amzn-errortype: missing (expected shaped, not asserted; got None)
  model: GetRule GET /rules/{ruleIdentifier} code 200; errors AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400. ResourceNotFoundException: required message + optional resourceId/resourceType.
  handler: services/service-com-amazonaws-networksecuritymanager-piccolocustomerapiservice/src/ops/get_rule.rs lines 181-194 (store.rule_by_identifier + the ResourceNotFoundException it builds). The rule-removal operation of the lifecycle is still a scaffold: services/.../src/ops/delete_rule.rs line 4 carries '//! SCAFFOLDED, NOT IMPLEMENTED:'.

Cause 2 — fixture network-security-manager-rule-identifier-validation, step task#2, operation GetRule, verdict mismatch:
  status: expected 400, got 404
  $.message: expected "ruleIdentifier must be the resource's ARN.", got 'no rule answers to the identifier "<submitted bare id>"'
  $.reason: expected 'FIELD_VALIDATION_FAILED', got None
  $headers.x-amzn-errortype: expected shaped (ValidationException), got 'ResourceNotFoundException'
  model: GetRuleInput.ruleIdentifier @httpLabel @required, doc "The identifier of the rule. This is the rule's Amazon Resource Name (ARN)."; ValidationException @httpError 400 {message required, reason: ValidationExceptionReason [ACCOUNT_NOT_ONBOARDED, FIELD_VALIDATION_FAILED, OTHER], fieldList}.
  handler: get_rule.rs lines 182-194 — any string is accepted and resolved, a miss is reported as 404.

Cause 3 — two clusters, same root cause:
  fixture network-security-manager-draft-rule-lifecycle step task#1 (CreateRule, status 201 both, affected_count 2 incl. network-security-manager-rule-lifecycle#task#1): $.updatedAt value_mismatch — expected shaped date-time, got 1794971374.860032
  fixture network-security-manager-draft-rule-lifecycle step task#2 (GetRule, status 200 both, affected_count 2 incl. network-security-manager-rule-lifecycle#task#2): $.updatedAt value_mismatch — expected shaped date-time, got 1794971374.860032
  model: DateTimestamp = timestamp with @timestampFormat "date-time"; CreateRuleOutput.updatedAt and GetRuleOutput.updatedAt both target DateTimestamp; protocol restJson1.
  handler: get_rule.rs line 209 (updated_at: Some(rule.updated_at_timestamp())) and the corresponding CreateRule output construction.

Cause 4 — fixture network-security-manager-aggregate-sync-status-bounds, step task#6, operation ListAggregateResourceSynchronizationStatuses, verdict mismatch (status 400 both):
  $.message expected "1 validation error detected: Value 'NOT_A_REAL_STATUS' at 'synchronizationStatus' failed to satisfy constraint: Member must satisfy enum value set: [NOT_APPLICABLE, OUT_OF_SYNC, IN_SYNC]"
  $.message actual   "1 validation error detected: Value 'NOT_A_REAL_STATUS' at 'synchronizationStatus' failed to satisfy constraint: Member must satisfy enum value set: [IN_SYNC, OUT_OF_SYNC, NOTAPPLICABLE-as-declared]" (actual list order: [IN_SYNC, OUT_OF_SYNC, NOT_APPLICABLE])
  difference is exactly the ordering of the bracketed allowed-value list.
  model: SynchronizationStatus enum values [IN_SYNC, OUT_OF_SYNC, NOT_APPLICABLE]; ListAggregateResourceSynchronizationStatusesInput.synchronizationStatus @httpQuery; ValidationException @httpError 400.

## Steps to Reproduce
Replay the simparity-wire-aws-v1 wire corpus against the emulator built from this checkout (replay_wire_corpus), or exercise the steps directly against the NSM endpoint:

Cause 1: POST /rules to create a rule, then run the fixture's rule-removal step for it, then GET /rules/<that rule's ARN> (botocore: nsm.get_rule(ruleIdentifier=arn)). Expect HTTP 404 with an x-amzn-errortype header and message "Rule not found: <the rule id from the identifier>"; observe HTTP 200 with the full rule body and no error members. Replay network-security-manager-draft-rule-lifecycle#task#4 (and network-security-manager-rule-lifecycle#task#6).

Cause 2: GET /rules/<a bare rule id, not an ARN> (botocore: nsm.get_rule(ruleIdentifier="sim2realprobensmbareid...")). Expect HTTP 400, x-amzn-errortype ValidationException, reason FIELD_VALIDATION_FAILED and message "ruleIdentifier must be the resource's ARN."; observe HTTP 404 ResourceNotFoundException with 'no rule answers to the identifier "..."' and no reason member. Replay network-security-manager-rule-identifier-validation#task#2.

Cause 3: POST /rules with a valid ruleName/firewallType/ruleType/configuration, then GET /rules/<returned ruleArn>. In both responses expect $.updatedAt to be an ISO-8601/RFC-3339 date-time STRING (per DateTimestamp's @timestampFormat date-time); observe a JSON number of epoch seconds with a fractional part. Replay network-security-manager-draft-rule-lifecycle#task#1 and #task#2 (and the same two steps of network-security-manager-rule-lifecycle).

Cause 4: GET /aggregate-sync-statuses?synchronizationStatus=NOT_A_REAL_STATUS (botocore: nsm.list_aggregate_resource_synchronization_statuses(synchronizationStatus="NOT_A_REAL_STATUS")). Expect HTTP 400 ValidationException whose message ends "...Member must satisfy enum value set: [NOT_APPLICABLE, OUT_OF_SYNC, IN_SYNC]"; observe the same message with the list in model declaration order [IN_SYNC, OUT_OF_SYNC, NOT_APPLICABLE]. Replay network-security-manager-aggregate-sync-status-bounds#task#6.
