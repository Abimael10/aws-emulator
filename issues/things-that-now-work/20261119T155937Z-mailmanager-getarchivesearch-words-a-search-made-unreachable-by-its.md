Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: mailmanager.archive-deletion-rejects-further-reads-of-its-completed-search
Issue-Shape-Id: com.amazonaws.mailmanager#MailManagerSvc
Issue-Key: mailmanager.archive-deletion-rejects-further-reads-of-its-completed-search|run#13|AssertionError|assert message == "Search not found.", f"unexpected message {message!r}"
Issue-Commit: 9b24c61a0dda10beed762779735937c3a0c3ebd2

# GetArchiveSearch words a search made unreachable by its archive's deletion as "Invalid SearchId" instead of AWS's "Search not found."

> _Note: this issue was identified against the code base at commit `9b24c61a0dda10beed762779735937c3a0c3ebd2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetArchiveSearch

## Description
One root cause (Cause 1 of 1).

Cause 1 of 1: GetArchiveSearch's refusal message for an unresolvable SearchId is emulator-authored wording, not AWS's.

AWS: Smithy (services/service-com-amazonaws-mailmanager-mailmanagersvc/smithy-model/com-amazonaws-mailmanager-mailmanagersvc.json) models GetArchiveSearch with input SearchId (@required, @length 1..64) and errors AccessDeniedException (403), ThrottlingException (429) and ValidationException (400, "The request validation has failed. For details, see the accompanying error message"); the model does not state the message text, so the message is recorded behavior only. The recording shows that after the archive a completed search ran against is deleted, a later GetArchiveSearch on that SearchId is refused with HTTP 400, Error.Code `ValidationException` and Error.Message `Search not found.` — the text is entirely fixed: it has no variable part, echoes no SearchId and no ArchiveId back. (The same recording goes on to require ListArchiveSearches on the deleted archive to answer ValidationException with `Archive is not active.`, which the emulator already words that way; that assertion was never reached because the GetArchiveSearch message assertion failed first, so a fix must keep it passing.)

Emulator: the status (400) and the error code (ValidationException) are right, but the message is `Invalid SearchId`. The handler resolves the job and filters it on check_archive_for_job, then returns ValidationException carrying crate::state::SEARCH_ID_INVALID, a constant whose value is the literal "Invalid SearchId" (deliberately modelled on the import-job constant IMPORT_JOB_ID_INVALID). So every unresolvable SearchId — one never minted and one whose archive is gone or no longer ACTIVE — is answered with that text rather than AWS's.

Why: a client that branches on the refusal text (log scraping, test assertions, retry/cleanup logic that distinguishes "this search is gone" from "this id is malformed") sees a message AWS never sends. The condition AWS reports is a resource that is NOT THERE ("Search not found."); the emulator reports it as a malformed member value, which misleads any caller or operator reading the message, and any assertion on the AWS wording fails against the emulator.

Fix: the refusal wording for an unresolvable SearchId — the message constant this service keeps for GetArchiveSearch and the handler that returns it (the operation's handler/validation layer, not its serializer). Keep the HTTP 400 and the ValidationException code; change only the fixed text so it is AWS's "Search not found." for every unresolvable SearchId, including one whose archive is PENDING_DELETION or already purged. Note the SearchId is not echoed: the text carries no value from the request or state.

Traces: mailmanager.archive-deletion-rejects-further-reads-of-its-completed-search. Code sites: services/service-com-amazonaws-mailmanager-mailmanagersvc/src/ops/get_archive_search.rs lines 167-176 (the ValidationException return) and services/service-com-amazonaws-mailmanager-mailmanagersvc/src/state.rs lines 484-493 (the SEARCH_ID_INVALID constant and its rationale).

Not covered by any open issue: the open mailmanager issues concern CreateArchive's ArchiveId pattern (20261030T011318Z), the ListAddressListImportJobs stub (20261113T190134Z), the CreateRuleSet stub (20261117T123331Z) and CreateArchive's KMS-key acceptance (20261117T122437Z). The fixed issue 20261029T234859Z-mailmanager-archive-search-and-export-reads-refused-with made these reads serve 200 while the archive is alive — which this check's positive control confirms still works; this is a separate wording defect on the legitimate refusal after deletion, not a reopening of it.

## Evidence
Check mailmanager.archive-deletion-rejects-further-reads-of-its-completed-search:
  1. CreateArchive -> ArchiveId a-...
  2. StartArchiveSearch(ArchiveId, FromTimestamp=now-1d, ToTimestamp=now, MaxResults=10) -> SearchId
  3. poll GetArchiveSearch until Status.State == COMPLETED (passes), GetArchiveSearch echoes ArchiveId (passes)
  4. DeleteArchive(ArchiveId)
  5. GetArchiveSearch(SearchId=<that search>)

Expected (recorded AWS): HTTP 400, Error.Code = "ValidationException", Error.Message = "Search not found." (fixed text, no variable part).
Actual (emulator): HTTP 400, Error.Code = "ValidationException", Error.Message = "Invalid SearchId".

Replay assertion:
  File "check_mailmanager_archive_deletion_rejects_further_reads_of_its_completed_search.py", line 65, in run
    assert message == "Search not found.", f"unexpected message {message!r}"
  AssertionError: unexpected message 'Invalid SearchId'

The code assertion on the preceding line (code == "ValidationException") passed, so only the message diverges.

## Steps to Reproduce
1. mailmanager CreateArchive(ArchiveName=<unique>) -> ArchiveId.
2. StartArchiveSearch(ArchiveId=<that id>, FromTimestamp=<now - 1 day>, ToTimestamp=<now>, MaxResults=10) -> SearchId.
3. Poll GetArchiveSearch(SearchId=<that id>) until Status.State is COMPLETED.
4. DeleteArchive(ArchiveId=<that id>).
5. GetArchiveSearch(SearchId=<that id>) and read Error.Code and Error.Message: the code is ValidationException with status 400 (correct) but the message is "Invalid SearchId" where AWS sends "Search not found.".
6. Then ListArchiveSearches(ArchiveId=<that id>) must still answer ValidationException / "Archive is not active." — assert it remains unchanged by the fix.
