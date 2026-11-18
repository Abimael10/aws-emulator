Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 1d88e353ac2946b362e015d0378664a997a5ecbe

# CreateCluster drops three documented member defaults (networkType IPV4, scaleDownIdleTimeInSeconds 600, accounting.defaultPurgeTimeInDays -1) so a read reports members the service documents as set

_Filed: 2026-11-18T23:55:54Z_
_Operation: CreateCluster | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `1d88e353ac2946b362e015d0378664a997a5ecbe`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

A create stores the networking and Slurm request structures verbatim, so every optional member the request omitted stays absent in the stored record and in every later `GetCluster`/`CreateCluster` body — even where the model's own documentation states the value the service applies:

* `networkType` — "The IP address version the cluster uses. The default is IPV4." (on BOTH `NetworkingRequest$networkType` and the response shape `Networking$networkType`). The crate already BEHAVES as if the default were IPV4 — `ClusterRecord::endpoint` publishes an IPv4 control-plane address for any value that is not `IPV6` — but the member it reports back stays absent, so the record contradicts the address the same record publishes.
* `slurmConfiguration.scaleDownIdleTimeInSeconds` — "The time (in seconds) before an idle node is scaled down. Default: 600" (on the request and the response shape).
* `slurmConfiguration.accounting.defaultPurgeTimeInDays` — "The default value for defaultPurgeTimeInDays is -1." (request and response), left absent by the create AND by `apply_slurm_update` on the update path.

The crate already applies a documented default of this very model one line away (`AccountingRequest$mode` -> `NONE`, `SlurmRestRequest$mode` -> `NONE`), so these three are the paths that skip a rule the crate otherwise honours, not a behaviour it never attempts.

## Evidence

Smithy, the three sentences (smithy_query query="shape" shape="NetworkingRequest,Networking,ClusterSlurmConfigurationRequest,ClusterSlurmConfiguration,AccountingRequest,Accounting"):
* `networkType`: "<p>The IP address version the cluster uses. The default is <code>IPV4</code>.</p>"
* `scaleDownIdleTimeInSeconds`: "<p>The time (in seconds) before an idle node is scaled down.</p> <p>Default: <code>600</code> </p>"
* `defaultPurgeTimeInDays`: "<p>The default value for <code>defaultPurgeTimeInDays</code> is <code>-1</code>.</p> <p>A value of <code>-1</code> means there is no purge time and records persist as long as the cluster exists.</p>"

The create stores the request structures verbatim — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_cluster.rs:345-348:
```
            scheduler: input.scheduler.into(),
            size: input.size,
            networking: input.networking.into(),
            slurm_configuration: input.slurm_configuration.map(Into::into),
```
and the conversions copy member for member with no default applied — src/state.rs:1473-1481 (`From<NetworkingRequest> for Networking`: `network_type: request.network_type`), src/state.rs:1498-1513 (`From<ClusterSlurmConfigurationRequest>`: `scale_down_idle_time_in_seconds: request.scale_down_idle_time_in_seconds`), src/state.rs:1483-1490 (`From<AccountingRequest> for Accounting`: `default_purge_time_in_days: request.default_purge_time_in_days`). The reads answer exactly those fields: src/state.rs:366-367 (`slurm_configuration: self.slurm_configuration.clone(), networking: self.networking.clone()`).

The crate ACTS on the IPV4 default while not recording it — src/state.rs:495-505:
```
    fn endpoint(&self, endpoint_type: &str, port: &str) -> Endpoint {
        let ipv6 = self.networking.network_type.as_deref() == Some(NETWORK_TYPE_IPV6);
        ...
            private_ip_address: if ipv6 {
                String::new()
            } else {
                control_plane_ipv4(self.seq)
```
so a cluster created with no `networkType` publishes an IPv4 endpoint while reporting no `networking.networkType`.

The precedent the create path skips — src/state.rs:1286-1293 and 1297-1303:
```
            // "The default value for mode is NONE" (`AccountingRequest$mode`).
            mode: accounting
                .mode
                .or_else(|| previous.map(|previous| previous.mode))
                .unwrap_or_else(|| MODE_NONE.to_string()),
```
and, in the same block, `default_purge_time_in_days: accounting.default_purge_time_in_days.or_else(|| previous.as_ref().and_then(|a| a.default_purge_time_in_days))` (src/state.rs:1284-1288) — the purge member gets no default on the update path either.

Test coverage: no test pins the ABSENCE of any of the three. tests/all/cluster_lifecycle.rs:107-118 and tests/all/cluster_update_and_endpoints.rs:52-62 always send `networkType` explicitly; tests/all/cluster_update_and_endpoints.rs:399/413/471 always send `scaleDownIdleTimeInSeconds` and `defaultPurgeTimeInDays` explicitly; tests/all/resource_tags.rs:102-108 and tests/all/delete_ordering.rs:76-84 omit them but assert nothing about `networking` or `slurmConfiguration`.

## Steps to Reproduce

1. CreateCluster(clusterName="defaults-cluster", scheduler={type:"SLURM",version:"24.11"}, size="SMALL", networking={subnetIds:["subnet-0123456789abcdef0"]}, slurmConfiguration={accounting:{mode:"STANDARD"}}) -> 200.
2. Expected per the member documentation: `cluster.networking.networkType` = "IPV4", `cluster.slurmConfiguration.scaleDownIdleTimeInSeconds` = 600, `cluster.slurmConfiguration.accounting.defaultPurgeTimeInDays` = -1.
3. Observed: all three members are absent from the response body.
4. GetCluster(clusterIdentifier="defaults-cluster") -> 200: the same three members are still absent, while `cluster.endpoints[0].privateIpAddress` is an IPv4 address (10.0.x.y) — the record publishes an IPv4 control plane while reporting no `networkType`.
