Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__eks.deploy-cfn-cluster-auto-mode-compute-network-storage-config
Issue-Key: cloudformation__eks.deploy-cfn-cluster-auto-mode-compute-network-storage-config|run#11|AssertionError|raise AssertionError(f"control stack ended {status!r}: {reason!r}")
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# cloudformation__eks: the AWS::EKS::Cluster resource handler refuses any template carrying ComputeConfig ('this emulator does not apply the ComputeConfig property'), so every EKS Auto Mode stack rolls back

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: the CloudFormation AWS::EKS::Cluster resource handler rejects the ComputeConfig property outright, failing the resource and rolling the stack back.
AWS: Smithy (eks) — CreateCluster accepts computeConfig (ComputeConfigRequest {enabled, nodePools, nodeRoleArn}), kubernetesNetworkConfig (incl. elasticLoadBalancing) and storageConfig (blockStorage); these are the EKS Auto Mode capabilities and AWS::EKS::Cluster maps ComputeConfig / KubernetesNetworkConfig.ElasticLoadBalancing / StorageConfig.BlockStorage onto them. Recorded behavior: a template whose AWS::EKS::Cluster sets ComputeConfig {Enabled: true, NodePools: []}, KubernetesNetworkConfig {ElasticLoadBalancing {Enabled: true}, IpFamily ipv4, ServiceIpv4Cidr <cidr>} and StorageConfig {BlockStorage {Enabled: true}} reaches CREATE_COMPLETE and DescribeCluster echoes those values back; only the illegal combination (ComputeConfig.NodeRoleArn set with an empty NodePools) is refused, and then with EKS's own message relayed through the handler — 'When Compute Config nodeRoleArn is not null or empty, nodePool value(s) must be provided. (Service: Eks, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)' with HandlerErrorCode InvalidRequest (fixed text is the sentence; Request ID / Attempt Count are per-run values).
Emulator: the positive-control stack — the legal Auto Mode template with no NodeRoleArn — ends ROLLBACK_COMPLETE with the resource-handler reason 'Resource handler returned message: "this emulator does not apply the ComputeConfig property of AWS::EKS::Cluster" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)'. The handler announces its own unimplemented property instead of applying it, so no Auto Mode cluster can ever be created through CloudFormation and the intended negative case (NodeRoleArn without NodePools) can never be reached either.
Why: every CDK/CloudFormation stack that enables EKS Auto Mode fails to deploy against the emulator even though the configuration is valid for AWS; the failure is reported as the client's InvalidRequest, so a client sees a template rejection rather than a missing emulator capability, and the compute/network/storage config round-trip is never exercised.
Fix: the AWS::EKS::Cluster resource handler in the CloudFormation emulator's resource registry — map ComputeConfig (and the paired KubernetesNetworkConfig.ElasticLoadBalancing / StorageConfig.BlockStorage) onto the eks CreateCluster request instead of failing the resource, and relay EKS's own refusal for the NodeRoleArn-without-NodePools combination. Related but distinct open issue: 20261110T203834Z-eks-6-net-new-root-causes-cluster-endpoint.md mentions two other CFN EKS resource types never reaching CREATE_COMPLETE; it does not cover AWS::EKS::Cluster ComputeConfig.
Traces: cloudformation__eks.deploy-cfn-cluster-auto-mode-compute-network-storage-config.

## Evidence
cloudformation__eks.deploy-cfn-cluster-auto-mode-compute-network-storage-config:
  AssertionError: control stack ended 'ROLLBACK_COMPLETE': 'Resource handler returned message: "this emulator does not apply the ComputeConfig property of AWS::EKS::Cluster" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)'

Expected: the control stack (AWS::EKS::Cluster with ComputeConfig {Enabled: true, NodePools: []}, KubernetesNetworkConfig {ElasticLoadBalancing {Enabled: true}, IpFamily 'ipv4', ServiceIpv4Cidr}, StorageConfig {BlockStorage {Enabled: true}}, private endpoint only) reaches CREATE_COMPLETE and DescribeCluster echoes the values it set. Only the variant adding ComputeConfig.NodeRoleArn with NodePools empty must fail, with message 'When Compute Config nodeRoleArn is not null or empty, nodePool value(s) must be provided. (Service: Eks, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)' and HandlerErrorCode 'InvalidRequest'.

## Steps to Reproduce
1. CreateStack with a template containing an AWS::IAM::Role for EKS and an AWS::EKS::Cluster whose Properties include ResourcesVpcConfig {SubnetIds: two AZ-distinct subnets, EndpointPublicAccess: false, EndpointPrivateAccess: true}, AccessConfig {AuthenticationMode: 'API'}, ComputeConfig {Enabled: true, NodePools: []}, KubernetesNetworkConfig {ElasticLoadBalancing {Enabled: true}, IpFamily: 'ipv4', ServiceIpv4Cidr: '10.95.0.0/16'}, StorageConfig {BlockStorage {Enabled: true}}.
2. Poll DescribeStacks until a terminal create status — it reports ROLLBACK_COMPLETE, and DescribeStackEvents shows the EksCluster CREATE_FAILED reason quoted above; expected CREATE_COMPLETE with DescribeCluster echoing computeConfig / kubernetesNetworkConfig / storageConfig.
3. Then deploy the same template with ComputeConfig.NodeRoleArn set and NodePools empty and observe the relayed EKS refusal instead of an emulator-authored one.
