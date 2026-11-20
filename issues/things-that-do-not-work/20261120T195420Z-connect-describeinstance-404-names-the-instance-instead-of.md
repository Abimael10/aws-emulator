Issue-Source: trace-sweep-divergence
Issue-Trace: connect-ccp-traj-connect-instance-detour
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connect-ccp-traj-connect-instance-detour
Issue-Shape-Id: com.amazonaws.connect#AmazonConnectService
Issue-Key: connect|DescribeInstance|4xx->4xx|$.Message
Issue-Commit: 40a8392fd0198e21a06fbe4eb97ae7b924b10207

# connect DescribeInstance 404 names the instance instead of AWS's fixed "Resource not found" Message

> _Note: this issue was identified against the code base at commit `40a8392fd0198e21a06fbe4eb97ae7b924b10207`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstance

## Description
One net-new cause on the Amazon Connect instance read path. DescribeInstance handed an InstanceId this account holds no instance for answers the right status and the right error shape, but words the shape's Message with emulator-authored prose that echoes the rejected instance id instead of the fixed sentence AWS states for this error class.

Cause 1 of 1: DescribeInstance on an InstanceId that was never allocated answers 404 ResourceNotFoundException whose Message interpolates the requested instance id.
AWS: Smithy (services/service-com-amazonaws-connect-amazonconnectservice/smithy-model/com-amazonaws-connect-amazonconnectservice.json) models DescribeInstance as GET /instance/{InstanceId} (success 200, output Instance + ReplicationConfiguration) with input InstanceId (@httpLabel @required @length 1..100); its modeled errors are InternalServiceException (500), InvalidRequestException (400) and ResourceNotFoundException (errorKind client, @httpError 404) whose single member is Message, shape doc "The specified resource was not found." The model does not fix the Message text, so the requirement is recorded behavior: for an instance id that names no instance, AWS answers HTTP 404 ResourceNotFoundException with Message exactly "Resource not found". That string is ENTIRELY fixed text for this error class — it has NO variable part, and in particular AWS does not interpolate the InstanceId the request carried in its path label, nor the resource kind.
Emulator: status (404) and error shape (ResourceNotFoundException) are correct, but Message is "Instance <instanceId> not found" — a different template whose fixed text is "Instance " + " not found" and whose variable part is the InstanceId taken from the request path. In the recorded replay the instance id the fixture addressed was echoed back verbatim in the message; that id is a per-recording value and is not part of any requirement. The handler is explicit about doing this: services/service-com-amazonaws-connect-amazonconnectservice/src/ops/describe_instance.rs lines 174-182 resolve the store and, on a miss, build crate::state::instance_not_found(&input.instance_id) rather than the service's generic not-found refusal, and services/service-com-amazonaws-connect-amazonconnectservice/src/state/refusals.rs lines 41-80 encode that split as a deliberate rule (RESOURCE_NOT_FOUND_MESSAGE = "Resource not found" only for a resource INSIDE an instance, instance_not_found() for an operation whose addressed resource IS the instance). The recording contradicts that rule for DescribeInstance.
Why: a client that branches on, logs, alerts on, retries from or asserts the refusal text for a missing Connect instance sees a sentence AWS never emits, so message-matching error handling and recorded-response comparisons break; the emulator additionally echoes a caller-supplied identifier into an error body where AWS deliberately does not, which both leaks request content into the error surface and makes the refusal distinguishable from the real service.
Fix: DescribeInstance's handler/validation (the failed-instance-lookup arm in describe_instance.rs) must raise its modeled ResourceNotFoundException through the service's generic not-found refusal so the body carries the fixed text "Resource not found" with nothing interpolated, and the refusals module's documented split between the generic sentence and the instance-naming one must be corrected for this operation. Hypothesis (neither the model nor this recording states it): the instance-naming spelling may be what AWS uses specifically for an id that named an instance until a DeleteInstance retired it, which is the case the behavioral check connect.instance-deleted-id-refuses-read-and-mutation asserts (it expects DescribeInstance on a RETIRED id to answer "Instance <instanceId> not found"); this fixture's id was never created. If both are true the handler must discriminate never-allocated from retired, and a fix that simply replaces one template with the other will break that behavioral check — so re-run both.
Related but NOT covering this: fixed issue 20261117T032509Z-connect-resourcenotfoundexception-message-is-emulator-authored-instance-prose.md filed exactly this cluster and is closed, so this is a regression (or an incomplete close) of it and is net-new here; open issue 20261117T125330Z-connect-instance-attribute-reads-diverge-unmodeled-validationexception-refusal.md owns the DescribeInstanceAttribute/ListInstanceAttributes divergences in the same corpus, not this one; open issue 20261120T175054Z-connect-updateinstanceattribute-refuses-a-deleted-instance-id-with.md is the UpdateInstanceAttribute mutation leg on a deleted id, a different operation and a different template.
Traces: connect-ccp-traj-connect-instance-detour#task#4 (same cluster also covers connect-ccp-traj-connect-instance-detour#task#5).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator up, 25 steps, 18 passed, 7 mismatched, score 72.0%. One net-new mismatch cluster (the other two clusters in the run are owned by open issue 20261117T125330Z-...):

Fixture connect-ccp-traj-connect-instance-detour, step task#4, service connect, operation DescribeInstance, verdict mismatch, expected status 404 / actual status 404.
divergence_key: service=connect|DescribeInstance|status=404->404|keys=$.Message
affected (2): connect-ccp-traj-connect-instance-detour#task#4, connect-ccp-traj-connect-instance-detour#task#5

  $.Message: value_mismatch
    expected: "Resource not found"
    actual:   "Instance d6f2a9c1-5e3b-4a7d-9c21-8f4e6b2d1a70 not found"

What is fixed text and what is a value: AWS's message is the whole fixed string "Resource not found" with no variable part. The emulator's message is the template "Instance " + <InstanceId from the request's path label> + " not found"; the uuid d6f2a9c1-5e3b-4a7d-9c21-8f4e6b2d1a70 above is the fixture's own instance id, a per-recording value that is NOT part of the requirement. HTTP status (404) and the modeled error shape (ResourceNotFoundException, @httpError 404) both match; only the Message member's text diverges, and the divergence is the template, not the value.

Code citation for the cause: services/service-com-amazonaws-connect-amazonconnectservice/src/ops/describe_instance.rs lines 178-182 — the `store.instances.get(&input.instance_id).ok_or_else(...)` arm constructs `crate::state::instance_not_found(&input.instance_id)`; services/service-com-amazonaws-connect-amazonconnectservice/src/state/refusals.rs line 50 defines RESOURCE_NOT_FOUND_MESSAGE = "Resource not found" and lines 76-80 define instance_not_found(instance_id) producing "Instance {instance_id} not found".

## Steps to Reproduce
1. Clone this repository and boot the emulator built from the checkout; confirm connect is served (/__admin/health answers 200 and the data listener accepts).
2. Replay wire trace set simparity-wire-aws-v1.
3. Replay fixture connect-ccp-traj-connect-instance-detour, step task#4 — GET /instance/{InstanceId} (DescribeInstance) for an instance id this account never created.
4. Observe HTTP 404 with body {"Message": "Instance <the InstanceId sent> not found"}. AWS answers HTTP 404 ResourceNotFoundException with body {"Message": "Resource not found"} — fixed text, nothing interpolated.
5. Step task#5 of the same fixture repeats the same call and diverges identically (same cluster).
6. The issue is closed when DescribeInstance on an unallocated InstanceId answers 404 ResourceNotFoundException with $.Message == "Resource not found" and no request-derived text, while the behavioral check connect.instance-deleted-id-refuses-read-and-mutation (DescribeInstance on an id retired by DeleteInstance) still passes.
