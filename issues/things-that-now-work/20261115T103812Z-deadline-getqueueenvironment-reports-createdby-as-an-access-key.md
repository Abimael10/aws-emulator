Issue-Source: trace-sweep-divergence
Issue-Trace: deadline-queue-environment-create-get-semantics
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: deadline-queue-environment-create-get-semantics
Issue-Shape-Id: com.amazonaws.deadline#Deadline
Issue-Key: deadline|GetQueueEnvironment|2xx->2xx|$.createdBy
Issue-Commit: f40222b80bff0bb01bc5725930758a9790cd3d69

# deadline GetQueueEnvironment reports createdBy as an access-key ARN, not the caller principal

> _Note: this issue was identified against the code base at commit `f40222b80bff0bb01bc5725930758a9790cd3d69`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueueEnvironment

## Description
GetQueueEnvironment answers 200 with a body whose audit member `createdBy` names a principal the service never had: instead of the identity of the caller that created the queue environment, it reports an IAM-user ARN whose resource part is the SigV4 access key id taken off the request headers. 1 cause, on service deadline.

Cause 1 of 1: GetQueueEnvironment.createdBy carries a synthesized access-key ARN rather than the calling principal's identity (fixture deadline-queue-environment-create-get-semantics, step task#5).
AWS: Smithy (services/service-com-amazonaws-deadline-deadline/smithy-model/com-amazonaws-deadline-deadline.json) models GetQueueEnvironment as GET /2023-10-12/farms/{farmId}/queues/{queueId}/environments/{queueEnvironmentId}, @readonly, success 200, with output members queueEnvironmentId, name, priority, templateType, template, createdAt (@timestampFormat date-time) and createdBy all @required, plus optional updatedAt/updatedBy; createdBy targets CreatedBy and is documented as "The user or system that created this resource." The model puts no pattern or length on the string, so its lexical form is pinned by the recording: the recorded response reports the caller's principal identity, an IAM user ARN whose resource is the identity NAME of the principal that called CreateQueueEnvironment (in the recording, the emulator's canonical caller identity, `.../user/emulator`). The account-id segment of that ARN is environment-specific and is not the requirement; what is required is that the resource segment be the caller's principal identity, stable for the same caller and identical on every later read of the same resource. Hypothesis: AWS derives it from the authenticated principal (its user/role name), which is why it does not vary with the credential material used to sign a particular request.
Emulator: the step gets HTTP 200 with `$.createdBy` = `arn:aws:iam::000000000000:user/EMULATOR5CD6137506FF` where the recording has `arn:aws:iam::000000000000:user/emulator` (replay detail: "$.createdBy: value_mismatch"). Source of the value: crate::state::caller_principal (services/service-com-amazonaws-deadline-deadline/src/state.rs, fn at ~line 1307) builds the stored principal as `engine::emulator_arn("iam", "", format!("user/{akid}"))` with `akid = engine::extract_akid(&ctx.parts().headers)` — i.e. the access key id of the signing credential, and for an unsigned request the framework's per-run `EMULATOR<hex>` fallback key id. CreateQueueEnvironment stamps that string into QueueEnvironment.created_by (src/ops/create_queue_environment.rs:274) and GetQueueEnvironment serializes it back faithfully, so the serializer is correct and the derivation is the defect. The same helper feeds createdBy/updatedBy for every other deadline resource, so any read whose createdBy is asserted literally diverges the same way.
Why: a client that reads createdBy to attribute a queue environment (audit export, ownership display, drift reconciliation, "did my principal create this" checks) gets a different class of identifier than AWS returns — an access key id rather than a principal identity — and that identifier changes whenever the caller's credentials or signing mode change, so the same resource created by the same principal appears to have different creators across runs and comparisons against a principal identity never match.
Fix: the stored-principal derivation — crate::state::caller_principal in src/state.rs and the create handlers that stamp it into state (src/ops/create_queue_environment.rs and the sibling creates that set created_by/updated_by) — must resolve the caller's principal identity instead of interpolating the raw access key id; GetQueueEnvironment's serializer needs no change. Related open issue 20261029T171822Z-identitystore-createdby-updatedby-report-a-synthesized-iam-access.md reports the identical defect in the identitystore crate's own copy of this helper and does NOT cover this service or this fixture step.
Traces: deadline-queue-environment-create-get-semantics#task#5 (verdict mismatch; 200 expected and actual, keys $.createdBy).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false, 71 steps, 54 passed, score 76.1%). Service deadline is bootstrapped (service-com-amazonaws-deadline-deadline is in get_bootstrap_status().bootstrapped).

Cause 1 — fixture deadline-queue-environment-create-get-semantics, step task#5, operation GetQueueEnvironment, verdict mismatch, divergence_key service=deadline|GetQueueEnvironment|status=200->200|keys=$.createdBy, affected_count 1:
  Expected (AWS, recorded): HTTP 200, $.createdBy = 'arn:aws:iam::000000000000:user/emulator' — an IAM-user ARN whose resource segment is the calling principal's identity name; the account segment is environment-specific and not part of the requirement.
  Actual (emulator):        HTTP 200, $.createdBy = 'arn:aws:iam::000000000000:user/EMULATOR5CD6137506FF' — the resource segment is the request's SigV4 access key id (here the framework's unsigned-request EMULATOR<hex> fallback), which is per-credential/per-run and is not a principal identity.
  Replay detail: "$.createdBy: value_mismatch (expected 'arn:aws:iam::000000000000:user/emulator', got 'arn:aws:iam::000000000000:user/EMULATOR5CD6137506FF')". All other asserted members of the response (queueEnvironmentId, name, priority, templateType, template, createdAt) matched.

Model evidence (smithy_query, model_path services/service-com-amazonaws-deadline-deadline/smithy-model/com-amazonaws-deadline-deadline.json):
  GetQueueEnvironment: GET /2023-10-12/farms/{farmId}/queues/{queueId}/environments/{queueEnvironmentId}, 200, @readonly; output requires queueEnvironmentId (^queueenv-[0-9a-f]{32}$), name, priority (0..10000), templateType (JSON|YAML), template (len 1..15000, @sensitive), createdAt (date-time), createdBy; optional updatedAt/updatedBy; errors AccessDenied 403, InternalServerError 500, ResourceNotFound 404, Throttling 429, Validation 400.
  GetQueueEnvironmentResponse$createdBy documentation: "The user or system that created this resource." (no pattern/length constraint).
  CreateQueueEnvironment: POST /2023-10-12/farms/{farmId}/queues/{queueId}/environments, @idempotent, output queueEnvironmentId only — so createdBy is observable only through the read.

Code evidence: services/service-com-amazonaws-deadline-deadline/src/state.rs, caller_principal() -> engine::emulator_arn("iam", "", &format!("user/{akid}")) with akid from engine::extract_akid(headers) (~lines 1304-1310); services/service-com-amazonaws-deadline-deadline/src/ops/create_queue_environment.rs:274 sets created_by: crate::state::caller_principal(ctx).

## Steps to Reproduce
1. Clone the repo and boot the emulator from this checkout; deadline is bootstrapped.
2. deadline.CreateFarm(displayName=<unique>) -> farmId
3. deadline.CreateQueue(farmId, displayName=<unique>) -> queueId
4. deadline.CreateQueueEnvironment(farmId, queueId, priority=<n>, templateType="JSON", template=<a template declaring an environment name>) -> queueEnvironmentId
5. deadline.GetQueueEnvironment(farmId, queueId, queueEnvironmentId)
   -> AWS: HTTP 200 with createdBy = an IAM-user ARN whose resource segment is the calling principal's identity (recorded: arn:aws:iam::<account>:user/emulator).
   -> Emulator: HTTP 200 with createdBy = arn:aws:iam::<account>:user/<access-key-id of the request> (e.g. .../user/EMULATOR5CD6137506FF for an unsigned replay), which also changes whenever the signing credential changes.
6. Replay with: replay_wire_corpus, fixture deadline-queue-environment-create-get-semantics, step task#5.
