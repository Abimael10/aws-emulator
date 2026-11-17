Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch.cancel-jobs-over-fifty-ids-rejected batch.cancel-jobs-rejects-over-fifty-jobs batch.terminate-jobs-rejects-over-fifty-jobs batch.list-consumable-resources-rejects-max-results-out-of-range batch.list-jobs-by-consumable-resource-rejects-max-results-out-of-range batch.list-scheduling-policies-rejects-max-results-out-of-range batch.describe-compute-environments-rejects-over-hundred-items batch.register-job-definition-rejects-oversized-definition batch.update-consumable-resource-invalid-operation-rejected batch.update-consumable-resource-negative-quantity-rejected batch.terminate-service-jobs-bulk-without-permission-denied
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch.cancel-jobs-over-fifty-ids-rejected|run#21|AssertionError|assert message == expected_message, f"CancelJobs for 51 ids: expected message {expected_message!r}, got {message!r}"
Issue-Key: batch.cancel-jobs-rejects-over-fifty-jobs|run#6|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: batch.terminate-jobs-rejects-over-fifty-jobs|run#6|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: batch.list-consumable-resources-rejects-max-results-out-of-range|run#8|AssertionError|assert zero_message == expected_zero_message, (
Issue-Key: batch.list-jobs-by-consumable-resource-rejects-max-results-out-of-range|run#8|AssertionError|assert zero_message == expected_zero_message, (
Issue-Key: batch.list-scheduling-policies-rejects-max-results-out-of-range|run#8|AssertionError|assert zero_message == expected_zero_message, (
Issue-Key: batch.describe-compute-environments-rejects-over-hundred-items|_error_code#4|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: batch.register-job-definition-rejects-oversized-definition|_error_code#5|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: batch.update-consumable-resource-invalid-operation-rejected|run#8|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: batch.update-consumable-resource-negative-quantity-rejected|run#8|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: batch.terminate-service-jobs-bulk-without-permission-denied|run#12|AssertionError|assert code == "AccessDeniedException", f"expected AccessDeniedException, got {code!r}"
Issue-Commit: 70a3a5b79f16f8a83ae30f7fd1a45ab35d74c677

# batch: 7 net-new causes — bulk-job and page-size refusal messages are emulator-authored, DescribeComputeEnvironments/RegisterJobDefinition accept input AWS refuses, and TerminateServiceJobs answers NotImplemented instead of AccessDeniedException

> _Note: this issue was identified against the code base at commit `70a3a5b79f16f8a83ae30f7fd1a45ab35d74c677`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CancelJobs

## Description
Seven net-new root causes on the Batch control plane, all in the same place: the verdict and the message the emulator produces when a request is wrong (or when it is wrong and the emulator accepts it anyway). Batch has exactly ONE client error shape (ClientException, HTTP 400) for every client mistake, so the MESSAGE is the only machine-readable detail a refusal carries — a re-worded message is a defect, not cosmetics. Each cause must be closed before submitting.

Cause 1 of 7 — CancelJobs and TerminateJobs refuse an over-50 `jobs` list with emulator prose instead of Batch's enveloped sentence.
AWS: Smithy (com.amazonaws.batch) CancelJobs POST /v1/canceljobs and TerminateJobs POST /v1/terminatejobs, success 200, modeled errors ClientException (httpError 400) / ServerException (500); `jobs` (StringList) is @required and documented as "An array of up to 50 Batch job IDs". A list of 51 ids is refused with 400 / ClientException whose message is Batch's request envelope around a fixed sentence: "Error executing request, Exception : A maximum of 50 jobs can be specified per request., RequestId: <request id>". Fixed text: "Error executing request, Exception : A maximum of 50 jobs can be specified per request., RequestId: "; the only variable part is the call's own request id (the per-run uuid is not itself a requirement). The limit 50 is the documented one, not a recorded value.
Emulator: 400 / ClientException with the bare, un-enveloped sentence "Too many job IDs for one CancelJobs request: this emulator stores at most 50" (and "...for one TerminateJobs request..." for TerminateJobs) — the emulator's own prose, naming the emulator rather than the service, with no "Error executing request, Exception : ... , RequestId: ..." envelope.
Why: a client or test that tells an over-limit bulk request apart from any other ClientException by AWS's wording gets no match, and the sentence advertises an emulator-internal storage limit rather than the API's documented per-request maximum, so the caller cannot tell it must split its batch into 50-id chunks.
Fix: the message produced by the bulk-size guard in the CancelJobs and TerminateJobs handlers (services/service-com-amazonaws-batch-awsbatchv20160810/src/ops/cancel_jobs.rs and .../terminate_jobs.rs) and the shared `crate::state::quota_message` helper they call — it must emit Batch's enveloped fixed sentence. The verdict (400/ClientException) and the bound (50 accepted, 51 refused) are already correct, and the positive control of exactly 50 unknown ids reported `successful` passes.
Traces: batch.cancel-jobs-over-fifty-ids-rejected, batch.cancel-jobs-rejects-over-fifty-jobs, batch.terminate-jobs-rejects-over-fifty-jobs

Cause 2 of 7 — every maxResults bound refusal shares one generic emulator sentence, and ListJobsByConsumableResource's bound itself is wrong.
AWS: Smithy ListSchedulingPolicies, ListConsumableResources and ListJobsByConsumableResource all model `maxResults` as a plain Integer (no @range) and all model ClientException (400); each operation words its own bound refusal and each has its own maximum:
  • ListSchedulingPolicies: "Error executing request, Exception : Max Results should be greater than 1 and less than 100., RequestId: <request id>"
  • ListConsumableResources: "Error executing request, Exception : Max Results should be greater than 0 and less than or equal to 100, RequestId: <request id>"
  • ListJobsByConsumableResource: "Error executing request, Exception : Max Results should be greater than 0 and less than or equal to 1000, RequestId: <request id>" — its maximum is 1000, not 100.
In each the quoted sentence is fixed text (note the differing comparatives and the trailing period only on the first) and the only variable part is the call's own request id.
Emulator: all three answer one generic sentence from the shared `crate::state::max_results_bound_message` helper — "Error executing request, Exception : Max Results should be greater than or equal to 1 and less than or equal to 100., RequestId: <request id>" — for maxResults=0 and for the over-limit value alike. The envelope is right; the sentence is the emulator's own normalisation of three different AWS sentences. For ListJobsByConsumableResource the bound is also wrong: the handler caps at 100, so it refuses values in 101..=1000 that AWS accepts.
Why: a caller that matches the refusal to learn the page-size window is told the wrong window; worse, a client paginating ListJobsByConsumableResource with the documented maxResults=1000 is refused outright against the emulator and succeeds against AWS, so a working paginator cannot run here at all.
Fix: per-operation page-size validation in the ListSchedulingPolicies, ListConsumableResources and ListJobsByConsumableResource handlers (services/service-com-amazonaws-batch-awsbatchv20160810/src/ops/list_scheduling_policies.rs, list_consumable_resources.rs, list_jobs_by_consumable_resource.rs) plus the shared `max_results_bound_message` helper and the MIN_/MAX_LIST_* constants in src/state — each operation needs its own AWS sentence, and ListJobsByConsumableResource needs an upper bound of 1000.
Traces: batch.list-scheduling-policies-rejects-max-results-out-of-range, batch.list-consumable-resources-rejects-max-results-out-of-range, batch.list-jobs-by-consumable-resource-rejects-max-results-out-of-range

Cause 3 of 7 — DescribeComputeEnvironments accepts a computeEnvironments list longer than 100 and answers 200.
AWS: Smithy DescribeComputeEnvironments POST /v1/describecomputeenvironments, success 200, @paginated (pageSize maxResults), models ClientException (400). A `computeEnvironments` list of 101 entries is refused before any lookup with 400 / ClientException and the message "Error executing request, Exception : Number of compute environments should be between 1 and 100., RequestId: <request id>" — fixed text with only the call's request id variable; the bounds 1 and 100 are the API's. Exactly 100 unknown names is accepted and answers an empty `computeEnvironments`.
Emulator: HTTP 200 with `{"computeEnvironments": []}` for the 101-name request — the over-limit request is accepted and answered as a successful describe of nothing.
Why: the 400 is the only signal that the caller must chunk its describe. Accepted with 200 and an empty list, a client concludes that all 101 compute environments are absent — indistinguishable from a genuine not-found — so a batching bug or a typo'd name surfaces much later, somewhere else, and code written against AWS's chunking requirement is never exercised.
Fix: input validation in the DescribeComputeEnvironments handler (services/service-com-amazonaws-batch-awsbatchv20160810/src/ops/describe_compute_environments.rs) — bound the `computeEnvironments` list at 100 and refuse with 400 / ClientException carrying AWS's enveloped sentence, before the state lookup.
Traces: batch.describe-compute-environments-rejects-over-hundred-items

Cause 4 of 7 — RegisterJobDefinition registers a job definition over the 24 KB size limit.
AWS: Smithy RegisterJobDefinition POST /v1/registerjobdefinition, success 200, models ClientException (400). A request whose serialized definition exceeds Batch's documented 24 KB job-definition size limit is refused with 400 / ClientException and the message "Error executing request, Exception : JobDefinition size must be less than 24 KB, got <N> KB., RequestId: <request id>" — fixed text "Error executing request, Exception : JobDefinition size must be less than 24 KB, got " + " KB., RequestId: ", with two variable parts: <N>, the request's own compact-JSON byte length floored to whole KB, and the call's request id. Nothing is registered: a following DescribeJobDefinitions for that name returns [].
Emulator: HTTP 200 with jobDefinitionName/jobDefinitionArn/revision 1 — the oversized definition (a ~40 KB environment-variable value) is stored and becomes revision 1 of a usable job definition.
Why: the size ceiling is a hard service limit; a client that packs parameters, environment or overrides into a definition gets a working definition here and a 400 in production, so the emulator certifies a deployment AWS will reject, and the accepted definition also pollutes the account's job-definition listing.
Fix: request-size validation in the RegisterJobDefinition handler's validation layer (services/service-com-amazonaws-batch-awsbatchv20160810/src/ops/register_job_definition.rs) — measure the definition, refuse over 24 KB with the enveloped message above, and register nothing. The positive control (the same shape under the limit) already passes.
Traces: batch.register-job-definition-rejects-oversized-definition

Cause 5 of 7 — UpdateConsumableResource's closed-set refusal for `operation` names the member instead of AWS's noun.
AWS: Smithy UpdateConsumableResource POST /v1/updateconsumableresource; `operation` is a plain String documented "Must be one of: SET ... ADD ... REMOVE"; ClientException (400). A value outside that set is refused with "Error executing request, Exception : Update operation needs to be one of the following values: [SET, ADD, REMOVE], RequestId: <request id>" — fixed text, the value set is the model's, and only the request id varies.
Emulator: the same envelope but "... Exception : operation needs to be one of the following values: [SET, ADD, REMOVE], RequestId: ..." — AWS's noun "Update operation" is replaced by the bare member name, because the handler calls the service-wide `crate::state::closed_set_detail("operation", ...)` helper with the member name.
Why: Batch's single ClientException forces callers to match on message text; a refusal that drops AWS's noun fails those matches, and a generic "operation needs to be ..." reads as if the API call itself were wrong rather than the `operation` argument.
Fix: the closed-set message passed at the `operation` check in services/service-com-amazonaws-batch-awsbatchv20160810/src/ops/update_consumable_resource.rs (the `closed_set_detail` call) — it must read "Update operation". The verdict, the accepted set and the unchanged totalQuantity are already correct.
Traces: batch.update-consumable-resource-invalid-operation-rejected

Cause 6 of 7 — UpdateConsumableResource's negative-quantity refusal is bare emulator prose outside Batch's envelope.
AWS: `quantity` (Long) is documented "Must be a non-negative value."; a negative value is refused with 400 / ClientException and the message "Error executing request, Exception : Quantity cannot be a negative number., RequestId: <request id>" — entirely fixed text apart from the call's request id, and notably it does NOT echo the rejected value.
Emulator: 400 / ClientException with "quantity must be non-negative, not -1" — un-enveloped emulator prose that echoes the rejected value; the handler builds it with `client_exception` (raw) rather than `client_exception_enveloped`, unlike the sibling `operation` check in the same function.
Why: same single-error-shape consequence — a caller matching AWS's sentence sees an unrecognised failure, and the absence of the "Error executing request, Exception : ..., RequestId: ..." envelope also hides the request id AWS puts in every ClientException for support/correlation.
Fix: the negative-`quantity` guard in services/service-com-amazonaws-batch-awsbatchv20160810/src/ops/update_consumable_resource.rs — emit AWS's fixed sentence through the same enveloping path the `operation` guard uses.
Traces: batch.update-consumable-resource-negative-quantity-rejected

Cause 7 of 7 — TerminateServiceJobs answers NotImplemented where AWS answers an IAM AccessDeniedException, so the operation's authorization contract cannot be reached.
AWS: Smithy TerminateServiceJobs POST /v1/terminateservicejobs, success 200, `jobs` and `reason` @required, modeled errors ClientException (400) / ServerException (500). A caller whose role holds no batch permissions is refused at the authorization layer with AccessDeniedException and the message "User: <caller principal ARN> is not authorized to perform: batch:TerminateServiceJob on resource: <service-job ARN> because no identity-based policy allows the batch:TerminateServiceJob action" — fixed text "User: " / " is not authorized to perform: batch:TerminateServiceJob on resource: " / " because no identity-based policy allows the batch:TerminateServiceJob action", with the assumed-role ARN taken from the caller and the ARN arn:aws:batch:<region>:<account-id>:service-job/<job-id> taken from the request; note the action is the SINGULAR batch:TerminateServiceJob even for the bulk operation. The denied attempt must leave the job untouched (status not FAILED, isTerminated false), and a permitted principal then terminates it.
Emulator: the call is refused with error code 'NotImplemented' — the operation is an unimplemented scaffold stub, so neither the denial nor the bulk termination it gates can be observed. (The suite already files the stub itself for the sibling check batch.terminate-service-jobs-bulk-treats-unknown-id-as-successful; this check is NOT covered by that filing and is reported here for the authorization contract it asserts — closing the stub is necessary but not sufficient: the handler must also deny an unauthorized principal with the exact message above.)
Why: a client that relies on IAM to scope who may stop service jobs gets a NotImplemented it will treat as an emulator/infrastructure fault rather than a policy decision, and any test asserting that an unprivileged role cannot terminate a job passes vacuously or fails for the wrong reason — the overly-permissive path (no policy, job still terminated) is never exercised.
Fix: implement services/service-com-amazonaws-batch-awsbatchv20160810/src/ops/terminate_service_jobs.rs, including the per-job batch:TerminateServiceJob authorization check and its AccessDeniedException message, and leave the job's state unchanged on denial.
Traces: batch.terminate-service-jobs-bulk-without-permission-denied

Related open issues that do NOT cover these causes (named for context, none carries these checks' replay keys): 20261102T113649Z-batch-job-api-refusal-contracts-diverge-two-wrong.md (DescribeJobs/ListJobs/RegisterJobDefinition log-driver/SubmitJob wording), 20261113T130550Z-batch-describeschedulingpolicies-malformed-arn-refusal-message-still-diverges.md.

## Evidence
All seven from the simparity-behavioral-aws-v1 replay against this checkout (295 checks, 206 passed, 86 mismatched, 3 errored). Expected = real AWS behavior asserted by the check; actual = emulator.

Cause 1 — batch.cancel-jobs-rejects-over-fifty-jobs (CancelJobs, 51 ids), code ClientException on both sides:
  message: expected 'Error executing request, Exception : A maximum of 50 jobs can be specified per request., RequestId: <request id>'
           got      'Too many job IDs for one CancelJobs request: this emulator stores at most 50'
  batch.cancel-jobs-over-fifty-ids-rejected: identical divergence.
  batch.terminate-jobs-rejects-over-fifty-jobs (TerminateJobs, 51 ids):
  message: expected 'Error executing request, Exception : A maximum of 50 jobs can be specified per request., RequestId: <request id>'
           got      'Too many job IDs for one TerminateJobs request: this emulator stores at most 50'

Cause 2 — maxResults=0, code ClientException on both sides:
  batch.list-consumable-resources-rejects-max-results-out-of-range:
    expected 'Error executing request, Exception : Max Results should be greater than 0 and less than or equal to 100, RequestId: <id>'
    got      'Error executing request, Exception : Max Results should be greater than or equal to 1 and less than or equal to 100., RequestId: <id>'
  batch.list-jobs-by-consumable-resource-rejects-max-results-out-of-range:
    expected 'Error executing request, Exception : Max Results should be greater than 0 and less than or equal to 1000, RequestId: <id>'
    got      'Error executing request, Exception : Max Results should be greater than or equal to 1 and less than or equal to 100., RequestId: <id>'
    (the check also asserts maxResults=1001 refused and maxResults=1 accepted; the emulator's 100 cap additionally refuses 101..1000, which AWS accepts)
  batch.list-scheduling-policies-rejects-max-results-out-of-range:
    expected 'Error executing request, Exception : Max Results should be greater than 1 and less than 100., RequestId: <id>'
    got      'Error executing request, Exception : Max Results should be greater than or equal to 1 and less than or equal to 100., RequestId: <id>'

Cause 3 — batch.describe-compute-environments-rejects-over-hundred-items, DescribeComputeEnvironments with 101 names:
  expected: HTTP 400, ClientException, message 'Error executing request, Exception : Number of compute environments should be between 1 and 100., RequestId: <id>'
  got:      HTTP 200, body {'computeEnvironments': []} — 'expected the call to be refused, but it succeeded'

Cause 4 — batch.register-job-definition-rejects-oversized-definition, RegisterJobDefinition with a ~40 KB environment value:
  expected: HTTP 400, ClientException, message 'Error executing request, Exception : JobDefinition size must be less than 24 KB, got <N> KB., RequestId: <id>', and DescribeJobDefinitions for that name returns []
  got:      HTTP 200, {'jobDefinitionName': '<name>', 'jobDefinitionArn': 'arn:aws:batch:<region>:<account-id>:job-definition/<name>:1', 'revision': 1} — 'expected the call to be refused, but it succeeded'

Cause 5 — batch.update-consumable-resource-invalid-operation-rejected, operation='MULTIPLY', code ClientException on both sides:
  message: expected 'Error executing request, Exception : Update operation needs to be one of the following values: [SET, ADD, REMOVE], RequestId: <id>'
           got      'Error executing request, Exception : operation needs to be one of the following values: [SET, ADD, REMOVE], RequestId: <id>'

Cause 6 — batch.update-consumable-resource-negative-quantity-rejected, operation='ADD', quantity=-1, code ClientException on both sides:
  message: expected 'Error executing request, Exception : Quantity cannot be a negative number., RequestId: <id>'
           got      'quantity must be non-negative, not -1'

Cause 7 — batch.terminate-service-jobs-bulk-without-permission-denied, TerminateServiceJobs from a role with no policies:
  expected: code 'AccessDeniedException', message 'User: <restricted role ARN> is not authorized to perform: batch:TerminateServiceJob on resource: arn:aws:batch:<region>:<account-id>:service-job/<job-id> because no identity-based policy allows the batch:TerminateServiceJob action'
  got:      code 'NotImplemented'

## Steps to Reproduce
Boot the emulator from this checkout and replay the behavioral suite (simparity-behavioral-aws-v1), or by hand:

Cause 1: aws batch cancel-jobs --jobs <51 uuids> --reason t  -> 'Too many job IDs for one CancelJobs request: this emulator stores at most 50'; AWS: 'Error executing request, Exception : A maximum of 50 jobs can be specified per request., RequestId: ...'. Same with terminate-jobs.
Cause 2: aws batch list-scheduling-policies --max-results 0 ; aws batch list-consumable-resources --max-results 0 ; aws batch list-jobs-by-consumable-resource --consumable-resource <arn> --max-results 0 -> all three answer the same generic sentence. Also: aws batch list-jobs-by-consumable-resource --consumable-resource <arn> --max-results 1000 -> refused by the emulator, accepted by AWS.
Cause 3: aws batch describe-compute-environments --compute-environments <101 names> -> emulator HTTP 200 {"computeEnvironments": []}; AWS 400 ClientException.
Cause 4: aws batch register-job-definition --job-definition-name t --type container --container-properties '{"image":"public.ecr.aws/amazonlinux/amazonlinux:2023","resourceRequirements":[{"type":"VCPU","value":"1"},{"type":"MEMORY","value":"512"}],"environment":[{"name":"PADDING","value":"<40000 A characters>"}]}' -> emulator 200 with revision 1; AWS 400 ClientException and nothing registered.
Cause 5: aws batch create-consumable-resource --consumable-resource-name r --resource-type REPLENISHABLE --total-quantity 10 ; aws batch update-consumable-resource --consumable-resource r --operation MULTIPLY --quantity 5.
Cause 6: aws batch update-consumable-resource --consumable-resource r --operation ADD --quantity -1.
Cause 7: create a SAGEMAKER_TRAINING service environment + job queue, submit-service-job, then call aws batch terminate-service-jobs --jobs <jobId> --reason t with credentials for an IAM role holding no policies -> emulator 'NotImplemented'; AWS AccessDeniedException with the batch:TerminateServiceJob sentence, and the job left untouched.

Then re-replay: batch.cancel-jobs-over-fifty-ids-rejected, batch.cancel-jobs-rejects-over-fifty-jobs, batch.terminate-jobs-rejects-over-fifty-jobs, batch.list-consumable-resources-rejects-max-results-out-of-range, batch.list-jobs-by-consumable-resource-rejects-max-results-out-of-range, batch.list-scheduling-policies-rejects-max-results-out-of-range, batch.describe-compute-environments-rejects-over-hundred-items, batch.register-job-definition-rejects-oversized-definition, batch.update-consumable-resource-invalid-operation-rejected, batch.update-consumable-resource-negative-quantity-rejected, batch.terminate-service-jobs-bulk-without-permission-denied.
