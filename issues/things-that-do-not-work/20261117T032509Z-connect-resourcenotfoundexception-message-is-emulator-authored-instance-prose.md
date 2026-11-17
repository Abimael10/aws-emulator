Issue-Source: trace-sweep-divergence
Issue-Trace: connect-ccp-traj-connect-instance-detour
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connect-ccp-traj-connect-instance-detour connect-queue-crud-validation
Issue-Shape-Id: com.amazonaws.connect#AmazonConnectService
Issue-Key: connect|DescribeInstance|4xx->4xx|$.Message
Issue-Key: connect|CreateQueue|4xx->4xx|$.Message
Issue-Key: connect|DescribeQueue|4xx->4xx|$.Message
Issue-Key: connect|DeleteQueue|4xx->4xx|$.Message
Issue-Commit: d8c7eba5175b77c2c6fcf7e7602360fd9434b57f

# connect: ResourceNotFoundException Message is emulator-authored instance prose instead of AWS's fixed "Resource not found" text

> _Note: this issue was identified against the code base at commit `d8c7eba5175b77c2c6fcf7e7602360fd9434b57f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstance

## Description
Every Amazon Connect operation that takes an InstanceId httpLabel and is handed an instance id that does not exist answers the correct error shape and the correct HTTP status (404 ResourceNotFoundException), but fills the shape's `Message` member with emulator-authored prose that names the rejected instance id ("Instance <instanceId> not found") instead of AWS's fixed text for this error class, "Resource not found". The fault is shared by all four causes below: a single not-found helper in the Connect error path composes its own message rather than emitting the service's wording, so the divergence appears identically on the instance-scoped read and on all three queue lifecycle operations.

Cause 1 of 4: DescribeInstance on an unknown InstanceId.
  AWS: Smithy: DescribeInstance GET /instance/{InstanceId}, InstanceId @httpLabel @required @length(1,100); modeled error ResourceNotFoundException (errorKind client, @httpError 404) with a single member Message ("The message about the resource"); shape doc "The specified resource was not found." The model does not fix the message text, so the requirement is taken from the recorded response: the body's Message is exactly "Resource not found" — the whole string is fixed text for this error class, with NO variable part (the rejected instance id is NOT interpolated into it).
  Emulator: answers 404 ResourceNotFoundException (status matches) but with $.Message = "Instance <instanceId> not found", i.e. a different template whose variable part is the InstanceId taken from the request path.
  Why: a client that branches on, logs, or asserts the error message for a missing Connect instance sees text AWS never emits; message-matching retry/alerting rules and recorded-response comparisons break, and the emulator additionally echoes a caller-supplied identifier into an error string where AWS deliberately does not.
  Fix: the Connect not-found error path — the shared helper that builds ResourceNotFoundException for an unresolved instance (operation handler/validation for DescribeInstance and its serializer of the error body). It must emit the fixed text "Resource not found" with no interpolation.
  Traces: connect-ccp-traj-connect-instance-detour#task#4 (cluster also covers connect-ccp-traj-connect-instance-detour#task#5).

Cause 2 of 4: CreateQueue on an unknown InstanceId.
  AWS: Smithy: CreateQueue PUT /queues/{InstanceId}, InstanceId @httpLabel @required; modeled errors include ResourceNotFoundException (client, @httpError 404) carrying only Message. Recorded behavior: 404 with Message exactly "Resource not found" (all fixed text, no request-derived part).
  Emulator: 404 ResourceNotFoundException with $.Message = "Instance <instanceId> not found".
  Why: the create leg of a queue lifecycle against a non-existent instance surfaces non-AWS wording, so a client that keys off the message cannot tell this refusal apart from its own ad-hoc errors.
  Fix: same Connect not-found helper, reached from the CreateQueue handler's instance-existence check.
  Traces: connect-queue-crud-validation#task#2.

Cause 3 of 4: DescribeQueue on an unknown InstanceId.
  AWS: Smithy: DescribeQueue GET /queues/{InstanceId}/{QueueId}, both members @httpLabel @required; ResourceNotFoundException (client, 404), member Message. Recorded behavior: 404 with Message exactly "Resource not found".
  Emulator: 404 ResourceNotFoundException with $.Message = "Instance <instanceId> not found".
  Why: same as above for the read leg — the emulator reveals which path segment failed to resolve, which AWS does not, and the message text diverges.
  Fix: same Connect not-found helper, reached from the DescribeQueue handler's instance-existence check.
  Traces: connect-queue-crud-validation#task#3.

Cause 4 of 4: DeleteQueue on an unknown InstanceId.
  AWS: Smithy: DeleteQueue DELETE /queues/{InstanceId}/{QueueId}; ResourceNotFoundException (client, 404), member Message. Recorded behavior: 404 with Message exactly "Resource not found".
  Emulator: 404 ResourceNotFoundException with $.Message = "Instance <instanceId> not found".
  Why: same as above for the delete leg; idempotent-delete handling that inspects the message diverges from AWS.
  Fix: same Connect not-found helper, reached from the DeleteQueue handler's instance-existence check.
  Traces: connect-queue-crud-validation#task#6.

No open issue covers this: the only similar filed item is "ce tagging ResourceNotFoundException diverges: served 404 with the model's doc sentence instead of 400 'Resource not found'" (20261116T095207Z-ce-tagging-...), which is the Cost Explorer service and a different status, not Connect.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, 16 steps, 9 passed, 7 mismatched). Four mismatch clusters, all service=connect, all status 404 expected -> 404 actual, all keyed on $.Message:

1. DescribeInstance — fixture connect-ccp-traj-connect-instance-detour, step task#4 (divergence_key service=connect|DescribeInstance|status=404->404|keys=$.Message; affected: task#4, task#5)
   expected $.Message: "Resource not found"
   actual   $.Message: "Instance d6f2a9c1-5e3b-4a7d-9c21-8f4e6b2d1a70 not found"
2. CreateQueue — fixture connect-queue-crud-validation, step task#2
   expected $.Message: "Resource not found"; actual: "Instance <instanceId> not found"
3. DescribeQueue — fixture connect-queue-crud-validation, step task#3
   expected $.Message: "Resource not found"; actual: "Instance <instanceId> not found"
4. DeleteQueue — fixture connect-queue-crud-validation, step task#6
   expected $.Message: "Resource not found"; actual: "Instance <instanceId> not found"

In every cluster the HTTP status and the error shape (ResourceNotFoundException, @httpError 404 per the Connect Smithy model) are correct; only the Message member's text diverges. The uuid shown is the fixture's instance id — it is a per-recording value, not part of the requirement; the requirement is that Message is the fixed string "Resource not found".

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture connect-ccp-traj-connect-instance-detour, step task#4 — GET /instance/{InstanceId} for an instance id that was never created. Observe 404 with body {"Message": "Instance <instanceId> not found"}; AWS answers 404 ResourceNotFoundException with {"Message": "Resource not found"}.
3. Cause 2: replay connect-queue-crud-validation step task#2 — PUT /queues/{InstanceId} (CreateQueue) against the same unknown instance id. Same Message divergence.
4. Cause 3: replay connect-queue-crud-validation step task#3 — GET /queues/{InstanceId}/{QueueId} (DescribeQueue). Same Message divergence.
5. Cause 4: replay connect-queue-crud-validation step task#6 — DELETE /queues/{InstanceId}/{QueueId} (DeleteQueue). Same Message divergence.
6. All four must answer $.Message == "Resource not found" before the issue is closed.
