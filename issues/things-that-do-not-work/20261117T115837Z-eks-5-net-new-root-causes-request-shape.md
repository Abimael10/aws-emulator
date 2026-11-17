Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eks.create-access-entry-malformed-principal-arn-rejected eks.create-capability-invalid-delete-propagation-policy-rejected eks.create-cluster-public-access-cidr-quota-rejected eks.deploy-eks-identityproviderconfig-cloudformation-oidc-rbac eks.tag-resource-empty-tags-map-rejected
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks|CreateAccessEntry|ResourceNotFoundException
Issue-Key: eks|CreateCapability|ResourceNotFoundException
Issue-Key: eks.create-cluster-public-access-cidr-quota-rejected|_expect_error#7|AssertionError|raise AssertionError("the call SUCCEEDED; real AWS rejects it, so there is no error")
Issue-Key: eks|DescribeIdentityProviderConfig|ResourceNotFoundException
Issue-Key: eks|TagResource|BadRequestException
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# eks: 5 net-new root causes — request-shape validation runs after cluster lookup (CreateAccessEntry, CreateCapability), the publicAccessCidrs quota is not enforced, a CloudFormation IdentityProviderConfig never associates, and TagResource's empty-tags refusal is framework-worded

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAccessEntry

## Description
Five independent, net-new root causes in the EKS control plane. All five are grounded in behavioral checks that fail against the emulator; none is covered by an open issue (the nearest ones are named per cause).

Cause 1 of 5: CreateAccessEntry validates principalArn only AFTER resolving the cluster.
AWS: Smithy — CreateAccessEntry input principalArn (required, String); modeled errors InvalidParameterException (400) and ResourceNotFoundException (404). Recorded behavior: a principalArn that is not ARN-shaped is refused with InvalidParameterException and the message 'The principalArn parameter format is not valid' even when clusterName names a cluster that does not exist, i.e. request-shape validation precedes cluster resolution. Fixed text: the whole sentence 'The principalArn parameter format is not valid' (no variable part). The positive control shows that an ARN-shaped principal on the same non-existent cluster must then give ResourceNotFoundException with 'No cluster found for name: <clusterName>.' (fixed text 'No cluster found for name: ' + the clusterName from the request + '.').
Emulator: the malformed-principal call is answered ResourceNotFoundException / 'No cluster found for name: <clusterName>.' — the cluster lookup runs first and the principalArn syntax check never runs.
Why: a client that distinguishes a client-side ARN typo (400 InvalidParameterException, not retryable, fix the request) from a missing cluster (404, create the cluster / retry) mis-routes the failure; a validation-order test or an SDK error-type branch takes the wrong arm.
Fix: the CreateAccessEntry handler's input validation in the eks service crate (services/service-com-amazonaws-eks-awswesleyfrontend/src/ops/create_access_entry.rs) — perform principalArn format validation before the cluster is looked up in state. Related but distinct open issues: 20261104T105252Z-eks-9-net-new-root-causes-in-parameter.md (other parameter-validation refusals) and 20261113T110805Z-eks-pod-identity-association-mutations-validate-rolearn-before.md (the inverse order on pod-identity mutations); neither covers CreateAccessEntry.principalArn.
Traces: eks.create-access-entry-malformed-principal-arn-rejected.

Cause 2 of 5: CreateCapability validates deletePropagationPolicy only AFTER resolving the cluster.
AWS: Smithy — CreateCapability input deletePropagationPolicy is required and targets enum CapabilityDeletePropagationPolicy whose only value is RETAIN; modeled errors include InvalidParameterException (400). Recorded behavior: deletePropagationPolicy='DELETE_EVERYTHING' is refused with InvalidParameterException and the message 'Invalid delete propagation policy' before the named cluster is looked up; with the legal value RETAIN the same call then fails ResourceNotFoundException 'No cluster found for name: <clusterName>.'. Fixed text: the whole sentence 'Invalid delete propagation policy'.
Emulator: the out-of-enum value is accepted by validation and the call is answered ResourceNotFoundException / 'No cluster found for name: <clusterName>.' instead of the 400 InvalidParameterException.
Why: an illegal enum value is reported as a missing resource, so a client retries or provisions a cluster instead of correcting the request, and never learns the value is unsupported.
Fix: the CreateCapability handler's input validation in the eks service crate (services/service-com-amazonaws-eks-awswesleyfrontend/src/ops/create_capability.rs) — reject values outside CapabilityDeletePropagationPolicy before cluster resolution.
Traces: eks.create-capability-invalid-delete-propagation-policy-rejected.

Cause 3 of 5: CreateCluster does not enforce the publicAccessCidrs per-cluster quota.
AWS: Smithy — CreateCluster.resourcesVpcConfig (VpcConfigRequest).publicAccessCidrs is a StringList of CIDR blocks allowed to reach the public API-server endpoint; modeled error InvalidParameterException (400). Recorded behavior: EKS has a fixed, non-adjustable quota of 40 public-endpoint-access CIDR ranges per cluster, and a create carrying 41 entries is refused with InvalidParameterException and the message 'The publicAccessCidrs list should not be larger than 40.' before any per-entry CIDR check runs; the refused create leaves no cluster, so DescribeCluster answers ResourceNotFoundException 'No cluster found for name: <name>.'. Message template: fixed text 'The publicAccessCidrs list should not be larger than ' + the quota (40) + '.'.
Emulator: the 41-entry create SUCCEEDS (the check reports 'the call SUCCEEDED; real AWS rejects it, so there is no error'), so an over-quota allow-list is persisted on a cluster.
Why: a client that relies on EKS to bound its allow-list builds a cluster configuration real AWS will never accept; the divergence only surfaces at promotion time, and capacity/quota-handling paths are never exercised.
Fix: CreateCluster's resourcesVpcConfig validation in the eks service crate (services/service-com-amazonaws-eks-awswesleyfrontend/src/ops/create_cluster.rs) — enforce the list-length quota, ahead of per-entry CIDR validation, and leave no cluster behind on refusal.
Traces: eks.create-cluster-public-access-cidr-quota-rejected.

Cause 4 of 5: a CloudFormation AWS::EKS::IdentityProviderConfig reaches CREATE_COMPLETE but no OIDC association exists on the cluster.
AWS: Smithy — AssociateIdentityProviderConfig (POST /clusters/{clusterName}/identity-provider-configs/associate) creates the association and DescribeIdentityProviderConfig (POST /clusters/{clusterName}/identity-provider-configs/describe) returns identityProviderConfig.oidc with issuerUrl, clientId, usernameClaim, groupsClaim, status ACTIVE and identityProviderConfigArn; ResourceNotFoundException (404) is reserved for a config that is not associated. Recorded behavior: after the stack carrying the AWS::EKS::IdentityProviderConfig resource reports CREATE_COMPLETE, DescribeIdentityProviderConfig for that {type: oidc, name} answers 200 with the association, and only after the stack is deleted does it answer ResourceNotFoundException.
Emulator: the stack reaches CREATE_COMPLETE, but the immediately following DescribeIdentityProviderConfig is answered 404 ResourceNotFoundException with 'The requested resource does not exist.' — the resource handler reports success without associating anything (or the association is not visible to the describe path).
Why: a client that provisions OIDC trust through CloudFormation gets a green stack and a cluster that trusts nobody; every read-back, in-place tag update (same identityProviderConfigArn) and delete-removes-the-association assertion fails, and the OIDC data-plane path cannot be exercised at all.
Fix: the AWS::EKS::IdentityProviderConfig resource handler must actually perform the association against EKS state (and surface it to DescribeIdentityProviderConfig) before signalling CREATE_COMPLETE; the eks side is DescribeIdentityProviderConfig in services/service-com-amazonaws-eks-awswesleyfrontend/src/ops/ (describe_identity_provider_config.rs / associate_identity_provider_config.rs). Distinct from the filed stub issue 20261104T105252Z-eks-listidentityproviderconfigs-stub.md (ListIdentityProviderConfigs), which this check never calls.
Traces: eks.deploy-eks-identityproviderconfig-cloudformation-oidc-rbac.

Cause 5 of 5: TagResource's empty-tags refusal carries generic framework-constraint wording instead of EKS's sentence.
AWS: Smithy — TagResource.tags is required and targets TagMap with @length min 1, max 50; modeled errors BadRequestException (400) and NotFoundException (404). Recorded behavior: tags={} is refused with BadRequestException and the message 'Invalid request input, expected non-empty tags map', before the resourceArn is resolved; a non-empty map on the same made-up ARN then gives NotFoundException 'Resource was not found'. Fixed text: the whole sentence 'Invalid request input, expected non-empty tags map' (no variable part).
Emulator: same status and code, but the message is the generic serialization-framework constraint text "1 validation error detected: Value '0 entries' at 'tags' failed to satisfy constraint: Member must have length greater than or equal to 1".
Why: clients and operators match on the error message for this class; the emulator's text names an internal constraint report rather than EKS's wording, so message assertions and log-based alerting diverge.
Fix: TagResource's input validation / error rendering in the eks service crate (services/service-com-amazonaws-eks-awswesleyfrontend/src/ops/tag_resource.rs) — emit EKS's own BadRequestException sentence for an empty tags map instead of the framework's constraint-violation text.
Traces: eks.tag-resource-empty-tags-map-rejected.

## Evidence
Cause 1 — eks.create-access-entry-malformed-principal-arn-rejected:
  expected ('InvalidParameterException', 'The principalArn parameter format is not valid')
  got      ('ResourceNotFoundException', 'No cluster found for name: <clusterName>.')

Cause 2 — eks.create-capability-invalid-delete-propagation-policy-rejected:
  expected ('InvalidParameterException', 'Invalid delete propagation policy')
  got      ('ResourceNotFoundException', 'No cluster found for name: <clusterName>.')

Cause 3 — eks.create-cluster-public-access-cidr-quota-rejected:
  expected ('InvalidParameterException', 'The publicAccessCidrs list should not be larger than 40.') for a create with 41 publicAccessCidrs entries
  got      the call SUCCEEDED (AssertionError: 'the call SUCCEEDED; real AWS rejects it, so there is no error')

Cause 4 — eks.deploy-eks-identityproviderconfig-cloudformation-oidc-rbac:
  stack status CREATE_COMPLETE for the AWS::EKS::IdentityProviderConfig resource, then
  botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the DescribeIdentityProviderConfig operation: The requested resource does not exist.
  expected 200 with identityProviderConfig.oidc {issuerUrl, clientId, usernameClaim 'sub', groupsClaim 'cognito:groups', status 'ACTIVE', identityProviderConfigArn present}

Cause 5 — eks.tag-resource-empty-tags-map-rejected:
  expected ('BadRequestException', 'Invalid request input, expected non-empty tags map')
  got      ('BadRequestException', "1 validation error detected: Value '0 entries' at 'tags' failed to satisfy constraint: Member must have length greater than or equal to 1")

## Steps to Reproduce
1. Cause 1: eks.create_access_entry(clusterName=<a name no cluster has>, principalArn='not-an-arn') — expect 400 InvalidParameterException 'The principalArn parameter format is not valid'; then repeat with principalArn='arn:aws:iam::<account>:role/<any>' and expect 404 ResourceNotFoundException 'No cluster found for name: <clusterName>.'.
2. Cause 2: eks.create_capability(clusterName=<a name no cluster has>, capabilityName=<any>, type='KRO', roleArn=<any role arn>, deletePropagationPolicy='DELETE_EVERYTHING') — expect 400 InvalidParameterException 'Invalid delete propagation policy'; repeat with deletePropagationPolicy='RETAIN' and expect the 404 not-found.
3. Cause 3: eks.create_cluster(name=<new>, roleArn=<cluster role>, resourcesVpcConfig={'subnetIds': [two AZ-distinct subnets], 'publicAccessCidrs': [41 distinct /32 CIDRs]}) — expect 400 InvalidParameterException 'The publicAccessCidrs list should not be larger than 40.' and DescribeCluster on that name to answer ResourceNotFoundException.
4. Cause 4: create a cluster, then CreateStack with a template holding one AWS::EKS::IdentityProviderConfig (oidc type, that cluster, an issuer URL and client id); wait for CREATE_COMPLETE, then eks.describe_identity_provider_config(clusterName=<cluster>, identityProviderConfig={'type':'oidc','name':<config name>}) — expect 200 with the ACTIVE oidc config and an identityProviderConfigArn.
5. Cause 5: eks.tag_resource(resourceArn='arn:aws:eks:<region>:<account>:cluster/<name no cluster has>', tags={}) — expect 400 BadRequestException 'Invalid request input, expected non-empty tags map'; then tags={'k':'v'} and expect 404 NotFoundException 'Resource was not found'.
