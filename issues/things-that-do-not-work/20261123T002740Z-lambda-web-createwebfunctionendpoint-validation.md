Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 039d38dc4c02de0c6d595d33e91f7f516f07aa43

# throttleConfig.rateLimit is accepted at any non-negative value, not only the supported values the member documents

_Filed: 2026-11-23T00:27:40Z_
_Operation: CreateWebFunctionEndpoint | Class: validation_

> _Note: this issue was identified against the code base at commit `039d38dc4c02de0c6d595d33e91f7f516f07aa43`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ThrottleConfig$rateLimit` documents a closed set of accepted values (0, 100, 200, …, 1000, 2000, …, 10000). The model carries only `@range(min: 0)` for the member, so the request boundary cannot enforce the set; the handler stores whatever arrived (`throttle_config: input.throttle_config`) and the endpoint then reports an unsupported rate (e.g. 137 or 10001) from every endpoint reader.

This crate's own pattern is to apply the rules the model cannot express: `endpoint_config_violation` enforces the documented `endpointName` naming rule and the documented `regions` rule, and `function_name_violation` enforces the documented 64-character function-name rule — all three cited to member documentation in exactly the way this one is. The `rateLimit` set is the one such documented rule with no check anywhere in the crate (grep for `rate_limit` finds only the store/clear plumbing in state/endpoints.rs:326 and the account constant), and no comment or test states a reason for the divergence. The same omission applies to the other write path, `UpdateWebFunctionEndpoint` (src/ops/update_web_function_endpoint.rs:366 passes `throttle_config: input.throttle_config` straight into `EndpointUpdate`).

## Evidence

Smithy (smithy_query shape ThrottleConfig, member rateLimit): "<p>The maximum request rate per second for the endpoint. The value must be one of the following supported values: <code>0</code>, <code>100</code>, <code>200</code>, <code>300</code>, <code>400</code>, <code>500</code>, <code>600</code>, <code>700</code>, <code>800</code>, <code>900</code>, <code>1000</code>, <code>2000</code>, <code>3000</code>, <code>4000</code>, <code>5000</code>, <code>6000</code>, <code>7000</code>, <code>8000</code>, <code>9000</code>, or <code>10000</code>. ..." — modelled constraint is only `"range": {"min": 0}`.

Handler stores it unchecked — services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function_endpoint.rs:472-481:
```
                crate::state::NewEndpoint {
                    endpoint_name: input.endpoint_name.clone(),
                    ...
                    scaling_config: input.scaling_config,
                    throttle_config: input.throttle_config,
```

The crate's own convention for doc-only rules — services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/endpoints.rs:494-517:
```
/// Why `config` is not an endpoint this service can create in `home_region` ...
/// Two rules the model cannot express and the boundary validator therefore cannot
/// enforce:
/// * the `endpointName` naming rule its member documentation states, and
/// * the `regions` rule: ...
pub fn endpoint_config_violation(
```
…and src/ops/create_web_function_endpoint.rs:414-424 calls it, but nothing judges `throttleConfig`.

No justifying comment or test: tests/all/web_function_endpoint.rs:162-164 only notes "`throttleConfig.rateLimit` is `@range(0..)` … so the rows that carry them sit on those published bounds" and every row uses a supported value (0, 100, 10000 — lines 188, 203, 932, 948, 1770).

## Steps to Reproduce

1. PUT /2025-03-07/web-functions/my-fn (CreateWebFunction) with a revisionConfig, so a revision exists
2. PUT /2025-03-07/web-functions/my-fn/endpoints with {"endpointName":"prod","endpointType":"HomeRegion","authType":"IamAuth","throttleConfig":{"rateLimit":137}}
3. Observe 202 Accepted instead of a 400 ValidationException, although 137 is not one of the supported values the member documents
4. GET /2025-03-07/web-functions/my-fn/endpoints/prod — the stored endpoint reports "throttleConfig": {"rateLimit": 137}
5. PATCH the endpoint with {"throttleConfig":{"rateLimit":10001}} — also 202, same gap on the update path
