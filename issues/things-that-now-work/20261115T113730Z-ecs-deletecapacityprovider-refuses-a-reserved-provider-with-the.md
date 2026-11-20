Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-capacity-provider-management
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-capacity-provider-management ecs-cluster-capacity-provider-lifecycle
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|DeleteCapacityProvider|4xx->4xx|$.__type
Issue-Key: ecs|DeleteCapacityProvider|4xx->4xx|$.message
Issue-Commit: 3fd6090c6180e74798ad464a124672539521deef

# ECS DeleteCapacityProvider refuses a reserved provider with the wrong error code and Smithy doc prose

> _Note: this issue was identified against the code base at commit `3fd6090c6180e74798ad464a124672539521deef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteCapacityProvider

## Description
Cause 1 of 1 — DeleteCapacityProvider on a reserved (FARGATE / FARGATE_SPOT) capacity provider is refused with the wrong modeled error and a message copied out of the API documentation.
AWS: Smithy: DeleteCapacityProvider models errors AccessDeniedException(403), ClientException(400), ClusterNotFoundException(400), InvalidParameterException(400), ServerException(500), UnsupportedFeatureException(400), UpdateInProgressException(400). Its documentation states that 'The FARGATE and FARGATE_SPOT capacity providers are reserved and can't be deleted', and the recorded response shows AWS refusing that attempt with HTTP 400, __type 'ClientException' and the message 'The specified capacity provider is reserved and cannot be modified by this operation. Specify a valid name or ARN and try again.' That message is entirely fixed text for this error class — it names no request value, so nothing in it is substituted from the request or from state.
Emulator: answers HTTP 400 (status matches) but with __type 'InvalidParameterException' and the message "The FARGATE and FARGATE_SPOT capacity providers are reserved and can't be deleted. You can disassociate them from a cluster using either PutClusterCapacityProviders or by deleting the cluster." — i.e. the Smithy documentation note for the operation, verbatim, rather than the wire message, under a different error code.
Why: AWS SDKs and clients dispatch on the error code, and ClientException and InvalidParameterException are distinct modeled shapes with distinct retry/handling semantics; code that catches ClientException to report 'this provider is reserved' never fires, while code that catches InvalidParameterException treats it as a malformed-argument bug. Any client asserting on or logging the refusal text also sees documentation prose instead of AWS's wording.
Fix: the DeleteCapacityProvider handler's validation in the ECS service — raise ClientException with AWS's fixed reserved-provider text for FARGATE/FARGATE_SPOT, and stop sourcing refusal messages from the model's documentation trait.
Traces: ecs-capacity-provider-management#task#7 and ecs-cluster-capacity-provider-lifecycle#task#12 (same cluster).

Related open issues that do NOT carry this replay key: 20261104T062603Z-ecs-deletecapacityprovider-stub.md was filed when the operation was an unimplemented stub (it now answers, wrongly); 20261115T045707Z-ecs-6-net-new-causes-container-instance-agent.md records the same 'refusal message is the Smithy doc prose' pattern on other ECS operations.

## Evidence
DeleteCapacityProvider on a reserved provider — expected 400 / actual 400:
  $.__type: expected 'ClientException', got 'InvalidParameterException'
  $.message: expected 'The specified capacity provider is reserved and cannot be modified by this operation. Specify a valid name or ARN and try again.', got "The FARGATE and FARGATE_SPOT capacity providers are reserved and can't be deleted. You can disassociate them from a cluster using either PutClusterCapacityProviders or by deleting the cluster."
The actual message is the operation's Smithy documentation note reproduced verbatim. Cluster members: ecs-capacity-provider-management#task#7, ecs-cluster-capacity-provider-lifecycle#task#12.

## Steps to Reproduce
1. Replay fixture ecs-capacity-provider-management up to step task#7: call DeleteCapacityProvider with capacityProvider=FARGATE (or FARGATE_SPOT).
2. Observe HTTP 400 with __type 'InvalidParameterException' and the documentation-note message, instead of __type 'ClientException' with 'The specified capacity provider is reserved and cannot be modified by this operation. Specify a valid name or ARN and try again.'
3. The same divergence reproduces at ecs-cluster-capacity-provider-lifecycle step task#12.
