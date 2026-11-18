Issue-Source: trace-sweep-divergence
Issue-Trace: sagemaker-space-and-userprofile-error-paths
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sagemaker-space-and-userprofile-error-paths
Issue-Shape-Id: com.amazonaws.sagemaker#SageMaker
Issue-Key: sagemaker|CreateUserProfile|4xx->4xx|$.message
Issue-Commit: 08ef6eb54c6e14f189e0f2c7e0cb9a68ecb9a3a2

# sagemaker CreateUserProfile missing-domain message names the domain ARN instead of the request DomainId

> _Note: this issue was identified against the code base at commit `08ef6eb54c6e14f189e0f2c7e0cb9a68ecb9a3a2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateUserProfile

## Description
One cause.

Cause 1 of 1: CreateUserProfile against a DomainId no domain exists under is refused with the right status and error shape, but the message text is rendered from the synthesized domain ARN and carries a trailing period, so it is not the sentence AWS words for this refusal.

AWS: Smithy (services/service-com-amazonaws-sagemaker-sagemaker/smithy-model/com-amazonaws-sagemaker-sagemaker.json) models CreateUserProfile with input member DomainId (target DomainId, @length 0..63, @pattern ^d-(-*[a-z0-9]){1,61}$, required) and declares only ResourceInUse and ResourceLimitExceeded, so the not-found refusal is not modeled and the wording is recorded behavior only. The recording shows HTTP 400 with __type ValidationException and the message "Domain [<DomainId>] does not exist": the fixed text is "Domain [" ... "] does not exist" with NO trailing period, and the single variable part is the DomainId exactly as the request spelled it (in the recording, the value the caller sent for a domain that does not exist). The status (400) and the error code (ValidationException) already match.

Emulator: the same step answers HTTP 400 / ValidationException but with the message "Domain [arn:aws:sagemaker:<region>:<account-id>:domain/<DomainId>] does not exist." — it substitutes a constructed domain ARN where AWS puts the bare DomainId, and appends a '.' that AWS's sentence does not have. The replay reports this as $.message value_mismatch (expected 'Domain [d-...] does not exist', got 'Domain [arn:aws:sagemaker:us-west-2:000000000000:domain/d-...] does not exist.'); the region and account-id in the emulator's rendering are per-environment values and are not part of any requirement — the defect is that an ARN is interpolated at all, plus the extra period.

Why: a client that reads the refusal to report or to key on which domain was missing gets an ARN-shaped string where AWS gives the domain id, so echo/extraction of the DomainId from the message fails, and any text comparison against AWS's sentence fails on the trailing period as well. Error-message assertions in SDK integration tests and user-facing error surfaces therefore diverge from real SageMaker.

Fix: the not-found branch of the CreateUserProfile handler's validation path — it builds the refusal with the ARN-forming message helper instead of a helper that interpolates the request's DomainId, and the helper's sentence ends in a period. Render the message as "Domain [<DomainId>] does not exist" (no trailing period) using the DomainId taken from the request, and keep the existing 400 / ValidationException. Other readers of a DomainId in this service that share the same message helper should be checked so they do not regress the wording in the other direction.

Traces: sagemaker-space-and-userprofile-error-paths#task#2 (CreateUserProfile, expected 400 and got 400, $.message diverges). Related open issues cover different causes and do not carry this replay key: 20261113T114936Z-sagemaker-createuserprofile-stub.md (filed when the operation was still a 501 scaffold; the operation now answers and validates, only the message text diverges), 20261113T114936Z-sagemaker-8-net-new-causes-createdomain-accepts-a.md and 20261118T212957Z-sagemaker-7-net-new-causes-a-deleted-app.md (Studio domain/app lifecycle behavior, not this message).

## Evidence
Fixture sagemaker-space-and-userprofile-error-paths, step task#2, operation CreateUserProfile, service sagemaker.

Expected (recorded AWS): status 400, __type ValidationException, $.message = "Domain [d-sim2real-probe-sagemaker-nodomain-887bf983] does not exist".
Actual (emulator): status 400, __type ValidationException, $.message = "Domain [arn:aws:sagemaker:us-west-2:000000000000:domain/d-sim2real-probe-sagemaker-nodomain-887bf983] does not exist.".
Replay detail: "$.message: value_mismatch"; divergence_key service=sagemaker|CreateUserProfile|status=400->400|keys=$.message; verdict mismatch; stub_path empty; owned_by empty (net-new).

Message anatomy: fixed text "Domain [" + "] does not exist" (no terminal period); variable part = the DomainId value carried by the request. The recorded id value itself is a per-run probe name and is not a requirement.

Code: services/service-com-amazonaws-sagemaker-sagemaker/src/ops/create_user_profile.rs lines 191-198 return CreateUserProfileError::validation(crate::state::domain_not_found(&region, &input.domain_id)) — the helper is handed the region and so composes an ARN for the message.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Call sagemaker CreateUserProfile with a syntactically valid DomainId (matching ^d-(-*[a-z0-9]){1,61}$) that no domain exists under, plus any valid UserProfileName.
3. Observe HTTP 400 with __type ValidationException (correct) and message "Domain [arn:aws:sagemaker:<region>:<account>:domain/<DomainId>] does not exist.".
4. AWS answers the same status and code with message "Domain [<DomainId>] does not exist" — bare domain id, no trailing period.
5. Equivalent: replay fixture sagemaker-space-and-userprofile-error-paths step task#2.
