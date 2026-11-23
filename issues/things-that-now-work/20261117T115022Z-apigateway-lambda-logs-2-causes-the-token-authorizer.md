Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation apigateway__lambda__logs.apigw-method-level-log-level-overrides-the-stage-wide-level
Issue-Key: apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation|_stays_absent#22|Timeout|time.sleep(interval_s)
Issue-Key: apigateway__lambda__logs.apigw-method-level-log-level-overrides-the-stage-wide-level|run#24|AssertionError|assert ms.get("~1echo/POST", {}).get("loggingLevel") == "INFO", (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# apigateway__lambda__logs: 2 causes — the TOKEN-authorizer deny scenario never reaches a verdict, and a stage's method-level setting is keyed without the escaped resource path

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetStage

## Description
Two net-new root causes in the API Gateway REST stage/authorizer surface.

Cause 1 of 4... (correction: Cause 1 of 2): a TOKEN authorizer that returns Deny never produces an observable outcome.
AWS: recorded behavior only — the Smithy model for apigateway describes the control plane (CreateAuthorizer/CreateDeployment/CreateStage), not the execute-api data plane. What the scenario requires is AWS's documented TOKEN-authorizer contract: a request whose authorizationToken yields a policyDocument with Effect Deny for the invoked methodArn is answered 403 by execute-api and the backend integration is NOT invoked (no backend log output), while an allowing token is answered 200 and the backend marker appears.
Emulator: the check makes no progress at all and is killed by the harness at its 60 s budget — neither the deny (403, no backend invocation) nor the allow (200, backend marker) half ever becomes observable.
Why: a client cannot tell 'authorizer denied' from 'authorizer ignored'; an authorizer-gated API appears to hang rather than to enforce.
Fix: the execute-api invoke path's authorizer evaluation stage (TOKEN authorizer invocation, policy evaluation, and the 403 short-circuit before the integration call).
Failing check: apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation

Cause 2 of 2: a method-level MethodSetting is keyed by the bare resource path, not by the escaped path AWS uses.
AWS: Smithy apigateway Stage$methodSettings documents the map keys as method paths '{resource_path}/{http_method}' for an individual method override, or '/*/*' for the stage-wide override; AWS escapes the resource path's '/' as '~1' in that key, so an override on resource /echo and method POST reads back under the key '~1echo/POST' (the fixed shape is '~1'+<resource path segments>+'/'+<HTTP method>; the path and method are values from the request).
Emulator: GetStage returns the same override under the key 'echo/POST' — the leading escaped separator is dropped — alongside the correctly keyed '*/*' entry. The setting's own values (loggingLevel INFO) are right; only the key diverges.
Why: every client that reads or writes a per-method setting addresses it by that key (CDK, console, `aws apigateway get-stage`/`update-stage` with /~1echo/POST patch paths). Under the emulator's key the override is invisible to a lookup by AWS's key and a patch against AWS's key creates a second, duplicate entry.
Fix: the Stage serializer's methodSettings key construction (and the matching key parsing in UpdateStage's patch handling).
Failing check: apigateway__lambda__logs.apigw-method-level-log-level-overrides-the-stage-wide-level

## Evidence
Cause 1: [mismatch] apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation — 'Error: Check timed out after 60.0s'. Expected: a Deny policy from the TOKEN authorizer answers 403 and the backend Lambda logs no 'BACKEND-INVOKED <marker>' line, while an allowing token returns 200 with the marker. Actual: no verdict within the budget.

Cause 2: [mismatch] apigateway__lambda__logs.apigw-method-level-log-level-overrides-the-stage-wide-level — AssertionError: "method-level override should be keyed '~1echo/POST' with loggingLevel INFO"; the emulator returned methodSettings {'*/*': {... 'loggingLevel': 'ERROR' ...}, 'echo/POST': {... 'loggingLevel': 'INFO' ...}} — the method override is present but keyed 'echo/POST' instead of '~1echo/POST'.

## Resolution

Cause 2 — FIXED, and confirmed by the replay (`apigw-method-level-log-level-overrides-the-stage-wide-level`
passes): a stage keys one `methodSettings` entry by the method path its `UpdateStage` pointer
SPELLED — the resource half written exactly as the request wrote it, its separators escaped `~1`
as `PatchOperation$path` requires — so the recorded patch reads back under `~1echo/POST`. The
keying lives in `src/ops/update_stage.rs` (`method_setting` -> `addressed_method_path` ->
`written_tokens`, with the one-token spelling keyed by the method path it decodes to); it landed
before this report was worked, which the issue's own drift note anticipates.

Cause 1 — DECLINED: no divergence of this emulator remains. A `CUSTOM`-authorized method of a
deployed stage answers the recorded outcomes — `403` with API Gateway's explicit-deny sentence
and `AccessDeniedException`, `401` `{"message":"Unauthorized"}` for a request carrying nothing at
the identity source, and the integration's own `200` for an allowing token — never performs the
integration for a refused request, logs the authorizer's own run, and writes one access-log
record per answer carrying the stage-qualified `$context.path`. That is pinned end to end by
`tests/all/stage_authorizer_deny_blocks_the_backend.rs`, by
`core/router/tests/all/apigateway_token_authorizer_deny_and_access_log.rs` on the emulator as it
is assembled, and — added for this report —
`tests/all/stage_authorizer_decision_on_the_served_wire.rs`, which asserts the same decisions as
a client READING THE WIRE sees them: the router served by the framework that serves this
emulator's data port, five rows on one keep-alive connection, each answer read by its own
`content-length`, under both addressings of a deployed stage.

What still fails on replay is the check's own wall clock: `Error: Check timed out after 60.0s`
inside its absence-watch helper (`_stays_absent`, at `time.sleep(interval_s)`), which is a
fixed-window watch rather than an answer of this emulator — the same check run under the earlier
180 s cap (issue 20261107T191316Z) got PAST that helper and expired later. The emulator's whole
share of this scenario (two functions created and Active, the authorizer invoked per request,
two Lambda runs, the log deliveries and five served invokes) measures under four seconds in the
wire suite above, so there is no emulator behaviour left for the check to observe differently.

## Steps to Reproduce
1. Replay apigateway__lambda__logs.apigw-method-level-log-level-overrides-the-stage-wide-level: create a REST API with resource /echo and method POST, deploy a stage, set a stage-wide MethodSetting ('*/*') with loggingLevel ERROR and a method-level override for /echo POST with loggingLevel INFO, then call GetStage and inspect the methodSettings map keys.
2. Replay apigateway__lambda__logs.apigw-lambda-authorizer-deny-blocks-backend-invocation: attach a TOKEN authorizer whose handler returns Effect Deny unless authorizationToken == 'allow-me', invoke the deployed method with a denying token (expect 403 and no backend log marker) and then with 'allow-me' (expect 200 plus the marker).
