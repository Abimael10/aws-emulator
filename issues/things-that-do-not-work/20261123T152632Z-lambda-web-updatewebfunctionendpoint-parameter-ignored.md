Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: c3e9e6b2ad75a47d5c834c28447cb874a69b2715

# An update's validated revisionWeights are discarded inside the same call when the endpoint is in LatestRevision mode

_Filed: 2026-11-23T15:26:32Z_
_Operation: UpdateWebFunctionEndpoint | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `c3e9e6b2ad75a47d5c834c28447cb874a69b2715`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateWebFunctionEndpoint` validates the request's `revisionWeights` (404 for a revision the function does not have, 400 for a revision named twice), then hands them to `ComAmazonawsLambdawebLambdawebState::update_endpoint`, which calls `Endpoint::apply` — storing the requested weights — and IMMEDIATELY calls `WebFunction::deploy_latest_revision()` on the same record. For an endpoint whose `autoDeploymentMode` is (or stays) `LatestRevision`, that second call overwrites the weights just stored with `[{newest Active revision, 100}]` unless the request happened to name exactly that one revision. So a PATCH that asks a LatestRevision endpoint for a canary split — e.g. `revisionWeights: [{R1,70},{R2,30}]` — is accepted, answers 202, and reports `[{R2,100}]`: the parameter the handler went to the trouble of validating has no effect at all, and nothing in the response says so.

This contradicts the rule `Endpoint::apply`'s own doc states one line above the assignment: "every member the request named takes its new value and every member it omitted keeps the stored one". The same crate's Disabled-mode path does honour the parameter (that is what the update tests pin), so the two mode branches of one handler disagree about whether a validated `revisionWeights` is applied. `update_endpoint`'s own doc justifies the extra call for "an update that turns the mode on", but the code runs it for every update, including one that neither names nor changes `autoDeploymentMode`.

## Evidence

The rule the apply path states, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/endpoints.rs:292-295:
```
    /// Apply `update` to this endpoint: every member the request named takes its new
    /// value and every member it omitted keeps the stored one ("Omit this field to keep
    /// the current …", stated by each updatable member of
    /// `UpdateWebFunctionEndpointRequest`).
```
and the assignment under it, endpoints.rs:311-313:
```
        if let Some(revision_weights) = update.revision_weights {
            self.revision_weights = revision_weights;
        }
```

The line that discards it, endpoints.rs:656-657 (`update_endpoint`):
```
        endpoint.apply(update, now);
        function.deploy_latest_revision();
```
whose overwrite is services/service-com-amazonaws-lambdaweb-lambdaweb/src/state.rs:420-432:
```
        for endpoint in self.endpoints.values_mut() {
            if endpoint.auto_deployment_mode != AUTO_DEPLOYMENT_MODE_LATEST_REVISION {
                continue;
            }
            let serving_only_newest = endpoint.revision_weights.len() == 1
                && endpoint.revision_weights[0].revision_id == revision_id;
            if serving_only_newest {
                continue;
            }
            endpoint.revision_weights = vec![crate::ops::types::RevisionWeight {
                revision_id: revision_id.clone(),
                weight: FULL_WEIGHT,
            }];
```

The parameter is validated first, services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/update_web_function_endpoint.rs:347:
```
            refuse_requested_weights(function, input.revision_weights.as_deref())?;
```

Test coverage: every weight-setting update row starts from a baseline endpoint whose mode is `Disabled` — tests/all/web_function_endpoint.rs:1766 `"autoDeploymentMode": "Disabled",` — and the rows that set weights (tests/all/web_function_endpoint.rs:1861-1893, "the traffic moved onto another revision", "the traffic split across two revisions") therefore never exercise a LatestRevision endpoint, so nothing pins this combination.

## Steps to Reproduce

1. PUT /2025-03-07/web-functions to create a function with a revisionConfig (revision R1), wait for R1 to report Active
2. POST /2025-03-07/web-functions/{functionName}/revisions to create a second revision R2, wait for R2 to report Active
3. PUT /2025-03-07/web-functions/{functionName}/endpoints with endpointName, endpointType HomeRegion, authType IamAuth and "autoDeploymentMode": "LatestRevision"
4. PATCH /2025-03-07/web-functions/{functionName}/endpoints/{endpointName} with {"revisionWeights":[{"revisionId":"<R1>","weight":70},{"revisionId":"<R2>","weight":30}]}
5. Observe the 202 response reporting revisionWeights [{R2,100}] instead of the requested split, and GET .../endpoints/{endpointName} reporting the same — the accepted and validated parameter had no effect
6. Repeat with the baseline endpoint created with "autoDeploymentMode": "Disabled" and observe the same request honoured
