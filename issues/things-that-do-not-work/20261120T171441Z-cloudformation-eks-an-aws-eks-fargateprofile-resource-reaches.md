Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__eks.deploy-cfn-fargateprofile-active-and-deleted
Issue-Key: cloudformation__eks.deploy-cfn-fargateprofile-active-and-deleted|run#20|AssertionError|assert profiles == [profile_name], f"ListFargateProfiles {profiles!r}"
Issue-Commit: 3fc1bb2776bd84c02126163be463ba1779e82c7d

# cloudformation__eks: an AWS::EKS::FargateProfile resource reaches CREATE_COMPLETE but no Fargate profile exists in EKS — ListFargateProfiles on the stack's cluster is empty

> _Note: this issue was identified against the code base at commit `3fc1bb2776bd84c02126163be463ba1779e82c7d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: a CloudFormation stack containing an AWS::EKS::FargateProfile reports the resource created, but the profile is not present in the EKS service, so the cluster's own ListFargateProfiles answers an empty list.
  AWS: Smithy (eks): CreateFargateProfile (POST /clusters/{clusterName}/fargate-profiles) creates the profile and ListFargateProfiles (GET /clusters/{clusterName}/fargate-profiles) returns its fargateProfileNames; DescribeFargateProfile reads it back. AWS::EKS::FargateProfile maps ClusterName / FargateProfileName / PodExecutionRoleArn / Subnets / Selectors / Tags onto that API, its Ref is "<clusterName>|<fargateProfileName>" and its Arn attribute is the profile's fargateProfileArn. Recorded behavior: once the stack is CREATE_COMPLETE the profile really exists in EKS — ListFargateProfiles on the stack's cluster returns exactly the one profile name, DescribeFargateProfile reports status ACTIVE with the template's podExecutionRoleArn, the template's private subnets, selectors [{namespace: <Namespace>, labels: {}}] (AWS returns the unset selector labels as an empty map) and the template's tags, and a stack UPDATE that drops the resource deletes the profile while the cluster stays ACTIVE.
  Emulator: the stack reaches CREATE_COMPLETE and its outputs (the profile's Ref and Arn) resolve, and the EKS cluster the same stack created describes correctly (status ACTIVE, arn, endpoint, roleArn, subnetIds, endpointPublicAccess=false, endpointPrivateAccess=true, tags all match), but a fully paged ListFargateProfiles for that cluster returns [] — "ListFargateProfiles []". So CloudFormation reports a resource that the EKS service does not hold. Hypothesis: the AWS::EKS::FargateProfile resource handler synthesizes the physical id/ARN for the resource without performing the EKS CreateFargateProfile (the cluster resource in the same template clearly does reach EKS, since DescribeCluster answers for it). ListFargateProfiles itself is not at fault: it lists a profile created through the EKS API in eks.fargate-profile-full-lifecycle.
  Why: every CloudFormation/CDK consumer of EKS Fargate — stacks whose pods must land on Fargate, and the controllers that read the profile back — gets a green deployment for infrastructure that does not exist. Nothing the stack says about the profile can be verified through the EKS API, DescribeFargateProfile cannot resolve the name the stack's Ref advertises, and the subsequent lifecycle (an update that drops the profile, the stack delete) operates on a resource that was never created, so the divergence is silent until pod scheduling.
  Fix: the AWS::EKS::FargateProfile resource handler in the CloudFormation emulator's resource registry — provision the resource by calling the eks CreateFargateProfile path with the template's ClusterName/FargateProfileName/PodExecutionRoleArn/Subnets/Selectors/Tags, wait for ACTIVE before completing the resource, return Ref "<clusterName>|<fargateProfileName>" and the real fargateProfileArn for the Arn attribute, and delete the profile through DeleteFargateProfile when the resource is removed. Related but distinct: 20261117T115837Z-cloudformation-eks-the-aws-eks-cluster-resource-handler.md covers the AWS::EKS::Cluster handler refusing ComputeConfig; it does not cover AWS::EKS::FargateProfile.
  Traces: cloudformation__eks.deploy-cfn-fargateprofile-active-and-deleted.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout. Overall 269/364 passed; cloudformation__eks 0/4, and this is the only one of its four failures not marked OWNED/ALREADY FILED.

cloudformation__eks.deploy-cfn-fargateprofile-active-and-deleted [mismatch]:
  File "check_cloudformation__eks_deploy_cfn_fargateprofile_active_and_deleted.py", line 245, in run
  assert profiles == [profile_name], f"ListFargateProfiles {profiles!r}"
  AssertionError: ListFargateProfiles []

The assertions before it all pass: CreateStack with the template below reaches CREATE_COMPLETE within the 1200 s budget, a fully paged ListClusters returns the cluster and matches the stack's Ref output, and DescribeCluster matches (status ACTIVE, arn, endpoint, roleArn, sorted subnetIds, endpointPublicAccess false, endpointPrivateAccess true, tags {sim2real: <stack>}). The failing read is paginate(eks, 'list_fargate_profiles', 'fargateProfileNames', clusterName=<cluster>) — every page — which must be exactly [<FargateProfileName>].

Template resource under test (AWS::EKS::FargateProfile):
  ClusterName: {"Ref": "EksCluster"}, FargateProfileName: <profile name>,
  PodExecutionRoleArn: {"Fn::GetAtt": ["FargateRole", "Arn"]}, Subnets: <the run's private subnets>,
  Selectors: [{"Namespace": <namespace>}], Tags: [{"Key": "sim2real-fp", "Value": <profile name>}]
  Outputs: ProfileRefOut = {"Ref": "FargateProfile"}, ProfileArnOut = {"Fn::GetAtt": ["FargateProfile", "Arn"]}

Not yet reached, and therefore also unverified: ProfileRefOut == "<clusterName>|<profileName>"; DescribeFargateProfile == (ACTIVE, ProfileArnOut, clusterName, FargateRoleArn, the private subnets, [{namespace: <ns>, labels: {}}], {sim2real: <stack>, sim2real-fp: <profile>}); the profile-removing UpdateStack reaching UPDATE_COMPLETE with DescribeFargateProfile then ResourceNotFoundException while the cluster stays ACTIVE; and the stack delete removing the cluster.

Smithy model consulted: services/service-com-amazonaws-eks-awswesleyfrontend/smithy-model/com-amazonaws-eks-awswesleyfrontend.json — CreateFargateProfile POST /clusters/{clusterName}/fargate-profiles (required fargateProfileName, clusterName, podExecutionRoleArn; optional subnets, selectors, tags) -> FargateProfile; ListFargateProfiles -> fargateProfileNames; DescribeFargateProfile -> FargateProfile.

## Steps to Reproduce
1. git_clone the project repository and build/boot the emulator from this checkout.
2. Replay the behavioral suite (simparity-behavioral-aws-v1) and re-run cloudformation__eks.deploy-cfn-fargateprofile-active-and-deleted.
3. Manually: CreateStack (Capabilities=['CAPABILITY_IAM']) with a template containing an EKS cluster role, a Fargate pod execution role trusting eks-fargate-pods.amazonaws.com, an AWS::EKS::Cluster (private endpoint only, two AZ-distinct subnets) and an AWS::EKS::FargateProfile whose ClusterName is {"Ref": "EksCluster"}, Subnets are private subnets and Selectors are [{"Namespace": "<ns>"}], plus outputs for the profile's Ref and Arn.
4. Poll DescribeStacks until CREATE_COMPLETE (it is reached).
5. Call eks:ListFargateProfiles(clusterName=<the cluster the stack created>) across every page. Expect exactly [<FargateProfileName>]; the emulator returns an empty list. Then call eks:DescribeFargateProfile for that name and expect an ACTIVE profile echoing the template's role, subnets, selectors (labels {}) and tags.
6. UpdateStack with the same template minus the FargateProfile resource and expect UPDATE_COMPLETE, DescribeFargateProfile then ResourceNotFoundException "The requested resource does not exist.", and DescribeCluster still ACTIVE.
