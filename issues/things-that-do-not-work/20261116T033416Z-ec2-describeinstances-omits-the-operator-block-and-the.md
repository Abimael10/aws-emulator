Issue-Source: trace-sweep-divergence
Issue-Trace: workspaces-instances-managed-instance-allocated-and-running
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: workspaces-instances-managed-instance-allocated-and-running
Issue-Shape-Id: com.amazonaws.workspacesinstances#EUCMIFrontendAPIService
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.operator.hiddenByDefault
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.operator.managed
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.operator.principal
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.tagSet.item[key="Name"]
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.tagSet.item[key="aws:ec2:managed-launch"]
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.tagSet.item[key="aws:workspaces-instances:workspace-instance-id"]
Issue-Commit: 40b4c40578424e9a73c61204c7ab51c695d87313

# ec2 DescribeInstances omits the Operator block and the managed-launch tags on a service-managed instance

> _Note: this issue was identified against the code base at commit `40b4c40578424e9a73c61204c7ab51c695d87313`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstances

## Description
One cause.

Cause 1 of 1: for an instance launched on behalf of a service provider (here, a running WorkSpace Instance), DescribeInstances answers HTTP 200 but reports the instance as unmanaged and drops the service-provider tags the launch applied.

AWS: DescribeInstances projects Instance.Operator, an OperatorResponse (Smithy: com.amazonaws.ec2#OperatorResponse {Managed -> xmlName "managed", boolean, "If true, the resource is managed by a service provider"; Principal -> xmlName "principal", string, "If managed is true, then the principal is returned. The principal is the service provider that manages the resource"; HiddenByDefault -> xmlName "hiddenByDefault", boolean, "If true, the resource is hidden by default based on the managed resource visibility settings for the account"}; Instance.Operator doc: "The service provider that manages the instance"). For a provider-managed launch AWS reports managed=true, principal = the managing service principal (for WorkSpaces Instances, workspaces-instances.amazonaws.com) and hiddenByDefault=true. The instance's tagSet additionally carries the reserved tags the managed launch applies — recorded behavior only for their exact keys: aws:ec2:managed-launch (value naming the managed launch configuration) and aws:workspaces-instances:workspace-instance-id (value = the WorkSpace Instance id that owns the EC2 instance). The requirement is the pair of reserved keys and that each value is taken from the launch/state (the managed-launch configuration name and the owning WorkSpace Instance id); the specific id in the recording (wsinst-0000000000000001) is one run's value, not part of the requirement.

Emulator: status matches (200), but the projection diverges on the same instance: operator.managed is 'false' where 'true' is expected; operator.principal is absent entirely (expected the managing service principal, workspaces-instances.amazonaws.com); operator.hiddenByDefault is 'false' where 'true' is expected; tagSet carries neither aws:ec2:managed-launch nor aws:workspaces-instances:workspace-instance-id, and instead carries an unexpected "Name" tag the recording does not have.

Why: a client that uses Operator to tell provider-managed capacity from its own sees every instance as self-managed, so management-visibility filtering and any "who owns this instance" decision is wrong, and nothing links the EC2 instance back to its WorkSpace Instance. Tooling that correlates by the reserved tag keys (inventory, cost attribution, cleanup that must not touch managed capacity) finds no match and may act on instances it should leave alone.

Fix: the DescribeInstances instance projection (its serializer for Instance.Operator) plus the launch path that records managed ownership in instance state: a provider-managed launch must store the managing principal and visibility flags and apply the reserved aws:ec2:managed-launch / aws:workspaces-instances:workspace-instance-id tags, instead of defaulting Operator to managed=false with no principal and tagging the instance with a Name tag.

No open issue carries this replay key (the divergence reports owned_by empty). Nearby open issues that do not cover it: "EC2 DescribeInstances and launch-template projections omit modeled members and misreport terminal state" (20261103T071133Z-ec2-describeinstances-and-launch-template-projections-omit-modeled.md) and "EC2 DescribeInstances refuses the WorkSpaces-Instances-reported instance id as InvalidInstanceID.Malformed" (20261107T124004Z-ec2-describeinstances-refuses-the-workspaces-instances-reported-instance.md).

Traces: workspaces-instances-managed-instance-allocated-and-running#task#7.

## Evidence
Fixture workspaces-instances-managed-instance-allocated-and-running, step task#7, operation DescribeInstances (service ec2), verdict mismatch.
Expected status 200, actual status 200.
Diff (from replay), all under $.DescribeInstancesResponse.reservationSet.item.instancesSet.item:
- operator.managed: value_mismatch (expected 'true', got 'false')
- operator.principal: missing (expected 'workspaces-instances.amazonaws.com', got None)
- operator.hiddenByDefault: value_mismatch (expected 'true', got 'false')
- tagSet.item[key="aws:ec2:managed-launch"]: missing (expected {'key': 'aws:ec2:managed-launch', 'value': 'workspaces_instances-managed-ws-hourly'})
- tagSet.item[key="aws:workspaces-instances:workspace-instance-id"]: missing (expected {'key': 'aws:workspaces-instances:workspace-instance-id', 'value': 'wsinst-0000000000000001'})
- tagSet.item[key="Name"]: extra (not in the recording)
Smithy: com.amazonaws.ec2#OperatorResponse members Managed (xmlName managed), Principal (xmlName principal), HiddenByDefault (xmlName hiddenByDefault); Instance.Operator doc "The service provider that manages the instance".

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Replay fixture workspaces-instances-managed-instance-allocated-and-running through step task#7: allocate a WorkSpace Instance, let it reach running, then call ec2 DescribeInstances for the backing EC2 instance.
3. Observe the instancesSet item reports operator.managed=false with no operator.principal and hiddenByDefault=false, and its tagSet contains neither aws:ec2:managed-launch nor aws:workspaces-instances:workspace-instance-id (but does contain a Name tag).
