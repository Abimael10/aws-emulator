Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 9d94d8e1cd3a917b1659a893a3ba20b52a695ddd

# An update may weight a revision whose build FAILED, although the same handler refuses to deploy that revision through autoDeploymentMode — the endpoint then serves nothing

_Filed: 2026-11-25T20:52:47Z_
_Operation: UpdateWebFunctionEndpoint | Class: validation_

> _Note: this issue was identified against the code base at commit `9d94d8e1cd3a917b1659a893a3ba20b52a695ddd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateWebFunctionEndpoint` holds a requested `revisionWeights` list to exactly two rules (`crate::state::revision_weight_problem`): the revision must be one of the function's own, and no revision may be named twice. Neither rule looks at the revision's STATE, so a PATCH that moves an Active endpoint's whole traffic onto a revision whose build FAILED is accepted and answers 202.

Three lines further down the SAME handler refuses the equivalent change when it is asked for through `autoDeploymentMode`: `switches_to_latest_revision` + `crate::state::latest_revision_switch_conflict` answer 409 ConflictException, and the sentence they answer with states the rule the weights path breaks — "Only revisions in Active state can be deployed." So one handler refuses deploying a Failed revision when the request names it indirectly (the mode) and accepts it when the request names it directly (the weights).

The consequence is observable and silent: the crate's own data plane serves only a revision it finds `Active` (`endpoint_plane::addressed` filters `revision.state == RevisionState::Active`), so after the accepted PATCH the published `domainName` answers `503` with "No web function code is serving this endpoint." for every request, while `GetWebFunctionEndpoint` keeps reporting the endpoint `Active`. A `Failed` revision is terminal — `Revision::materialise_due` only ever moves a revision out of `Pending`, never out of `Failed` — so the endpoint never recovers on its own.

Note the scope: a still-`Pending` revision becomes `Active` on the clock, and the crate's create path deliberately accepts weights naming one (tests/all/web_function_endpoint.rs:1209-1264 creates endpoints weighted onto revisions that have not settled yet), so this is about the terminal `Failed` state the update path lets a live endpoint be pointed at.

## Evidence

The rule the same handler states and enforces for the mode switch, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/endpoints.rs:428-439:
```
pub fn latest_revision_switch_conflict(function: &WebFunction) -> Option<String> {
    let newest = function.latest_revision()?;
    if newest.state == RevisionState::Active {
        return None;
    }
    let revision_id = &newest.revision_id;
    let state = newest.state.wire();
    Some(format!(
        "The latest revision {revision_id} is in {state} state and cannot be deployed. Only \
         revisions in Active state can be deployed. Create a new revision."
    ))
}
```
and its own doc, endpoints.rs:412-417: "only a revision the service reports `Active` is \"ready to serve traffic\" (`RevisionState`) — so putting an endpoint into that mode while the newest revision is still building, or while its build failed, would route the endpoint's traffic onto a revision that cannot take it: a conflict with the current state of the resource rather than a change the service can make."

The rule the weights path applies instead — ownership and repetition only, no state — endpoints.rs:388-407:
```
pub fn revision_weight_problem(
    function: &WebFunction,
    requested: &[RevisionWeight],
) -> Option<RevisionWeightProblem> {
    let mut named: Vec<&str> = Vec::with_capacity(requested.len());
    for weight in requested {
        if !function.revisions.contains_key(&weight.revision_id) {
            return Some(RevisionWeightProblem::UnknownRevision { ... });
        }
        if named.contains(&weight.revision_id.as_str()) {
            return Some(RevisionWeightProblem::RepeatedRevision { ... });
        }
```

Both checks sit in one block of the handler, services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/update_web_function_endpoint.rs:344-355:
```
        {
            let function = require_function(&store, &function_name, &input.function_name)?;
            let endpoint = require_endpoint(function, &endpoint_name, &input.endpoint_name)?;
            refuse_requested_weights(function, input.revision_weights.as_deref())?;
            // Switching the endpoint onto the newest revision is only a change the
            // service can make while that revision is ready to serve traffic.
            if switches_to_latest_revision(endpoint, input.auto_deployment_mode.as_deref()) {
                if let Some(message) = crate::state::latest_revision_switch_conflict(function) {
                    return Err(conflict(message));
                }
            }
        }
```

What the accepted weights do to the published address, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/endpoint_plane.rs:682-685:
```
        .and_then(|revision_id| function.revisions.get(revision_id))
        // "Active: the revision is ready to serve traffic" (`RevisionState`), so a revision
        // that is still building or whose build failed takes none.
        .filter(|revision| revision.state == RevisionState::Active)
```
and endpoint_plane.rs:492-505, which answers that with `SERVICE_UNAVAILABLE` and endpoint_plane.rs:130 `const NO_CODE_TO_SERVE: &str = "No web function code is serving this endpoint.\n";`

Smithy, for what the state means (`smithy_query(query="shape", shape="RevisionState")`): "`Active` – The revision is ready to serve traffic. ... `Failed` – The revision build failed."

Test coverage: the mode-switch refusal onto a `Failed` revision is pinned in tests/all/revision_build_outcome.rs:871-975 (`the_latest_revision_switch_conflict_words_the_refusal_as_the_service_does`, asserting 409 ConflictException and the "Only revisions in Active state can be deployed." sentence). The update's weight-refusal table, tests/all/web_function_endpoint.rs:2324-2374, has rows only for "a weight naming a revision the function does not have", "two weights naming ONE revision" and the weight/list bounds — no row sends a weight naming a revision whose build failed, so nothing pins the acceptance.

## Steps to Reproduce

1. Create a web function with a revisionConfig whose codeConfig.s3Object names an object that is not a zip archive, plus an endpointConfig (HomeRegion / IamAuth, autoDeploymentMode Disabled) — call the resulting revision R_bad; then create a second revision R_good from a readable archive, or create the function from a good archive first and add R_bad with POST /2025-03-07/web-functions/{functionName}/revisions
2. Poll GET /2025-03-07/web-functions/{functionName}/revisions/{R_bad} until it reports state "Failed"
3. PATCH /2025-03-07/web-functions/{functionName}/endpoints/{endpointName} with {"autoDeploymentMode":"LatestRevision"} while R_bad is the newest revision and observe 409 ConflictException "... Only revisions in Active state can be deployed. ..."
4. PATCH the same endpoint with {"revisionWeights":[{"revisionId":"<R_bad>","weight":100}]} and observe 202 with the Failed revision reported as the endpoint's only weight
5. GET /2025-03-07/web-functions/{functionName}/endpoints/{endpointName} and observe state "Active" with revisionWeights naming R_bad
6. Issue any HTTP request to the endpoint's published domainName and observe 503 with the body "No web function code is serving this endpoint." — the endpoint is permanently out of service, with no error having been reported for the change that took it there
