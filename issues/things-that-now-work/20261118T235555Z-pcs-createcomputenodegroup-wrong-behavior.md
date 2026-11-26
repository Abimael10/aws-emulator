Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 1d88e353ac2946b362e015d0378664a997a5ecbe

# CreateComputeNodeGroup never applies the documented On-Demand default for purchaseOption, so the group reports no purchase option at all

_Filed: 2026-11-18T23:55:54Z_
_Operation: CreateComputeNodeGroup | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `1d88e353ac2946b362e015d0378664a997a5ecbe`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`purchaseOption` is optional, and both the request member and the RESPONSE member document the default the service applies: "If you don't provide this option, it defaults to On-Demand." The handler stores `input.purchase_option` verbatim, so a create that omits the member persists `None`, and every read of the group (`CreateComputeNodeGroup`, `GetComputeNodeGroup`) answers `computeNodeGroup` with NO `purchaseOption` member — instead of the `ONDEMAND` the documentation says the group is running on. A client that branches on `purchaseOption` (e.g. to decide whether `spotOptions` apply) sees an absent member where AWS documents a value, and the stored record carries no purchasing decision at all.

The crate already applies documented member defaults of this model elsewhere, so this is a path that skips a rule the crate otherwise honours: `crate::state::apply_slurm_update` fills `AccountingRequest$mode`/`SlurmRestRequest$mode` with the documented `NONE` when the request names none. `ONDEMAND` is one of the modelled enum members (`PURCHASE_OPTION_VALUES`), so nothing has to be invented.

## Evidence

Smithy (smithy_query query="doc" shape="CreateComputeNodeGroupRequest" member="purchaseOption"), last sentence: "<p>Specifies how EC2 instances are purchased on your behalf. ... If you don't provide this option, it defaults to On-Demand.</p>" — and the output member carries the same sentence (smithy_query query="doc" shape="ComputeNodeGroup" member="purchaseOption": "... If you don't provide this option, it defaults to On-Demand.").

The create stores the member verbatim — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_compute_node_group.rs:470-473:
```
            ami_id: input.ami_id,
            subnet_ids: input.subnet_ids,
            purchase_option: input.purchase_option,
            custom_launch_template: input.custom_launch_template,
```
with `pub purchase_option: Option<String>` on the record (src/state.rs:299-300: "/// A `PurchaseOption` member, when the request named one.") and the read answering it as stored — src/state.rs:528: `purchase_option: self.purchase_option.clone(),`. The input type is `pub purchase_option: Option<String>,` (src/ops/types/create_compute_node_group_types.rs:21), i.e. nothing fills it in before the handler runs.

The crate's own precedent for applying a documented default of this model — src/state.rs:1286-1293:
```
            // "The default value for mode is NONE" (`AccountingRequest$mode`).
            mode: accounting
                .mode
                .or_else(|| previous.map(|previous| previous.mode))
                .unwrap_or_else(|| MODE_NONE.to_string()),
```
(and the same for `SlurmRestRequest$mode`, src/state.rs:1297-1303). The value to use is already named in the crate: src/ops/types/shared_types.rs:339 `pub const PURCHASE_OPTION_VALUES: &[&str] = &["CAPACITY_BLOCK", "INTERRUPTIBLE_CAPACITY_RESERVATION", "ONDEMAND", "SPOT"];`

Test coverage: `grep -rn "purchaseOption\|ONDEMAND" tests/` reports NO hit — no test sends or asserts the member, so nothing pins the current absence (the compute-node-group round trip at tests/all/cluster_lifecycle.rs:309 compares the create's body against the following read, both of which come from the same stored record).

## Steps to Reproduce

1. CreateCluster(clusterName="png-host", scheduler={type:"SLURM",version:"24.11"}, size="SMALL", networking={subnetIds:["subnet-0123456789abcdef0"]}) -> 200.
2. CreateComputeNodeGroup(clusterIdentifier="png-host", computeNodeGroupName="nodes", subnetIds=["subnet-0123456789abcdef0"], customLaunchTemplate={id:"lt-0123456789abcdef0",version:"1"}, iamInstanceProfileArn="arn:aws:iam::<account>:instance-profile/AWSPCS-demo", scalingConfiguration={minInstanceCount:0,maxInstanceCount:4}, instanceConfigs=[{instanceType:"c6i.large"}]) — note NO purchaseOption.
3. Expected per the member documentation ("If you don't provide this option, it defaults to On-Demand"): the response's `computeNodeGroup.purchaseOption` is "ONDEMAND". Observed: the member is absent from the body.
4. GetComputeNodeGroup(clusterIdentifier="png-host", computeNodeGroupIdentifier="nodes") -> 200 with `computeNodeGroup` carrying no `purchaseOption` member either — the stored record holds None.
