Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: connect.instance-deleted-id-refuses-read-and-mutation
Issue-Shape-Id: com.amazonaws.connect#AmazonConnectService
Issue-Key: connect.instance-deleted-id-refuses-read-and-mutation|run#22|AssertionError|assert message2 == f"Instance not found: {instance_id}", (
Issue-Commit: 0fd0c8a38859f6782ef39e69b19293525b727c9f

# connect UpdateInstanceAttribute refuses a deleted instance id with the generic "Resource not found" text instead of AWS's "Instance not found: <instanceId>"

> _Note: this issue was identified against the code base at commit `0fd0c8a38859f6782ef39e69b19293525b727c9f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateInstanceAttribute

## Description
One net-new cause on the Amazon Connect instance-attribute mutation path. A DeleteInstance that has converged must leave the retired instance id refusing BOTH a read and a mutation with the wording AWS uses for each; the read leg is now correct and the mutation leg is not.

Cause 1 of 1: UpdateInstanceAttribute against an instance id whose DeleteInstance has converged answers 404 ResourceNotFoundException whose Message is the service-wide generic sentence instead of Connect's instance-not-found sentence for this operation.
AWS: Smithy (services/service-com-amazonaws-connect-amazonconnectservice/smithy-model/com-amazonaws-connect-amazonconnectservice.json) models UpdateInstanceAttribute as POST /instance/{InstanceId}/attribute/{AttributeType} (success 200, empty output) with input InstanceId (@httpLabel @required @length 1..100), AttributeType (@httpLabel @required, enum InstanceAttributeType), Value (@required @length 1..100) and ClientToken (@idempotencyToken); its modeled errors are InternalServiceException (500), InvalidParameterException (400), InvalidRequestException (400), ResourceNotFoundException (@httpError 404, single member Message, doc "The specified resource was not found.") and ThrottlingException (429). The model does not fix the Message text, so the requirement is recorded behavior: for an id that named an instance until a delete retired it, AWS answers 404 ResourceNotFoundException with Message "Instance not found: <instanceId>" — the fixed text is "Instance not found: " and the single variable part is the InstanceId the REQUEST carried in its path label. This is a DIFFERENT template from the one AWS uses on the read leg of the same scenario: DescribeInstance on the same retired id answers "Instance <instanceId> not found" (fixed text "Instance " + " not found", same variable part), and the check asserts both in one run, so the two sentences are genuinely distinct per operation and cannot be collapsed into one.
Emulator: status and error shape are right (404, ResourceNotFoundException) but Message is the fixed string "Resource not found", with no instance named. services/service-com-amazonaws-connect-amazonconnectservice/src/ops/update_instance_attribute.rs builds the refusal from crate::state::resource_not_found() — the service's generic helper, whose message constant is RESOURCE_NOT_FOUND_MESSAGE = "Resource not found" — rather than from crate::state::instance_not_found(&input.instance_id), the sibling helper that already exists in the same module for operations whose addressed resource IS the instance. So the handler classifies the instance as a mere container here, although the unresolved resource is the instance itself.
Why: a client that branches on, logs, retries from or asserts the refusal text for a mutation against a deleted Connect instance sees a sentence AWS never emits for this operation, and gets no indication of WHICH instance was missing; the read leg and the mutation leg of the same deleted id report two unrelated messages, so error handling that treats them uniformly (or matches AWS's text) breaks. It also makes the refusal indistinguishable from a missing resource INSIDE a live instance (an unknown queue, flow, hours-of-operation or storage association), which AWS does word "Resource not found".
Fix: UpdateInstanceAttribute's handler must word its ResourceNotFoundException with the instance-addressing sentence rather than the generic one — i.e. route the failed instance lookup through the instance-not-found refusal helper in the service's refusals module and add that operation's "Instance not found: <instanceId>" spelling there (it is a third template beside the existing generic sentence and DescribeInstance's "Instance <instanceId> not found"), documenting which operations state which. Related but NOT the same item: fixed issue 20261115T223209Z-connect-contact-lens-connect-describeinstance-refuses-a-deleted-instance-id-with.md corrected only the DescribeInstance read leg, and fixed issue 20261117T032509Z-connect-resourcenotfoundexception-message-is-emulator-authored-instance-prose.md established the generic "Resource not found" text for resources inside an instance; neither covers this operation, and open issue 20261117T125330Z-connect-instance-attribute-reads-diverge-unmodeled-validationexception-refusal.md covers DescribeInstanceAttribute/ListInstanceAttributes reads, not this mutation's 404 wording.
Traces: behavioral check connect.instance-deleted-id-refuses-read-and-mutation (the assertion that fails after the DescribeInstance leg has already passed).

## Evidence
Behavioral suite simparity-behavioral-aws-v1, check connect.instance-deleted-id-refuses-read-and-mutation, verdict mismatch:

  AssertionError: unexpected message mutating a deleted instance: 'Resource not found'
  (check_connect_instance_deleted_id_refuses_read_and_mutation.py line 166:
   assert message2 == f"Instance not found: {instance_id}")

Sequence actually exercised by the check: CreateInstance(IdentityManagementType=CONNECT_MANAGED, InstanceAlias=<unique>, InboundCallsEnabled=True, OutboundCallsEnabled=False) -> poll DescribeInstance to ACTIVE -> positive controls UpdateInstanceAttribute(AttributeType=CONTACTFLOW_LOGS, Value="true") and DescribeInstanceAttribute both succeed -> DeleteInstance -> poll until DescribeInstance answers ResourceNotFoundException.

After the delete converged:
  - DescribeInstance(InstanceId=<retired id>): PASSES — code ResourceNotFoundException, Message "Instance <instanceId> not found" as AWS words it.
  - UpdateInstanceAttribute(InstanceId=<retired id>, AttributeType=CONTACTFLOW_LOGS, Value="false"): code ResourceNotFoundException (correct, 404 per the model's @httpError), but
      expected $.Message: "Instance not found: <instanceId>"   (fixed text "Instance not found: " + the InstanceId from the request path)
      actual   $.Message: "Resource not found"                 (fixed text only; no instance named)

Code citation for the cause: services/service-com-amazonaws-connect-amazonconnectservice/src/ops/update_instance_attribute.rs lines 226-230 — the `store.instances.get_mut(&input.instance_id).ok_or_else(...)` arm constructs `crate::state::resource_not_found()`; services/service-com-amazonaws-connect-amazonconnectservice/src/state/refusals.rs line 50 defines RESOURCE_NOT_FOUND_MESSAGE = "Resource not found" and lines 76-80 define the unused-here `instance_not_found(instance_id)` producing "Instance {instance_id} not found". The concrete instance id in any one run is a per-run generated value and is not part of the requirement; the requirement is the template.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the behavioral suite check connect.instance-deleted-id-refuses-read-and-mutation.
2. CreateInstance with IdentityManagementType=CONNECT_MANAGED and a unique InstanceAlias; poll DescribeInstance until InstanceStatus is ACTIVE.
3. Confirm the positive controls: UpdateInstanceAttribute(InstanceId=<id>, AttributeType=CONTACTFLOW_LOGS, Value="true") succeeds and DescribeInstanceAttribute reads "true" back.
4. DeleteInstance(InstanceId=<id>) and poll DescribeInstance until it answers ResourceNotFoundException.
5. Call UpdateInstanceAttribute(InstanceId=<id>, AttributeType=CONTACTFLOW_LOGS, Value="false") on that retired id. Observe HTTP 404 ResourceNotFoundException with {"Message": "Resource not found"}; AWS answers {"Message": "Instance not found: <instanceId>"} with the request's InstanceId interpolated.
6. The issue is closed when the mutation leg answers Message == "Instance not found: <the InstanceId sent>" while the read leg (DescribeInstance) keeps answering Message == "Instance <the InstanceId sent> not found".
