Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: network-security-manager.get-deployment-for-an-unknown-identifier-is-not-found network-security-manager.update-rule-rejects-all-prior-update-tokens-after-new-version
Issue-Shape-Id: com.amazonaws.networksecuritymanager#PiccoloCustomerAPIService
Issue-Key: network-security-manager.get-deployment-for-an-unknown-identifier-is-not-found|run#6|AssertionError|assert message == "The requested resource was not found.", f"unexpected message {message!r}"
Issue-Key: network-security-manager.update-rule-rejects-all-prior-update-tokens-after-new-version|run#16|AssertionError|assert message == _stale_message, f"unexpected message {message!r}"
Issue-Commit: e0fb8928b55d063818ff45281a7e96a54cacfd74

# network-security-manager: 2 net-new refusal-message causes — GetDeployment's 404 states an emulator-authored sentence quoting the whole ARN, and UpdateRule's stale-token 409 states a generic token sentence

> _Note: this issue was identified against the code base at commit `e0fb8928b55d063818ff45281a7e96a54cacfd74`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDeployment

## Description
Two net-new causes, found replaying the behavioral suite (simparity-behavioral-aws-v1) against the emulator built from this checkout. Both are refusal-MESSAGE divergences in this service: the HTTP status and the error code match in each case, only the text differs. They are reported under `### network-security-manager` and neither failing check is marked ALREADY FILED or OWNED.

=== Cause 1 of 2: GetDeployment's ResourceNotFoundException message is an emulator-authored sentence that quotes the caller's whole ARN, where AWS states the generic resource sentence ===
AWS: Smithy com.amazonaws.networksecuritymanager#GetDeployment — GET /deployments/{deploymentIdentifier}, success @http code 200; deploymentIdentifier is @httpLabel @required (DeploymentIdentifier, @length 1..1010), documented "The identifier of the deployment. This is the deployment's Amazon Resource Name (ARN)". Modeled errors: AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400; ResourceNotFoundException is @error client / @httpError 404, @required message plus optional resourceId/resourceType, documented "The specified resource was not found. Verify that the resource identifier is correct and that the resource exists, then try your request again." The model does not fix the message string, so the wording is recorded behavior only: for a well-formed deployment ARN that names no deployment, AWS answers ResourceNotFoundException with Error.Message "The requested resource was not found." — that whole string is FIXED text for this error class on this operation; it interpolates no id, name, arn or timestamp, so there is no variable part at all. (AWS does NOT word every entity alike here: the recorded template read words its miss "Template not found: <templateId>" — filed separately as 20261124T100554Z-network-security-manager-gettemplate-not-found-refusal-states.md — so the per-entity wording has to be followed per operation rather than unified.)
Emulator: answers HTTP 404 with ResourceNotFoundException (status and code match) but states message "Deployment not found: arn:aws:network-security-manager:<region>:<account>:deployment:<deploymentId>" — a locally-invented sentence which additionally echoes the ENTIRE identifier ARN the caller sent instead of any id. Replay detail verbatim: AssertionError: unexpected message 'Deployment not found: arn:aws:network-security-manager:us-west-2:000000000000:deployment:simbehavnsmmissingdeploymentf031e1db1549'. Note the related issue 20261117T225331Z-network-security-manager-read-filter-surface-diverges-2.md (status fixed) made this read answer ResourceNotFoundException at all instead of 501, and that half now matches; the remaining message-text half is what is filed here, so this is not a re-refusal of a call that should succeed — AWS refuses it too.
Why: the error message is part of the contract. A client, test, operator runbook, UI string or log-based alert that matches or surfaces the not-found text for a deployment read sees a sentence AWS never sends, so assertions and alerting on AWS's wording silently fail to fire; and because the emulator pastes the caller's full ARN into the message, anything that logs the refusal now records the complete request identifier where AWS records a constant, changing what ends up in logs. The status and code being right keeps this invisible until something reads the text.
Fix: the GetDeployment miss branch's construction of ResourceNotFoundException — it must state AWS's fixed generic sentence for this operation rather than taking the entity-specific "<Type> not found: <id>" wording, and it must not interpolate the request's identifier into the message (the optional resourceId/resourceType members already carry the identifier and are not at issue). Code sites: services/service-com-amazonaws-networksecuritymanager-piccolocustomerapiservice/src/ops/get_deployment.rs lines 196-207 (the `store.deployment_by_identifier(...)` miss branch calls `crate::state::resource_not_found(RESOURCE_TYPE_DEPLOYMENT, &input.deployment_identifier)`); services/.../src/state.rs lines 83-90 (`resource_not_found` formats "{resource_type} not found: {resource_id}") and lines 100-106 (`entity_id_in` only strips a '/'-delimited tail, so a deployment ARN whose resource field is colon-delimited passes through whole).
Traces: network-security-manager.get-deployment-for-an-unknown-identifier-is-not-found.

=== Cause 2 of 2: UpdateRule's stale-updateToken ConflictException states a generic token sentence instead of AWS's concurrent-modification wording, and it is stated for every superseded token ===
AWS: Smithy com.amazonaws.networksecuritymanager#UpdateRule — PATCH /rules/{ruleIdentifier}, success @http code 200; updateToken is @required (UpdateToken, @length 36..36, @pattern ^([0-9a-f]{8})-([0-9a-f]{4}-){3}([0-9a-f]{12})$). When the supplied token is not the resource's current token the operation fails with the modeled ConflictException (@error client, @httpError 409, message @required, documented "The request conflicts with the current state of the resource. For example, the resource was modified concurrently, or it is in a state that does not allow the requested operation."). The model does not fix the message string, so the wording is recorded behavior only: AWS answers HTTP 409 with message "Invalid update token - entity was modified by another process". That whole string is FIXED text for this error class — it interpolates no rule id, token, version or timestamp, so no part of it is a per-run value. This check additionally pins that EVERY superseded token is rejected this way, not only the immediately-preceding one: after two successful updates (version 2 then version 3) the version-1 token must still be refused 409 with that same text.
Emulator: detects the stale token and answers HTTP 409 ConflictException (status and code match) but serializes message "The provided updateToken is not the current updateToken for the resource" — a locally-authored sentence about the token rather than AWS's concurrent-modification wording. Replay detail verbatim: AssertionError: unexpected message 'The provided updateToken is not the current updateToken for the resource'.
Why: callers and diagnostic layers that branch on, log, surface or test against the ConflictException message for an optimistic-concurrency collision get text that never appears on real AWS; anything matching the message (test assertions, runbooks, alerting on concurrent modification, UI strings telling a user another process changed the rule) fails to fire or reports a different cause than AWS reports.
Fix: the UpdateRule handler's optimistic-concurrency branch and the shared constant it reads — the site that compares the request's updateToken against the stored rule's current token must emit AWS's fixed sentence for the stale-token case. Code sites: services/.../src/state.rs lines 108-117 (`UPDATE_TOKEN_CONFLICT_MESSAGE = "The provided updateToken is not the current updateToken for the resource"`, the shared constant), consumed by the UpdateRule stale-token branch in services/.../src/ops/update_rule.rs. Related open issue 20261126T154623Z-network-security-manager-updaterule-stale-updatetoken-conflict-states.md describes this same root cause from the WIRE corpus (replay key network-security-manager|UpdateRule|4xx->4xx|$.message) and does NOT carry this behavioral check's key, so the behavioral check id below is recorded here; a fixer closing both should expect one code change to satisfy both replays.
Traces: network-security-manager.update-rule-rejects-all-prior-update-tokens-after-new-version.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 against the emulator built from this checkout: 7/11 checks passed; 4 mismatched, 0 errored, 0 skipped. network-security-manager 7/9 (78%), 2 failed.

Cause 1 — check network-security-manager.get-deployment-for-an-unknown-identifier-is-not-found, verdict mismatch:
  File "check_network_security_manager_get_deployment_for_an_unknown_identifier_is_not_found.py", line 37, in run
    assert message == "The requested resource was not found.", f"unexpected message {message!r}"
  AssertionError: unexpected message 'Deployment not found: arn:aws:network-security-manager:us-west-2:000000000000:deployment:simbehavnsmmissingdeploymentf031e1db1549'
The check first asserts code == "ResourceNotFoundException" (that assertion PASSED), then the message.
  Expected (AWS, recorded): HTTP 404, ResourceNotFoundException, Error.Message = "The requested resource was not found." (entirely fixed text; no variable part).
  Actual (emulator): HTTP 404, ResourceNotFoundException, Error.Message = "Deployment not found: <the full identifier ARN sent by the caller>". The region/account/id shown are this run's values, not requirements.
  Model: GetDeployment http {GET /deployments/{deploymentIdentifier}, code 200}; deploymentIdentifier @httpLabel @required @length(1,1010); errors AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400; ResourceNotFoundException { message @required, resourceId, resourceType }.
  Code: src/ops/get_deployment.rs:196-207 -> crate::state::resource_not_found(RESOURCE_TYPE_DEPLOYMENT, &input.deployment_identifier); src/state.rs:83-90 message = format!("{resource_type} not found: {resource_id}"); src/state.rs:100-106 entity_id_in() leaves a colon-delimited ARN resource field untouched, which is why the whole ARN appears.
  The same check's ListDeployments pagination read succeeded and never surfaced the never-created id, so the call path itself is sound.

Cause 2 — check network-security-manager.update-rule-rejects-all-prior-update-tokens-after-new-version, verdict mismatch:
  File "check_network_security_manager_update_rule_rejects_all_prior_update_tokens_after_new_version.py", line 94, in run
    assert message == _stale_message, f"unexpected message {message!r}"
  AssertionError: unexpected message 'The provided updateToken is not the current updateToken for the resource'
  with _stale_message = "Invalid update token - entity was modified by another process".
The preceding assertions PASSED: CreateRule, UpdateRule to version '2', UpdateRule to version '3', and code == "ConflictException" for the replayed version-1 token.
  Expected (AWS, recorded): HTTP 409, ConflictException, Error.Message = "Invalid update token - entity was modified by another process" (entirely fixed text).
  Actual (emulator): HTTP 409, ConflictException, Error.Message = "The provided updateToken is not the current updateToken for the resource".
  Model: UpdateRule http {PATCH /rules/{ruleIdentifier}, code 200}; updateToken @required @length(36,36) @pattern ^([0-9a-f]{8})-([0-9a-f]{4}-){3}([0-9a-f]{12})$; errors include ConflictException (client, httpError 409) whose message member is @required.
  Code: src/state.rs:108-117 UPDATE_TOKEN_CONFLICT_MESSAGE, consumed by the UpdateRule stale-token branch (src/ops/update_rule.rs).

## Steps to Reproduce
Cause 1 (GetDeployment message):
1. Clone the repo and boot the emulator built from it.
2. Call GetDeployment with a well-formed but never-created deployment ARN: GET /deployments/arn:aws:network-security-manager:<region>:<account>:deployment:<alnum id> (botocore: nsm.get_deployment(deploymentIdentifier=arn)).
3. Expect HTTP 404 with x-amzn-errortype ResourceNotFoundException and message exactly "The requested resource was not found."; observe HTTP 404 ResourceNotFoundException whose message is "Deployment not found: <the ARN you sent>".
4. Replay check network-security-manager.get-deployment-for-an-unknown-identifier-is-not-found.

Cause 2 (UpdateRule stale-token message):
1. CreateRule(ruleName=<unique>, firewallType="WAF", ruleType="CONFIGURATION", configuration={"DefaultAction":{"Allow":{}}}, isPublished=True) and keep updateToken token_v1.
2. UpdateRule(ruleIdentifier=<ruleArn>, updateToken=token_v1, isPublished=True, configuration={"DefaultAction":{"Block":{}}}) -> version "2", token_v2; then UpdateRule with token_v2 -> version "3", token_v3.
3. UpdateRule again with the two-versions-stale token_v1.
4. Expect HTTP 409 ConflictException with message exactly "Invalid update token - entity was modified by another process"; observe HTTP 409 ConflictException with message "The provided updateToken is not the current updateToken for the resource".
5. Replay check network-security-manager.update-rule-rejects-all-prior-update-tokens-after-new-version.
