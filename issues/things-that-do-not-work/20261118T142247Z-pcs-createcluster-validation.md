Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 11339946e28962ad6886a228614bad20ab02f1d0

# CreateCluster never resolves networking.securityGroupIds against EC2, so a cluster is created on security groups that do not exist

_Filed: 2026-11-18T14:22:47Z_
_Operation: CreateCluster | Class: validation_

> _Note: this issue was identified against the code base at commit `11339946e28962ad6886a228614bad20ab02f1d0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

CreateCluster's own documentation states that BOTH the subnets and the security groups of the cluster must already exist before the action is used. The handler implements only the subnet half: it resolves `networking.subnetIds` against EC2 (`crate::state::ec2_subnets::violation`) and refuses a create whose subnets EC2 reports nothing for, but it never resolves `networking.securityGroupIds` at all. A request naming a security group id the account holds no group under is served 200, the fabricated id is persisted on the record, and GetCluster/ListClusters report the cluster ACTIVE with a control-plane ENI that could never have been attached to those groups — instead of the modelled ValidationException (whose own documentation lists "There are networking related errors, such as network validation failure" as an example).

The comment above the one hop quotes the full documented sentence — subnets AND security groups — and then resolves only the subnets. Everything the other half needs is already on this exact path: the EC2 hop is built and made here, `securityGroupIds` is already bounded by the same `stored_list_violation` pass a few lines above (so a hop would not be sized by the request), and the neighbouring EC2 emulator holds security groups and implements DescribeSecurityGroups. Only the question is missing.

## Evidence

Smithy (smithy_query query="doc" shape="CreateCluster"): "<p>Creates a cluster in your account. PCS creates the cluster controller in a service-owned account. The cluster controller communicates with the cluster resources in your account. The subnets and security groups for the cluster must already exist before you use this API action.</p>"

The handler quotes that sentence and implements only its subnet half — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_cluster.rs:275-295:
```
        // "The subnets and security groups for the cluster must already exist before you
        // use this API action" (this operation's own documentation): the subnets the
        // control-plane ENI would be created in belong to EC2, so a create resolves them
        // against EC2 before it mints or stores anything — a cluster whose ENI could never
        // be placed is refused rather than persisted.
        ...
        if let Some(refusal) = crate::state::ec2_subnets::violation(
            ctx,
            &region,
            input.networking.subnet_ids.as_deref().unwrap_or_default(),
        )
        .await
```
Nothing else in the body reads `security_group_ids` except the length bound (create_cluster.rs:226-232: `"networking.securityGroupIds", input.networking.security_group_ids.as_ref().map_or(0, Vec::len)`) and the store (create_cluster.rs:347: `networking: input.networking.into(),`, which copies the member through — src/state.rs:1473-1481: `fn from(request: NetworkingRequest) -> Self { Self { subnet_ids: request.subnet_ids, security_group_ids: request.security_group_ids, ... } }`).

The resolution module quotes the same sentence and asks EC2 about subnets only — src/state/ec2_subnets.rs:5-11:
```
//! A subnet is not a resource of this service: it is Amazon EC2's. "The subnets and
//! security groups for the cluster must already exist before you use this API action"
//! (`CreateCluster`'s own documentation) ...
```
and src/state/ec2_subnets.rs:109-113 is the whole hop: `client.describe_subnets().set_subnet_ids(Some(subnet_ids.to_vec())).send().await`. `grep -rn security_group` in this crate's `src/` matches only the length bound, the record field and the `From` impl above — never a hop or a check.

The owner of the resource already answers the question: services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_security_groups.rs:327: `for group in store.security_groups.values() {` (an implemented handler over persisted EC2 security groups).

Test coverage: nothing pins the current acceptance. The EC2-mounted suite never sends the member — tests/all/subnet_placement.rs:121-129 (`fn create_cluster(name, version, subnets)` builds `"networking": {"subnetIds": subnets}` with no `securityGroupIds`), on the harness at tests/all/subnet_placement.rs:47 (`fn emulator_with_ec2()`). The suites that DO send a fabricated id (tests/all/cluster_lifecycle.rs:114: `"securityGroupIds": ["sg-0123456789abcdef0"],`) run on tests/all/cluster_lifecycle.rs:42-43 — "/// An emulator with only PCS mounted, on its own engine." — where, by the module's own rule, no refusal is grounded anyway (src/state/ec2_subnets.rs:22-26: "A hop that could not be made at all — an emulator with no EC2 mounted ... so it refuses nothing").

## Steps to Reproduce

1. Boot the emulator with EC2 mounted beside PCS on one engine (as tests/all/subnet_placement.rs:47 `emulator_with_ec2()` does).
2. EC2: CreateVpc 10.42.0.0/16 -> vpc-A; CreateSubnet(vpc-A, 10.42.1.0/24) -> subnet-A. Create NO security group.
3. PCS CreateCluster(clusterName="sg-check", scheduler={type:"SLURM",version:"24.11"}, size="SMALL", networking={subnetIds:[subnet-A], securityGroupIds:["sg-zzzzz000000000000"]}).
4. Expected per the operation documentation ("The subnets and security groups for the cluster must already exist before you use this API action"): the create is refused with the modelled ValidationException and nothing is persisted, the same way an unresolvable subnet id is refused today.
5. Observed: HTTP 200; GetCluster(clusterIdentifier="sg-check") reports the cluster ACTIVE with networking.securityGroupIds = ["sg-zzzzz000000000000"], an id EC2 resolves no security group for (EC2 DescribeSecurityGroups with that id reports none).
