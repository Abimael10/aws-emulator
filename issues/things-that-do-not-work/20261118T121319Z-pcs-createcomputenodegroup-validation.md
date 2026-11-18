Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: ee24c053d2f664e1aece2bc382274e6b27f60b27

# CreateComputeNodeGroup accepts subnetIds from a VPC other than the cluster's

_Filed: 2026-11-18T12:13:19Z_
_Operation: CreateComputeNodeGroup | Class: validation_

> _Note: this issue was identified against the code base at commit `ee24c053d2f664e1aece2bc382274e6b27f60b27`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateComputeNodeGroupRequest$subnetIds` is documented as "The list of subnet IDs where the compute node group launches instances. Subnets must be in the same VPC as the cluster." The handler resolves the request's subnets against EC2 for EXISTENCE only and never compares the VPC those subnets belong to with the VPC of the cluster's own control-plane subnets (which the cluster record persists). A compute node group whose instances could never reach the cluster controller is therefore created and reported ACTIVE instead of being refused with the modelled ValidationException (whose own documentation lists "There are networking related errors, such as network validation failure" as an example).

The gate is already on this exact path — the handler calls `crate::state::ec2_subnets::violation`, which asks EC2 `DescribeSubnets` for the very ids the request named — so the VPC of each reported subnet is in the answer the emulator already receives (`aws_sdk_ec2::types::Subnet`), and the cluster's own `networking.subnetIds` are in state. Only the comparison is missing.

## Evidence

Smithy (smithy_query doc, CreateComputeNodeGroupRequest$subnetIds): "<p>The list of subnet IDs where the compute node group launches instances. Subnets must be in the same VPC as the cluster.</p>"

Handler — the only networking gate on the path, services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_compute_node_group.rs:320-324:
```
        if let Some(refusal) =
            crate::state::ec2_subnets::violation(ctx, &region, &input.subnet_ids).await
        {
            return Err(CreateComputeNodeGroupErrorModeled::ValidationException(refusal).into());
```
That gate decides on ids alone — src/state/ec2_subnets.rs:116-126:
```
            // EC2 reports one subnet per id it resolved, so an id it reported nothing for
            // names no subnet of the account — the same thing the refusal below says.
            let reported: Vec<&str> = output
                .subnets()
                .iter()
                .filter_map(::aws_sdk_ec2::types::Subnet::subnet_id)
                .collect();
            if subnet_ids.iter().all(|id| reported.contains(&id.as_str())) {
                return None;
            }
```
`Subnet::vpc_id` is never read anywhere in the crate, and the handler never looks at the cluster's stored `networking` (src/state.rs:257-258: `/// The control-plane networking the request asked for.` `pub networking: Networking,`) when it validates the group's subnets.

Test coverage: tests/all/subnet_placement.rs:341-410 (`compute_node_group_subnets_are_resolved_the_same_way`) builds the cluster and the group from subnets of the SAME VPC (`let vpc = create_vpc(&ec2, VPC_CIDR).await; let held = create_subnet(&ec2, &vpc, "10.42.5.0/24").await;`, lines 343-344) — no case places a group in a second VPC, so nothing pins the current acceptance.

## Steps to Reproduce

1. Boot the emulator with EC2 mounted (as tests/all/subnet_placement.rs does).
2. EC2: CreateVpc 10.42.0.0/16 -> vpc-A, CreateSubnet(vpc-A, 10.42.1.0/24) -> subnet-A; CreateVpc 10.99.0.0/16 -> vpc-B, CreateSubnet(vpc-B, 10.99.1.0/24) -> subnet-B.
3. PCS CreateCluster(clusterName="group-host", scheduler={type:SLURM,version:"24.11"}, size=SMALL, networking={subnetIds:[subnet-A]}) -> 200.
4. PCS CreateComputeNodeGroup(clusterIdentifier="group-host", computeNodeGroupName="nodes", subnetIds=[subnet-B], customLaunchTemplate={id:"lt-0123456789abcdef0",version:"1"}, iamInstanceProfileArn="arn:aws:iam::<account>:instance-profile/AWSPCS-demo", scalingConfiguration={minInstanceCount:0,maxInstanceCount:4}, instanceConfigs=[{instanceType:"c6i.large"}]).
5. Expected per the member documentation: the request is refused (ValidationException) because subnet-B is not in the cluster's VPC. Observed: HTTP 200, the group is persisted, and GetComputeNodeGroup reports it ACTIVE with subnetIds=[subnet-B].
