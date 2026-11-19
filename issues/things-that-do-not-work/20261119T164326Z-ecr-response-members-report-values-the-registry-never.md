Issue-Source: trace-sweep-divergence
Issue-Trace: api-account-settings-and-authorization-token
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: api-account-settings-and-authorization-token ecr-repository-immutable-aes256-scan-off-own-account-policy-and-lifecycle-round-trip
Issue-Shape-Id: com.amazonaws.ecr#AmazonEC2ContainerRegistry_V20150921
Issue-Key: ecr|GetAuthorizationToken|2xx->2xx|$.authorizationData[*]
Issue-Key: ecr|GetLifecyclePolicy|2xx->2xx|$.lastEvaluatedAt
Issue-Key: ecr|DeleteLifecyclePolicy|2xx->2xx|$.lastEvaluatedAt
Issue-Commit: 23a6305fb4194ac652f562ce813f73dafa581013

# ecr: response members report values the registry never produced — GetAuthorizationToken's proxyEndpoint authority and the lifecycle-policy lastEvaluatedAt (3 causes)

> _Note: this issue was identified against the code base at commit `23a6305fb4194ac652f562ce813f73dafa581013`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAuthorizationToken, GetLifecyclePolicy, DeleteLifecyclePolicy

## Description
Three net-new wire divergences in this corpus, all of them a response member the ECR emulator fills from something other than what the registry actually did: the registry address a `docker login` is pointed at, and the instant a lifecycle policy was last evaluated. All three answer HTTP 200 — the status and the shape are right, the reported values are not.

Cause 1 of 3: GetAuthorizationToken announces a registry endpoint that is not the address the caller reaches the emulator on.

AWS: Smithy AuthorizationData$proxyEndpoint — "The registry URL to use for this authorization token in a `docker login` command. The Amazon ECR registry URL format is https://aws_account_id.dkr.ecr.region.amazonaws.com" — i.e. the authority that actually serves the Docker Registry v2 API for that account/region, which a client is documented to feed verbatim to `docker login --username AWS --password <token> <proxyEndpoint>` and to derive every pull/push base URL from. AuthorizationData$authorizationToken is the base64 of `user:password`, and AuthorizationData$expiresAt is 12 hours out ("Authorization tokens are valid for 12 hours"). For an emulator the account id, host and port inside that URL are per-environment values; the requirement is that the announced authority is one the emulator itself serves on the address this caller is talking to, so the registry plane behind it is reachable. The corpus asserts this element by shape only (recorded behavior: each of authorizationToken/expiresAt/proxyEndpoint is recorded as `<shaped-not-asserted>`), and the replay refuses the element the emulator returns.

Emulator: the single authorizationData[0] it returns carries proxyEndpoint `https://127.0.0.1:43117` — a bare loopback authority with no registry identity in it at all, and on a port other than the one the emulator is being reached on in this very replay (the run reports "/__admin/health answered 200 and the :43741 data listener accepts"). The fixed part of the defect is the shape: an authority composed by the emulator's own endpoint logic rather than the authority of the request that is being answered; `127.0.0.1`, `43117` and `43741` are per-run values. Hypothesis: the composition in the handler (`crate::state::registry_login_url(&crate::state::announced_registry_endpoint(ctx))`) derives a host and port of its own instead of echoing the authority the request arrived on.

Why: a client that does exactly what AWS documents — hand proxyEndpoint to `docker login`, or build the pull/push base URL from it — is sent to an authority that does not address the registry plane it just authenticated against. GetAuthorizationToken looks successful (a well-formed 12-hour token comes back) and then every login, manifest, blob, pull and push attempt fails at connect time, so no container-image workflow can use the emulated registry even though the control plane answers.

Related but NOT covering this: open issue 20261118T005003Z-ecr-getauthorizationtoken-publishes-a-proxyendpoint-on-a-host.md reports the same component announcing `https://<accountId>.dkr.ecr.<region>.localhost:<port>` from a behavioral check; it carries no replay key for this fixture step and the announced value here is a different one (a bare loopback authority on a non-serving port), so this wire step stays unexercised by it. Open issue 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md owns the sibling defect on Repository$repositoryUri and does not cover this operation or this member.

Fix: the GetAuthorizationToken handler's endpoint composition — services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/src/ops/get_authorization_token.rs and the announced-endpoint helpers in crate::state it calls — must announce the registry authority this request was made to (the same one Repository$repositoryUri is built on), and the registry v2 plane must answer there.

Cause 2 of 3: GetLifecyclePolicy reports a lastEvaluatedAt for a policy that has never been evaluated.

AWS: Smithy GetLifecyclePolicyResponse$lastEvaluatedAt (target EvaluationTimestamp) — "The time stamp of the last time that the lifecycle policy was run." Recorded behavior: in the fixture the policy is put and then read back without any lifecycle evaluation having run, and the recorded response reports lastEvaluatedAt as the epoch-0 sentinel (0.0) — i.e. storing a policy is not a run of it, so there is no last-run instant to report yet. The requirement is the semantics: the member reports an actual evaluation of the policy, and nothing else advances it.

Emulator: it reports the instant the policy was STORED. The replay sees `$.lastEvaluatedAt: value_mismatch (expected 0.0, got 1795106264.435)` on a 200 response; the particular epoch value is per-run, the defect is that a put-time instant is reported as an evaluation instant. In the source the stored record carries that instant by design — services/.../src/ops/get_lifecycle_policy.rs line 221 reports `policy.last_evaluated_at` with the comment "The policy was evaluated when it was set".

Why: a client that decides whether the lifecycle rules have taken effect yet — or computes an age since the last evaluation, or waits for the first evaluation before asserting on expired images — is told a run happened the registry never performed, so it skips the wait or reports a policy as enforced while no image has ever been evaluated against it.

Fix: the ECR lifecycle-policy state (the `last_evaluated_at` field written when a policy is stored, in services/.../src/state.rs) plus the GetLifecyclePolicy handler that reports it: the field must be advanced only by an actual lifecycle evaluation, and reported as the never-evaluated sentinel (epoch 0) until one has run.

Cause 3 of 3: DeleteLifecyclePolicy reports the same wrong lastEvaluatedAt.

AWS: Smithy DeleteLifecyclePolicyResponse$lastEvaluatedAt (target EvaluationTimestamp) — "The time stamp of the last time that the lifecycle policy was run", carried on the delete response as the final report of the policy being removed. Recorded behavior: for a policy removed without ever having been evaluated, the recorded response reports the epoch-0 sentinel (0.0).

Emulator: the delete response echoes the same put-time instant — `$.lastEvaluatedAt: value_mismatch (expected 0.0, got 1795106264.435)` on a 200. It is the same stored field read by a second handler, so it diverges identically wherever that field is reported.

Why: the delete response is the record a client keeps of what was removed; it attributes an evaluation to a policy that was never run, and it does so even in flows that never read the policy back, so the wrong value escapes through a second path.

Fix: the DeleteLifecyclePolicy handler (services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/src/ops/delete_lifecycle_policy.rs) must report the same corrected evaluation instant as cause 2 — one source of truth in the lifecycle-policy state, set only by a real evaluation.

Traces: api-account-settings-and-authorization-token#task#6 (cause 1); ecr-repository-immutable-aes256-scan-off-own-account-policy-and-lifecycle-round-trip#task#6 (cause 2); ecr-repository-immutable-aes256-scan-off-own-account-policy-and-lifecycle-round-trip#task#10 (cause 3).

## Evidence
Replay of trace set simparity-wire-aws-v1 (77 fixtures, 567 steps, 514 passed, 53 mismatched, score 90.7%). Emulator up: "/__admin/health answered 200 and the :43741 data listener accepts". Three divergence clusters below are net-new (no stub_path, no owned_by); the other five ECR clusters in this run are owned by existing issues and are not reported here.

Cause 1 — ecr | GetAuthorizationToken | expected status 200, actual status 200 | fixture api-account-settings-and-authorization-token step task#6 | divergence_key service=ecr|GetAuthorizationToken|status=200->200|keys=$.authorizationData[0]
  Expected (recorded): authorizationData[0] = {'authorizationToken': '<shaped-not-asserted>', 'expiresAt': '<shaped-not-asserted>', 'proxyEndpoint': '<shaped-not-asserted>'} — the element asserted by shape.
  Actual: authorizationData[0] = {'authorizationToken': 'QVdTOnNBYnRjMExNSWtUZVNkSlRiZXpLaU9nWC...' (base64 of 'AWS:<password>'), 'expiresAt': 1795149520.946, 'proxyEndpoint': 'https://127.0.0.1:43117'}
  Verdict: mismatch — "$.authorizationData[0]: missing / extra" (the returned element is not the recorded AuthorizationData). The announced authority 127.0.0.1:43117 is not the address the replay reaches the emulator on (:43741), and carries no registry identity.

Cause 2 — ecr | GetLifecyclePolicy | expected status 200, actual status 200 | fixture ecr-repository-immutable-aes256-scan-off-own-account-policy-and-lifecycle-round-trip step task#6 | divergence_key service=ecr|GetLifecyclePolicy|status=200->200|keys=$.lastEvaluatedAt
  Detail: "$.lastEvaluatedAt: value_mismatch (expected 0.0, got 1795106264.435)"
  Expected: lastEvaluatedAt = 0 (never evaluated). Actual: the instant PutLifecyclePolicy stored the document.

Cause 3 — ecr | DeleteLifecyclePolicy | expected status 200, actual status 200 | fixture ecr-repository-immutable-aes256-scan-off-own-account-policy-and-lifecycle-round-trip step task#10 | divergence_key service=ecr|DeleteLifecyclePolicy|status=200->200|keys=$.lastEvaluatedAt
  Detail: "$.lastEvaluatedAt: value_mismatch (expected 0.0, got 1795106264.435)"
  Expected: lastEvaluatedAt = 0 (never evaluated). Actual: the same stored put-time instant.

Model citations used: AuthorizationData$proxyEndpoint / $authorizationToken / $expiresAt; GetLifecyclePolicyResponse$lastEvaluatedAt; DeleteLifecyclePolicyResponse$lastEvaluatedAt (com.amazonaws.ecr, services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/smithy-model/com-amazonaws-ecr-amazonec2containerregistry-v20150921.json).

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Re-replay the wire corpus simparity-wire-aws-v1 and look at these three steps: api-account-settings-and-authorization-token#task#6, ecr-repository-immutable-aes256-scan-off-own-account-policy-and-lifecycle-round-trip#task#6 and #task#10. All three must pass before this issue is closed.
3. By hand, cause 1: call ECR GetAuthorizationToken and read authorizationData[0].proxyEndpoint. It is `https://127.0.0.1:<some port>` — an authority with no registry identity and not the host:port the call itself was made to. Point `docker login --username AWS --password <decoded token>` at it, or GET <proxyEndpoint>/v2/ with the token as HTTP Basic credentials: nothing answers. Against AWS the announced endpoint is the registry's own and serves the v2 API.
4. By hand, causes 2 and 3: CreateRepository, then PutLifecyclePolicy with a valid policy document, then GetLifecyclePolicy on that repository — lastEvaluatedAt comes back as the put-time instant instead of 0 even though no lifecycle evaluation has run. Then DeleteLifecyclePolicy on the same repository — its response echoes that same instant instead of 0.
