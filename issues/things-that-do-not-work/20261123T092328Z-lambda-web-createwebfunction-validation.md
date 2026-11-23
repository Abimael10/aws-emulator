Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 4de1224534ccf5deabb70883d8a6d9cce6b35fa7

# A revision naming an egressNetworkConnectorArn is accepted with a MultiRegion/PerRegion endpoint, although the member documents that such revisions support only HomeRegion endpoints

_Filed: 2026-11-23T09:23:28Z_
_Operation: CreateWebFunction | Class: validation_

> _Note: this issue was identified against the code base at commit `4de1224534ccf5deabb70883d8a6d9cce6b35fa7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateWebFunction` takes BOTH sides of the one documented compatibility rule in a single request: `revisionConfig.serviceConfig.egressNetworkConnectorArn` (the revision's network connector) and `endpointConfig.endpointType`. The member's own documentation states the rule: a revision that specifies a network connector supports only `HomeRegion` endpoints. The handler never looks at the two together — its only cross-member screen is `crate::state::endpoint_config_violation`, which is handed the endpoint name, the endpoint type and the regions, and never the revision's `serviceConfig` — so a request that names a network connector AND an endpoint of type `MultiRegion` or `PerRegion` is answered 202 and both resources are stored. The stored `egress_network_connector_arn` is never read anywhere in `src/` (it is a field of the stored `ServiceConfig` and nothing else), so the connector has no effect on which endpoint types the function can carry. The same rule is unenforced on the `CreateWebFunctionEndpoint` path, which can add a `MultiRegion` endpoint to a function whose only revision names a connector.

The crate already enforces the other rules of this family that the model cannot express — the `endpointName` naming rule and the `regions`/`endpointType` rule — in `crate::state::endpoint_config_violation`, whose doc comment enumerates them as "Two rules the model cannot express"; the connector rule is simply absent from that list.

## Evidence

Smithy (`smithy_query(query="shape", shape="ServiceConfig")`, member `egressNetworkConnectorArn`): "The Amazon Resource Name (ARN) of the Lambda network connector that gives the web function access to resources in your VPC. ... Revisions that specify a network connector support only <code>HomeRegion</code> endpoints, not <code>MultiRegion</code> or <code>PerRegion</code>."

Handler — the only endpoint screen the create applies, which never sees the revision configuration, services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function.rs:283-293:
```
            if let Some(message) = crate::state::endpoint_config_violation(
                &config.endpoint_name,
                &config.endpoint_type,
                config.regions.as_deref(),
                &region,
            ) {
```

The rule list that screen implements, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/endpoints.rs:497-507:
```
/// Two rules the model cannot express and the boundary validator therefore cannot
/// enforce:
///
/// * the `endpointName` naming rule its member documentation states, and
/// * the `regions` rule: "Required when the endpoint type is `MultiRegion` or
///   `PerRegion`: ...
```

The member is stored and never read: the only occurrence of `egress` in `src/` is the stored field, services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/types/shared_types.rs:99:
```
    pub egress_network_connector_arn: Option<String>,
```

Test coverage: tests/all/web_function_endpoint.rs:487 (`an_endpoint_configuration_outside_the_documented_rules_is_refused_and_stores_nothing`) and :1479 cover the endpointName/regions refusals only — no test sends `egressNetworkConnectorArn`, so no test pins the acceptance.

## Steps to Reproduce

1. PUT /2025-03-07/web-functions with `functionName`, a `revisionConfig` whose `serviceConfig.egressNetworkConnectorArn` names a fully qualified network connector ARN, and an `endpointConfig` with `endpointType: "MultiRegion"`, `authType: "IamAuth"` and `regions: ["eu-west-1"]`
2. Observe HTTP 202 with both the revision summary and the endpoint summary, instead of a ValidationException refusing the combination the member documents as unsupported
3. GET /2025-03-07/web-functions/{functionName}/endpoints/{endpointName} and observe the stored MultiRegion endpoint serving the connector revision
