Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 4de1224534ccf5deabb70883d8a6d9cce6b35fa7

# A create that sends an empty scalingConfig/throttleConfig object stores and reports the empty shape, where the update path (and the member docs) treat it as no configuration at all

_Filed: 2026-11-23T09:23:28Z_
_Operation: CreateWebFunctionEndpoint | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `4de1224534ccf5deabb70883d8a6d9cce6b35fa7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ScalingConfig$maxEnvironments` and `ThrottleConfig$rateLimit` carry no `@default`; the docs state that when no value is specified the configuration is ABSENT from the response, and `FunctionEndpointSummary$scalingConfig`/`throttleConfig` repeat it ("This field is absent if the endpoint has no scaling configuration"). The UPDATE path implements exactly that: `Endpoint::apply` stores the shape only when its inner member is present (`is_some().then_some(..)`), so a `{}` clears the stored configuration. The CREATE paths do not: `add_endpoint` stores the request's `ScalingConfig`/`ThrottleConfig` verbatim, so a create carrying `"scalingConfig": {}` (or `"throttleConfig": {}`) persists `Some(ScalingConfig { max_environments: None })` and every later reader — the create's own response, `Endpoint::summary` (so `CreateWebFunction`'s initial-endpoint summary), and `GetWebFunctionEndpoint` — reports the member as an empty object instead of omitting it for an endpoint that has no such configuration. The same endpoint, if it later receives an update that sends the same `{}`, ends up with the member absent — so the two write paths of one crate disagree about what an empty configuration object means.

## Evidence

Smithy (`smithy_query(query="shape", shape="ScalingConfig")`, member `maxEnvironments`): "There is no default value. If you don't specify a value, the scaling configuration is absent from the response. On an update, omit <code>scalingConfig</code> to keep the current value, or specify an empty object to clear a previously set value." — and `FunctionEndpointSummary$scalingConfig`: "The scaling configuration for the endpoint. This field is absent if the endpoint has no scaling configuration." (same two sentences for `ThrottleConfig$rateLimit` / `throttleConfig`).

The UPDATE path applies the rule, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/endpoints.rs:314-327:
```
        // "On an update, omit `scalingConfig` to keep the current value, or specify an
        // empty object to clear a previously set value." ... so a configuration that sets none of
        // its members clears the stored one rather than storing an empty shape.
        if let Some(scaling_config) = update.scaling_config {
            self.scaling_config = scaling_config
                .max_environments
                .is_some()
                .then_some(scaling_config);
        }
        if let Some(throttle_config) = update.throttle_config {
            self.throttle_config = throttle_config.rate_limit.is_some().then_some(throttle_config);
        }
```

The CREATE path stores the shape verbatim, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/endpoints.rs:619-620:
```
            scaling_config: new.scaling_config,
            throttle_config: new.throttle_config,
```
fed straight from the request by services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function_endpoint.rs:479-480:
```
                    scaling_config: input.scaling_config,
                    throttle_config: input.throttle_config,
```
and by services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function.rs:452-453:
```
                    scaling_config: config.scaling_config,
                    throttle_config: config.throttle_config,
```

Test coverage: the empty object is pinned on the UPDATE path only — tests/all/web_function_endpoint.rs:1843-1860 ("the scaling configuration cleared by an empty object" -> `("scalingConfig", Value::Null)`); no create row in `endpoint_cases()` (tests/all/web_function_endpoint.rs:165-232) sends `{}`, so nothing pins the create behaviour.

## Steps to Reproduce

1. Create a web function with one revision (PUT /2025-03-07/web-functions)
2. PUT /2025-03-07/web-functions/{functionName}/endpoints with `endpointName`, `endpointType: "HomeRegion"`, `authType: "IamAuth"` and `"scalingConfig": {}`, `"throttleConfig": {}`
3. Observe the 202 response reporting `scalingConfig` and `throttleConfig` as present (empty) members, and GET /2025-03-07/web-functions/{functionName}/endpoints/{endpointName} reporting them too, although the endpoint has no scaling or throttling configuration
4. Send PATCH .../endpoints/{endpointName} with the same `{"scalingConfig": {}}` and observe the member now absent — the two paths disagree
