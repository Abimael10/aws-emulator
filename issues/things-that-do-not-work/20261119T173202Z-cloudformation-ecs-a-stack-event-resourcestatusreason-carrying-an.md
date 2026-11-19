Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecs.deploy-ecs-cluster-capacity-provider-associations-unassociated-strategy-rejected
Issue-Key: cloudformation__ecs.deploy-ecs-cluster-capacity-provider-associations-unassociated-strategy-rejected|run#18|AssertionError|assert got == _REJECTED, f"handler reason {got!r}, expected {_REJECTED!r}"
Issue-Commit: 23a6305fb4194ac652f562ce813f73dafa581013

# cloudformation__ecs: a stack-event ResourceStatusReason carrying an ECS rejection drops AWS's SDK error envelope ("(Service: AmazonECS; Status Code: 400; Error Code: InvalidParameterException; Request ID: ...; Proxy: null)")

> _Note: this issue was identified against the code base at commit `23a6305fb4194ac652f562ce813f73dafa581013`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: when a CloudFormation resource fails because the underlying ECS call was refused, the stack event's ResourceStatusReason is only the bare ECS message; AWS appends the service-error envelope that names the service, HTTP status, error code and request id.

AWS: a CloudFormation resource whose provisioning call is refused by the downstream service reports a CREATE_FAILED event whose ResourceStatusReason is the downstream message followed by AWS's SDK envelope, in this fixed form: `<ECS message> (Service: AmazonECS; Status Code: 400; Error Code: InvalidParameterException; Request ID: <request id>; Proxy: null)`. For an AWS::ECS::ClusterCapacityProviderAssociations whose DefaultCapacityProviderStrategy names a provider not associated with the cluster, the ECS message itself is the fixed text "The specified capacity provider strategy cannot contain a capacity provider that is not associated with the cluster. Associate the capacity provider with the cluster or specify a valid capacity provider and try again." Fixed text in the envelope: "(Service: AmazonECS; Status Code: ", "; Error Code: ", "; Request ID: ", "; Proxy: null)". Variable parts: the status code and error code of the refusal that actually occurred (400 / InvalidParameterException here) and the per-run request id. Smithy: ECS PutClusterCapacityProviders / CreateService model InvalidParameterException (client, httpError 400).

Emulator: the stack event's reason is the ECS sentence alone, with no envelope: `('The specified capacity provider strategy cannot contain a capacity provider that is not associated with the cluster. Associate the capacity provider with the cluster or specify a valid capacity provider and try again.', 'InvalidRequest')`. The rollback status itself is right ('InvalidRequest'); only the reason text diverges.

Why: a client that parses a stack event's reason to recover the downstream error code — the normal way to distinguish "fix the template" (InvalidParameterException) from "retry" (throttling/5xx) and to correlate with a request id in support/audit flows — finds no code, no status and no request id, so every downstream refusal looks the same and is unattributable to a service call.

Fix: the CloudFormation emulator's resource-provisioning error path (where a downstream service refusal is turned into a stack event's ResourceStatusReason): wrap the downstream error in AWS's envelope, naming the service, the HTTP status, the error code and the request id of the refused call.

Traces: cloudformation__ecs.deploy-ecs-cluster-capacity-provider-associations-unassociated-strategy-rejected.

## Evidence
Expected (AWS): the CREATE_FAILED/rollback event's reason is "The specified capacity provider strategy cannot contain a capacity provider that is not associated with the cluster. Associate the capacity provider with the cluster or specify a valid capacity provider and try again. (Service: AmazonECS; Status Code: 400; Error Code: InvalidParameterException; Request ID: <id>; Proxy: null)".
Actual (emulator): "The specified capacity provider strategy cannot contain a capacity provider that is not associated with the cluster. Associate the capacity provider with the cluster or specify a valid capacity provider and try again." — the envelope suffix is absent. Replay: `AssertionError: handler reason ('The specified capacity provider strategy ... try again.', 'InvalidRequest'), expected ('The specified capacity provider strategy ... try again. (Service: AmazonECS; Status Code: 400; Error Code: InvalidParameterException; Request ID: <id>; Proxy: null)', 'InvalidRequest')` (check line 215). The request id inside the envelope is a per-run value; the envelope's labels and the service/status/code it reports are the requirement.

## Steps to Reproduce
1. CreateStack with an AWS::ECS::Cluster plus an AWS::ECS::ClusterCapacityProviderAssociations whose DefaultCapacityProviderStrategy names a capacity provider that is NOT in its CapacityProviders list / not associated with the cluster.
2. Wait for the stack to fail and roll back.
3. DescribeStackEvents and read the failing resource's ResourceStatusReason.
4. Observe the bare ECS sentence. Expected: the same sentence followed by " (Service: AmazonECS; Status Code: 400; Error Code: InvalidParameterException; Request ID: <id>; Proxy: null)".
