Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.container-instance-deregister-removes-from-list ecs.delete-attributes-removes-attribute ecs.discover-poll-endpoint-returns-agent-endpoint ecs.list-attributes-filters-by-target-type ecs.put-attributes-target-not-found-rejected ecs.update-container-agent-triggers-update-state
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|RegisterContainerInstance|InvalidParameterException
Issue-Commit: 6b20299dda36b610e0eb6266197358af63fda05b

# ecs: RegisterContainerInstance refuses the instance's own IMDS identity document with the non-AWS sentence "accountId cannot be blank.", so no container instance can be registered

> _Note: this issue was identified against the code base at commit `6b20299dda36b610e0eb6266197358af63fda05b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterContainerInstance

## Description
REOPENS 20261029T175650Z-ecs-7-net-new-root-causes-in-not.md: its fix left the call refused.

Cause 1 of 1: RegisterContainerInstance refuses the (instanceIdentityDocument, instanceIdentityDocumentSignature) pair that the launched instance itself published, with an emulator-authored sentence naming a member of the document's JSON body.

AWS: RegisterContainerInstance is the agent's registration call. Smithy (ecs 2014-11-13, com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113) gives its input exactly these members — cluster, instanceIdentityDocument, instanceIdentityDocumentSignature, totalResources, versionInfo, containerInstanceArn, attributes, platformDevices, tags (@length 0..50) — and its errors as AccessDeniedException (403), ClientException / ClusterNotFoundException / InvalidParameterException (400) and ServerException (500). There is NO accountId member in the request shape, so no AWS refusal for this operation names `accountId`. Presented with the document and signature the instance publishes at http://169.254.169.254/latest/dynamic/instance-identity/document and .../signature, AWS answers HTTP 200 with `containerInstance` (containerInstanceArn, status ACTIVE, agentConnected true) and the cluster thereafter counts it. When AWS cannot accept the pair the refusal is 400 InvalidParameterException with the fixed sentence "The identity document and identity document signature were not valid." — the whole string is fixed text, nothing from the request is interpolated (this is the recorded contract for this operation, as the resolved 20261029T175650Z-ecs-7-net-new-root-causes-in-not.md states for RegisterContainerInstance).

Emulator: the call is refused HTTP 400 InvalidParameterException with the message "accountId cannot be blank." In each of the six checks the pair is obtained exactly the way AWS requires — RunInstances with an SSM-capable instance profile, DescribeInstanceInformation PingStatus=Online, SendCommand AWS-RunShellScript curling the two IMDS paths, GetCommandInvocation Status=Success — and the document the emulator's own metadata service served is then rejected by the emulator's own validator. The sentence is the emulator's: services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/register_container_instance.rs builds it with format!("{member} cannot be blank.") while walking IDENTITY_DOCUMENT_REQUIRED_MEMBERS (documented_instance_id(), ~lines 203-214), and describes_this_caller() (~lines 165-170) additionally demands document.accountId == engine::EMULATOR_ACCOUNT_ID and document.region == the request's region. So the identity document the emulator's IMDS publishes and the members RegisterContainerInstance insists on do not agree. The fixed text of the divergence is the whole message; `accountId` is a member name the emulator chose, not a request value.

Why: a client that follows AWS's only supported registration path cannot register a container instance at all. Everything downstream of registration is therefore unreachable: PutAttributes/ListAttributes/DeleteAttributes against a container-instance target, DeregisterContainerInstance, DiscoverPollEndpoint, UpdateContainerAgent. A client cannot even tell the two AWS failure modes apart, because the message it gets is not one AWS emits for this operation.

Fix: the identity-document acceptance path of the RegisterContainerInstance handler (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/register_container_instance.rs: documented_instance_id / describes_this_caller / IDENTITY_DOCUMENT_REQUIRED_MEMBERS) together with whatever publishes the instance identity document on the emulator's EC2 metadata source — the pair the emulator serves to a guest must be the pair this operation accepts, and any refusal it does emit must carry AWS's fixed sentence, not a member name.

Traces: ecs.container-instance-deregister-removes-from-list, ecs.delete-attributes-removes-attribute, ecs.discover-poll-endpoint-returns-agent-endpoint, ecs.list-attributes-filters-by-target-type, ecs.put-attributes-target-not-found-rejected, ecs.update-container-agent-triggers-update-state (all reported by the suite under `### ecs`).

## Evidence
Expected: HTTP 200 from RegisterContainerInstance with `containerInstance` (status ACTIVE, agentConnected true) for the IMDS-sourced document/signature pair; a document the service cannot accept is refused 400 InvalidParameterException "The identity document and identity document signature were not valid."

Actual (identical in all six checks):
  botocore.errorfactory.InvalidParameterException: An error occurred (InvalidParameterException) when calling the RegisterContainerInstance operation: accountId cannot be blank.

The SSM retrieval itself succeeded in these six checks (the harness helper `_provision_signed_container_instances` raises unless GetCommandInvocation reports Status=Success and both the ---DOC--- and ---SIG--- markers are present), so the document handed to RegisterContainerInstance is the one the emulator's own instance-metadata source published. Instance ids, cluster names and ARNs in the recordings are per-run values; the 400, the error code and the message text are the divergence.

## Steps to Reproduce
1. ecs create-cluster --cluster-name <c>.
2. Create an IAM role trusting ec2.amazonaws.com with AmazonSSMManagedInstanceCore plus an instance profile, and ec2 run-instances with it into a shared-network subnet.
3. Poll ssm describe-instance-information until PingStatus=Online, then ssm send-command --document-name AWS-RunShellScript with a script that curls http://169.254.169.254/latest/dynamic/instance-identity/document and .../signature; poll ssm get-command-invocation until Status=Success and read StandardOutputContent.
4. ecs register-container-instance --cluster <c> --instance-identity-document <doc> --instance-identity-document-signature <sig> --total-resources ... — observe 400 InvalidParameterException "accountId cannot be blank." instead of 200 with containerInstance.
5. Re-replay ecs.container-instance-deregister-removes-from-list, ecs.delete-attributes-removes-attribute, ecs.discover-poll-endpoint-returns-agent-endpoint, ecs.list-attributes-filters-by-target-type, ecs.put-attributes-target-not-found-rejected and ecs.update-container-agent-triggers-update-state to confirm.
