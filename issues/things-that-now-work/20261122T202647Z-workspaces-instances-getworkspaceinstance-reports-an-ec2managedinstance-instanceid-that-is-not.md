Issue-Source: trace-sweep-divergence
Issue-Trace: workspaces-instances-managed-instance-allocated-and-running
Issue-Step: task#9
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: workspaces-instances-managed-instance-allocated-and-running
Issue-Shape-Id: com.amazonaws.workspacesinstances#EUCMIFrontendAPIService
Issue-Key: workspaces-instances|GetWorkspaceInstance|2xx->2xx|$.EC2ManagedInstance.InstanceId
Issue-Commit: 089d28114b41ce7f9e67fe8a9933352340958042

# GetWorkspaceInstance reports an EC2ManagedInstance.InstanceId that is not the managed instance bound to the WorkSpace Instance

> _Note: this issue was identified against the code base at commit `089d28114b41ce7f9e67fe8a9933352340958042`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetWorkspaceInstance

## Description
Cause 1 of 1: GetWorkspaceInstance serves an EC2ManagedInstance.InstanceId that does not correlate with the managed EC2 instance the rest of the session holds for that WorkSpace Instance. Fixture workspaces-instances-managed-instance-allocated-and-running, step task#9.

AWS: Smithy model services/service-com-amazonaws-workspacesinstances-eucmifrontendapiservice/smithy-model/com-amazonaws-workspacesinstances-eucmifrontendapiservice.json — GetWorkspaceInstance (input WorkspaceInstanceId, required, @length 15..70, @pattern ^wsinst-[0-9a-zA-Z]{8,63}$; output GetWorkspaceInstanceResponse with members WorkspaceInstanceErrors, EC2InstanceErrors, ProvisionState, WorkspaceInstanceId, EC2ManagedInstance, BillingConfiguration; errors AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400). Shape EC2ManagedInstance has one member, InstanceId (target String), documented as "Unique identifier of the managed EC2 instance."; CreateWorkspaceInstance is documented as "Launches a new WorkSpace Instance with specified configuration parameters" and the caller-visible failures are EC2's own (EC2InstanceError{EC2ErrorCode, EC2ExceptionType, EC2ErrorMessage}). So the value of this member is not a free-form identifier the WorkSpaces Instances service invents: it is the identity of the one EC2 instance that WorkSpace Instance IS, and the same value every other view of that instance must report. The recorded response for this step carries that member as the fixture's cross-step correlation handle for the managed instance id rather than a literal, i.e. the recording's requirement is an EQUALITY between the id already established for this WorkSpace Instance's managed instance and the id GetWorkspaceInstance reports. The model states nothing about the id's TEXT (no @pattern, no @length on EC2ManagedInstance$InstanceId), so the literal characters of any one run's id are not the requirement — the correlation is.

Emulator: the step answers HTTP 200 (status matches the recording) but $.EC2ManagedInstance.InstanceId diverges: the replay reports value_mismatch, expected the session's bound managed-instance id and got 'i-00000000000000001' — a value that did not resolve to the id the fixture had established for this WorkSpace Instance's managed instance, and which is the first value of the emulator's sequential instance-id counter. Every other asserted member of the response (ProvisionState, WorkspaceInstanceId, BillingConfiguration, the absent error lists) matched, so the record is found and settled correctly and only the managed-instance identity link is wrong.

Hypothesis: the WorkSpace Instance record's ec2_instance_id is populated from the in-process EC2 launch hop (state::launch, recorded by WorkspaceInstanceRecord::launched) and GetWorkspaceInstance echoes it verbatim, so the reported value is whatever that hop minted. The divergence is therefore an identity-propagation gap rather than a serializer bug: the id this service stores and reports is not the id the session's other views of the same managed instance carry, so the two sides of the WorkSpaces<->EC2 seam do not agree on which instance the WorkSpace Instance manages. A counter-seeded first id ('...0001') for a WorkSpace Instance created after other instances already exist in the session is consistent with the stored id being minted independently of, or ahead of, the instance EC2 actually holds.

Why: EC2ManagedInstance.InstanceId is the only handle a client gets for the instance behind a WorkSpace Instance, and clients use it directly against EC2 — ec2:DescribeInstances, ec2:CreateTags, ec2:StopInstances, ec2:AttachVolume, CloudWatch instance dimensions. When the reported id is not the instance that WorkSpace Instance actually manages, every such follow-on call is aimed at the wrong resource: a tag, stop or volume attach lands on an unrelated instance (or on none, with an InvalidInstanceID.NotFound), and a client that tags-then-describes to confirm its own WorkSpace Instance sees the managed instance as missing or mismatched. Any tooling that joins WorkSpaces Instances inventory to EC2 inventory on this id silently produces wrong joins rather than failing loudly.

Fix: the WorkSpaces Instances state that records the managed instance identity, and the create path that writes it — WorkspaceInstanceRecord.ec2_instance_id must hold exactly the InstanceId returned by the EC2 launch made for THAT record, and GetWorkspaceInstance must report that stored value unchanged. The read path (the GetWorkspaceInstance handler) already maps the stored Option<String> straight into EC2ManagedInstance{InstanceId} and needs no change; the defect belongs to how the id is obtained and stored at launch time, so the launch seam and the record update are where the identity link has to be made single-sourced.

Traces: workspaces-instances-managed-instance-allocated-and-running#task#9 (GetWorkspaceInstance, 200/200, $.EC2ManagedInstance.InstanceId value_mismatch). Code under suspicion for the Fix above: services/service-com-amazonaws-workspacesinstances-eucmifrontendapiservice/src/ops/get_workspace_instance.rs lines 199-209 (the read, which is faithful to the stored value); services/service-com-amazonaws-workspacesinstances-eucmifrontendapiservice/src/ops/create_workspace_instance.rs lines 462-464 and 504-533 (the launch hop and the record update); services/service-com-amazonaws-workspacesinstances-eucmifrontendapiservice/src/state.rs lines 1740-1799 (ec2_instance_id and WorkspaceInstanceRecord::launched); services/service-com-amazonaws-workspacesinstances-eucmifrontendapiservice/src/state/managed_ec2_instance.rs (the EC2 seam). Not covered by the two open workspaces-instances issues: 20261116T033416Z-workspaces-instances-listinstancetypes-serves-none-of-the-workspace.md is the ListInstanceTypes catalog, and 20261116T070652Z-workspaces-instances-associatevolume-disassociatevolume-never-create-or-remove-the-ebs.md is EBS attachment state on AssociateVolume/DisassociateVolume; neither carries this fixture step's replay key.

## Evidence
Replay of fixture workspaces-instances-managed-instance-allocated-and-running, step task#9, operation GetWorkspaceInstance, service workspaces-instances, verdict mismatch.

Expected status 200, actual status 200 — the status and the error-free shape are correct.
Diverging member: $.EC2ManagedInstance.InstanceId
  expected: the managed-instance id already bound for this WorkSpace Instance in the fixture (carried in the recording as the correlation handle '{{ec2_id}}', not as a literal id)
  actual:   'i-00000000000000001'
Replay detail, verbatim: "$.EC2ManagedInstance.InstanceId: value_mismatch (expected '{{ec2_id}}', got 'i-00000000000000001')"
divergence_key: service=workspaces-instances|GetWorkspaceInstance|status=200->200|keys=$.EC2ManagedInstance.InstanceId
owned_by: [] (no open issue carries this replay key), stub_path: "" (no unimplemented operation blocks it).

Fixed vs variable parts of the requirement: there is no fixed text here — EC2ManagedInstance$InstanceId is a bare String in the model with no pattern or length. The requirement is the EQUALITY of the reported id with the id of the EC2 instance launched for this WorkSpace Instance; the characters 'i-00000000000000001' and the id in any single recording are per-run values and are NOT themselves requirements. What is being reported as the defect is that the two sides do not agree, not that the id has particular digits.

Model evidence used (smithy_query against services/service-com-amazonaws-workspacesinstances-eucmifrontendapiservice/smithy-model/com-amazonaws-workspacesinstances-eucmifrontendapiservice.json):
  - query=operation, operation=GetWorkspaceInstance,CreateWorkspaceInstance
  - query=shape, shape=EC2ManagedInstance -> members: {InstanceId: String}
  - query=doc, shape=EC2ManagedInstance, member=InstanceId -> "Unique identifier of the managed EC2 instance."

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1) and let fixture workspaces-instances-managed-instance-allocated-and-running run in order.
3. Equivalently, by hand against the emulator's data listener, in one Region:
   a. workspaces-instances:CreateWorkspaceInstance with a valid ManagedInstance (ImageId, InstanceType, SubnetId) -> note the returned WorkspaceInstanceId (wsinst-...).
   b. Poll workspaces-instances:GetWorkspaceInstance on that id until ProvisionState is ALLOCATED, and note EC2ManagedInstance.InstanceId.
   c. Establish, independently of that read, which EC2 instance the WorkSpace Instance actually manages — e.g. ec2:DescribeInstances in the same Region and match on the tags/configuration CreateWorkspaceInstance was given.
4. Observe at step 3b/3c that the InstanceId GetWorkspaceInstance reports is not the id of the instance established in 3c; the replay reports this as $.EC2ManagedInstance.InstanceId value_mismatch at task#9, with the emulator serving its counter's first id ('i-00000000000000001') instead of the session's bound managed-instance id.
5. Confirmation after a fix: re-replay workspaces-instances-managed-instance-allocated-and-running#task#9 and require task#9 to pass with EC2ManagedInstance.InstanceId equal to the id bound for the WorkSpace Instance's managed instance in the same fixture.
