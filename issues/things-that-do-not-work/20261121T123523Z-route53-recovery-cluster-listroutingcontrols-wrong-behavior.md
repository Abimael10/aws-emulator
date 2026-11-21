Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: e52cf748fe15a6a8583f9d9789193e5a6472c9ef

# Unfiltered ListRoutingControls reports routing controls of every cluster, not of the cluster addressed

_Filed: 2026-11-21T12:35:23Z_
_Operation: ListRoutingControls | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `e52cf748fe15a6a8583f9d9789193e5a6472c9ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The model scopes an unfiltered `ListRoutingControls` to ONE cluster: "If you specify a control panel ARN, this call lists the routing controls in the control panel. Otherwise, it lists all the routing controls in the cluster." The handler applies no cluster scoping at all — with no `ControlPanelArn` it reports every routing control the control-plane registry holds, across every cluster in the account. So a client that dials a cluster endpoint and lists without a filter is handed routing controls that the cluster it addressed does not host, and `RoutingControls` is not the set the model says that call returns.

The crate itself already treats "which cluster a request is about" as a real rule elsewhere: `UpdateRoutingControlStates` refuses a batch whose entries span two clusters because "A cluster endpoint only serves its own cluster", and `crate::state::RoutingControl` carries `cluster_arn` precisely to back that rule — yet the listing path never consults it. The two operations therefore disagree about whether an endpoint of this API is cluster-scoped.

## Evidence

Model (smithy_query query="doc" shape="ListRoutingControls"): "If you specify a control panel ARN, this call lists the routing controls in the control panel. Otherwise, it lists all the routing controls in the cluster."

Handler, no cluster term anywhere — `services/service-com-amazonaws-route53recoverycluster-togglecustomerapi/src/ops/list_routing_controls.rs:273,287`:
```
        let filter = input.control_panel_arn.as_deref();
...
        let controls = plane.routing_controls_in(filter);
```
and the only filter that helper applies is the control panel — `src/state.rs:216-225`:
```
    pub fn routing_controls_in(&self, control_panel_arn: Option<&str>) -> Vec<RoutingControl> {
        self.registry
            .routing_controls
            .values()
            .filter(|record| {
                control_panel_arn.is_none_or(|panel| record.control_panel_arn == panel)
            })
```
Sibling handler that DOES hold a request to one cluster — `src/ops/update_routing_control_states.rs:318-329`:
```
            // A cluster endpoint only serves its own cluster, so one batch cannot
            // span two of them.
            match &batch_cluster {
                Some(first) if first != cluster => {
                    return Err(validation_exception(
                        format!(
                            "All routing controls in the request must be in cluster {first}"
```
and `src/state.rs:132-134`: "ARN of the cluster the hosting control panel is on. Backs the \"every entry of a batch update is in one cluster\" rule, which is a cluster-endpoint property of this API."

The cross-cluster listing is what the current suite asserts: the fixture is "two clusters, one of them hosting two control panels, and five routing controls" (`tests/all/common.rs:402-404`) and the unfiltered page walk expects all five (`tests/all/routing_control_toggle.rs:240-251`: `let expected = fleet.arns();` … "the pages must walk the whole set in order"), i.e. controls of both clusters in one answer.

## Steps to Reproduce

1. Create two clusters through the recovery-control-config plane, a control panel on each, and one routing control in each panel.
2. Call ListRoutingControls on the data plane with no ControlPanelArn (the endpoint published by cluster one).
3. Observe `RoutingControls` contains the routing control hosted on cluster TWO as well — the model says an unfiltered call "lists all the routing controls in the cluster".
4. Contrast with UpdateRoutingControlStates for the same two controls in one batch, which is refused with "All routing controls in the request must be in cluster <arn>" (src/ops/update_routing_control_states.rs:322-327).
