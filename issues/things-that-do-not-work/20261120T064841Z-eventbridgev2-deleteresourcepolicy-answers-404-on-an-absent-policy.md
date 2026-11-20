Issue-Source: trace-sweep-divergence
Issue-Trace: eventbridgev2-describe-and-policy-against-missing-resources
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eventbridgev2-describe-and-policy-against-missing-resources eventbridgev2-resource-policy-public-access-and-reserved-names
Issue-Shape-Id: com.amazonaws.eventbridgev2#AWSEventsV2
Issue-Key: eventbridgev2|DeleteResourcePolicy|2xx->4xx|$
Issue-Key: eventbridgev2|DeleteResourcePolicy|2xx->4xx|status
Issue-Key: eventbridgev2|DeleteResourcePolicy|4xx->4xx|$.Message
Issue-Commit: 5f184b6d595470414f1315ced82dbc505d6a982b

# eventbridgev2: DeleteResourcePolicy answers 404 on an absent policy and mis-words the AWS_RAM refusal

> _Note: this issue was identified against the code base at commit `5f184b6d595470414f1315ced82dbc505d6a982b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteResourcePolicy

## Description
Two net-new wire causes in the eventbridgev2 DeleteResourcePolicy path. Both must be closed before this issue is submitted.

Cause 1 of 2: DeleteResourcePolicy on an event bus that has no named policy attached is refused instead of succeeding.
AWS: DeleteResourcePolicy is modelled @idempotent (Smithy: DeleteResourcePolicy, input ResourceArn required + PolicyName defaulting to "default", output DeleteResourcePolicyResponse{RevisionId}). Deleting a policy name that is not attached is a no-op that answers HTTP 200 with a body carrying no required members (the recording shows an empty JSON body, {}). ResourceNotFoundException (404) is reserved for a missing event bus, not for a missing policy on an existing bus — the read path is what reports an unattached policy (GetResourcePolicy answers 404 "No 'default' resource policy is attached to this event bus").
Emulator: answers HTTP 404 with __type ResourceNotFoundException and Message "Resource does not exist" — i.e. it treats the absent policy as an absent resource, and the message is emulator-authored besides.
Why: a client that tears down or reconciles a bus's policy (the common "delete, then put" or idempotent-cleanup pattern) gets an error from an operation AWS declares idempotent. Retried deletes and teardown scripts abort, and SDK retry/idempotency wrappers that rely on the 200 cannot converge.
Fix: the DeleteResourcePolicy handler / resource-policy state lookup in the eventbridgev2 crate — distinguish "bus not found" (404 ResourceNotFoundException) from "policy name not attached" (200, empty response) instead of failing both.
Traces: eventbridgev2-describe-and-policy-against-missing-resources#task#6.

Cause 2 of 2: the AWS_RAM-policy refusal message is emulator-authored.
AWS: PolicyName is exclusive in both directions (Smithy: DeleteResourcePolicyRequest.PolicyName doc — "only Resource Access Manager can delete \"AWS_RAM\", and only the bus owner can delete \"default\", so naming the other party's policy fails with AccessDeniedException"; AccessDeniedException httpError 403). The owner naming AWS_RAM is refused with HTTP 403, __type com.amazon.nexus.v20250515.exceptions#AccessDeniedException and the message "Only AWS Resource Access Manager can modify the AWS_RAM resource policy". The whole sentence is fixed text; the only variable part is the reserved policy name AWS_RAM, taken from the request's PolicyName.
Emulator: status 403 is right, but the Message reads "The AWS_RAM resource policy is written exclusively by AWS Resource Access Manager" — different wording for the same error class. (The PutResourcePolicy twin of this wording is already covered by the open issue 20261111T025659Z-eventbridgev2-constraint-validation-refusals-answer-validationexception-with-no.md; the DeleteResourcePolicy occurrence is not carried by it or any other open issue.)
Why: clients and operators match on the refusal text to tell the RAM-owned policy apart from an ordinary authorization failure; emulator-authored wording makes that classification and any log/alarm pattern built on it fail.
Fix: the message template used by the DeleteResourcePolicy handler's reserved-policy-name validation — emit AWS's fixed sentence with the requested policy name interpolated.
Traces: eventbridgev2-resource-policy-public-access-and-reserved-names#task#6.

## Evidence
Cause 1 — fixture eventbridgev2-describe-and-policy-against-missing-resources, step task#6, DeleteResourcePolicy:
  expected: status 200, body {}
  actual:   status 404, body {"__type": "ResourceNotFoundException", "Message": "Resource does not exist"}

Cause 2 — fixture eventbridgev2-resource-policy-public-access-and-reserved-names, step task#6, DeleteResourcePolicy(PolicyName=AWS_RAM):
  expected: status 403, $.Message = 'Only AWS Resource Access Manager can modify the AWS_RAM resource policy'
  actual:   status 403, $.Message = 'The AWS_RAM resource policy is written exclusively by AWS Resource Access Manager'

Smithy evidence: DeleteResourcePolicy is @idempotent; PolicyName defaults to "default"; AccessDeniedException httpError 403; ResourceNotFoundException httpError 404.

## Steps to Reproduce
1. Replay the wire corpus (trace set simparity-wire-aws-v1) against the emulator.
2. Cause 1: create an event bus, attach no resource policy, call DeleteResourcePolicy(ResourceArn=<bus arn>) — replay fixture eventbridgev2-describe-and-policy-against-missing-resources step task#6. Observe 404 ResourceNotFoundException where AWS answers 200 with an empty body.
3. Cause 2: on an existing bus call DeleteResourcePolicy(ResourceArn=<bus arn>, PolicyName="AWS_RAM") — replay fixture eventbridgev2-resource-policy-public-access-and-reserved-names step task#6. Observe the 403 AccessDeniedException message text diverging from AWS's wording.
