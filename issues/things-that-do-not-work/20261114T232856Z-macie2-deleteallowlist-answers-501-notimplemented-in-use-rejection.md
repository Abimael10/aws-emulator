Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: macie2.delete-allow-list-in-use-rejected
Issue-Shape-Id: com.amazonaws.macie2#Macie2
Issue-Key: macie2.delete-allow-list-in-use-rejected|run#11|AssertionError|assert error["Code"] == "ValidationException", f"expected ValidationException, got {error['Code']!r}"
Issue-Commit: b9ec11b758c0a88959ff4f7c1c663fc7da468908

# macie2 DeleteAllowList answers 501 NotImplemented: in-use rejection, ignoreJobChecks override and post-delete ResourceNotFoundException are all unimplemented

> _Note: this issue was identified against the code base at commit `b9ec11b758c0a88959ff4f7c1c663fc7da468908`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteAllowList

## Description
One root cause (Cause 1 of 1): macie2 `DeleteAllowList` is still an unimplemented scaffold stub, so every behavior the operation is responsible for — the job-reference check, the `ignoreJobChecks` override, and the resulting absence of the allow list — is unobservable. (Note: the suite did NOT mark this check as covered by a stub issue, and no open issue names DeleteAllowList; the existing open macie2 issues cover CreateAllowList/GetFindings messages, ListMembers, and the TestCustomDataIdentifier / ListAutomatedDiscoveryAccounts / DeleteMember / GetFindingStatistics stubs only.)

AWS: Smithy `com.amazonaws.macie2#DeleteAllowList` — DELETE /allow-lists/{id}, input `id` (@httpLabel, required) and `ignoreJobChecks` (@httpQuery "ignoreJobChecks"); success is HTTP 200 with an empty body; modeled errors AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), ValidationException(400). The model's own documentation for `ignoreJobChecks` states the rule: "When you try to delete an allow list, Amazon Macie checks for classification jobs that use the list and have a status other than COMPLETE or CANCELLED. By default, Macie rejects your request if any jobs meet these criteria. To skip these checks and delete the list, set this value to true." So with `ignoreJobChecks=false` and at least one referencing job in a non-terminal status, AWS refuses with HTTP 400 `ValidationException` whose message is, per the recorded behavior, the fixed text "The allow list can't be deleted. N jobs are configured to use the list and those jobs have a status other than Complete or Cancelled." — everything there is fixed text except `N`, which is the count of referencing non-terminal jobs taken from the emulator's state (the recording shows `1` because exactly one job referenced the list). The allow list must still exist after that refusal. With `ignoreJobChecks=true` the delete succeeds (200, empty body), and afterwards both `GetAllowList` and `UpdateAllowList` on that id must fail with HTTP 404 `ResourceNotFoundException` and the message "Input validation failed. The request specifies an allow list that does not exist." (fixed text; no variable parts).

Emulator: `DeleteAllowList` returns HTTP 501 with error code `NotImplemented` and message "operation DeleteAllowList is scaffolded but not implemented in this emulator" for every request, regardless of `ignoreJobChecks` or of whether any classification job references the list. The check observed `error['Code'] == 'NotImplemented'` where `ValidationException` is required, and the handler at services/service-com-amazonaws-macie2-macie2/src/ops/delete_allow_list.rs:166 unconditionally returns `Scaffolded::not_implemented("DeleteAllowList")`.

Why: a client cannot delete an allow list at all, and cannot distinguish "refused because a live job still uses this list" from "operation missing". Code that relies on the documented safety check (delete with ignoreJobChecks=false, expect ValidationException, then retry with the override) sees an unmodeled 501/`NotImplemented` instead of a 400 `ValidationException`, so its error handling never triggers; cleanup paths that delete allow lists leave state behind, and a 501 is not in the operation's modeled error set so typed SDK clients surface it as an unknown server failure.

Fix: implement the handler in services/service-com-amazonaws-macie2-macie2/src/ops/delete_allow_list.rs (the only file for this operation): resolve `id` in the macie2 allow-list state, 404 `ResourceNotFoundException` when absent; when `ignoreJobChecks` is absent or "false", count classification jobs in state whose `allowListIds` contain this id and whose status is not COMPLETE/CANCELLED and, if non-zero, refuse with 400 `ValidationException` carrying the message template above with that count substituted, leaving state untouched; when "true", remove the allow list and answer 200 with an empty body so subsequent GetAllowList/UpdateAllowList on the id report 404 `ResourceNotFoundException` with the not-found message above. Remove the SCAFFOLDED marker once implemented.

Traces: macie2.delete-allow-list-in-use-rejected.

## Evidence
Failing check macie2.delete-allow-list-in-use-rejected (verdict: mismatch).

Expected (AWS / recorded):
  delete_allow_list(id=<allow-list-id>, ignoreJobChecks="false") with one IDLE classification job referencing the list ->
    HTTP 400, Error.Code = "ValidationException"
    Error.Message = "The allow list can't be deleted. 1 jobs are configured to use the list and those jobs have a status other than Complete or Cancelled."
      (fixed text except the leading job count, taken from state)
  get_allow_list(id=<allow-list-id>) after the refusal -> 200, id unchanged (list still present)
  delete_allow_list(id=<allow-list-id>, ignoreJobChecks="true") -> 200, empty body
  get_allow_list(id=<allow-list-id>) -> HTTP 404, Error.Code = "ResourceNotFoundException",
    Error.Message = "Input validation failed. The request specifies an allow list that does not exist."
  update_allow_list(id=<allow-list-id>, name=..., criteria=...) -> HTTP 404, same code and message

Actual (emulator):
  delete_allow_list(...) -> HTTP 501, Error.Code = "NotImplemented",
    Error.Message = "operation DeleteAllowList is scaffolded but not implemented in this emulator"
  Assertion: `assert error["Code"] == "ValidationException"` ->
    AssertionError: expected ValidationException, got 'NotImplemented'
  The remaining assertions (list still present, forced delete, post-delete 404 on GetAllowList and UpdateAllowList) were never reached.

Source: services/service-com-amazonaws-macie2-macie2/src/ops/delete_allow_list.rs:3-6 ("SCAFFOLDED, NOT IMPLEMENTED") and :166 `Err(crate::scaffolded::Scaffolded::not_implemented("DeleteAllowList"))`.
Model: smithy_query(operation="DeleteAllowList") -> DELETE /allow-lists/{id}, success 200, errors include ValidationException(400) and ResourceNotFoundException(404); `ignoreJobChecks` documents the non-terminal-job rejection rule.

## Steps to Reproduce
1. EnableMacie (or confirm GetMacieSession succeeds).
2. Create an S3 scratch bucket in the configured region.
3. macie2 CreateAllowList(name=<unique>, clientToken=<unique>, criteria={"regex": ".+"}) -> note the returned `id`.
4. macie2 CreateClassificationJob(jobType="SCHEDULED", scheduleFrequency={"dailySchedule":{}}, initialRun=False, s3JobDefinition={"bucketDefinitions":[{"accountId":<caller account>,"buckets":[<bucket>]}]}, allowListIds=[<id>]) and poll DescribeClassificationJob until jobStatus == "IDLE".
5. macie2 DeleteAllowList(id=<id>, ignoreJobChecks="false").
   Observed: HTTP 501 NotImplemented. Expected: HTTP 400 ValidationException with "The allow list can't be deleted. 1 jobs are configured to use the list and those jobs have a status other than Complete or Cancelled."
6. macie2 GetAllowList(id=<id>) -> must still return the list (200).
7. macie2 DeleteAllowList(id=<id>, ignoreJobChecks="true") -> must return 200 with an empty body.
8. macie2 GetAllowList(id=<id>) and UpdateAllowList(id=<id>, name=<same>, criteria=<same>) -> both must return HTTP 404 ResourceNotFoundException with "Input validation failed. The request specifies an allow list that does not exist."

Replay: run the behavioral suite check macie2.delete-allow-list-in-use-rejected.
