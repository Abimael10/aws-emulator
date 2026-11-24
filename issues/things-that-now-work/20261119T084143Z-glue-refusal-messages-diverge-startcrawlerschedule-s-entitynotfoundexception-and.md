Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue.crawler-schedule-lifecycle-toggles-state glue.test-connection-unknown-name-rejected
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue.crawler-schedule-lifecycle-toggles-state|run#26|AssertionError|assert message == expected_missing, f"expected {expected_missing!r}, got {message!r}"
Issue-Key: glue.test-connection-unknown-name-rejected|run#12|AssertionError|assert message == "This operation is only supported for the Unified Connections", (
Issue-Commit: 9883e736c3a61c061667f65bdd8b0063b3d57885

# Glue refusal messages diverge: StartCrawlerSchedule's EntityNotFoundException and TestConnection's ValidationException are worded differently from AWS

> _Note: this issue was identified against the code base at commit `9883e736c3a61c061667f65bdd8b0063b3d57885`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartCrawlerSchedule

## Description
Two net-new causes, both in the Glue control plane, both message-text divergences on a refusal whose HTTP status and error code the emulator already gets right. Neither is covered by an open issue: the nearest one, 20261117T135333Z-glue-iam-glue-crawler-schedule-refusal-checks-never-complete-the.md, is about crawler-schedule calls not answering within the budget (a hang) in the glue__iam check, not about the wording of a refusal that IS answered here.

Cause 1 of 2: StartCrawlerSchedule on a crawler name that does not exist returns EntityNotFoundException with the wrong message.
AWS: Smithy (com.amazonaws.glue#StartCrawlerSchedule) models EntityNotFoundException (HTTP 400, "A specified entity does not exist") for a crawler the account does not hold; the model does not fix the message text, so this is recorded behavior only. The recording shows HTTP 400, code EntityNotFoundException, message `Crawler with name <CrawlerName> does not exist not found.` — fixed text `Crawler with name ` + ` does not exist not found.` (AWS's own doubled "does not exist not found." phrasing), with the single variable part being the CrawlerName taken verbatim from the request.
Emulator: answers HTTP 400 / EntityNotFoundException but with `Crawler entry with name <CrawlerName> does not exist` — it inserts the word "entry" and drops the trailing " not found.". Replay: `AssertionError: expected 'Crawler with name sim2real-behav-glue-crmissing-<id> does not exist not found.', got 'Crawler entry with name sim2real-behav-glue-crmissing-<id> does not exist'` (the name itself is a per-run generated value and is not part of the requirement).
Why: callers and SDK/CLI users that match or surface Glue's not-found text (log scrapers, retry/branch logic keyed on the message, golden-output tests) see a message AWS never emits; the error class is right but the human- and machine-readable detail is not AWS's.
Fix: the message produced for StartCrawlerSchedule's EntityNotFoundException — the shared crawler-not-found message helper the handler calls (`crate::state::messages::crawler_not_found`) renders the wrong template for this operation. Note the helper is shared with other crawler reads, so the fix must keep each operation's own recorded wording rather than globally rewording it.
Traces: glue.crawler-schedule-lifecycle-toggles-state. The rest of that check (SCHEDULED initial state, StopCrawlerSchedule/StartCrawlerSchedule toggling, cron expression preserved, NoScheduleException "No schedule found.") passes — only this message diverges. Handler: services/service-com-amazonaws-glue-awsglue/src/ops/start_crawler_schedule.rs lines 163-173; message helper: services/service-com-amazonaws-glue-awsglue/src/state/messages.rs.

Cause 2 of 2: TestConnection on a stored non-unified connection returns ValidationException with the wrong message.
AWS: Smithy (com.amazonaws.glue#TestConnection) lists no ValidationException for the operation, so the restriction is recorded behavior only: the recording shows that TestConnection against an existing connection whose type is not a Unified Connection (here a stored JDBC connection) is refused HTTP 400, code ValidationException, message exactly `This operation is only supported for the Unified Connections` — the whole string is fixed text with no variable part (it says "This operation", not the operation's name).
Emulator: answers HTTP 400 / ValidationException with `TestConnection is only supported for the Unified Connections` — it substitutes the operation name for AWS's fixed leading "This operation". Replay: `AssertionError: unexpected ValidationException message: 'TestConnection is only supported for the Unified Connections'`.
Why: a client that distinguishes this restriction from the other TestConnection refusals by its message (the check's own positive control does exactly this: it proves the refusal keys on connection kind, not on the name's shape) reads text AWS never returns, so message-based handling and recorded expectations break.
Fix: the TestConnection handler's validation message constant (`crate::state::messages::TEST_CONNECTION_UNIFIED_ONLY`), used for both the inline-input and stored-connection branches.
Traces: glue.test-connection-unknown-name-rejected. The EntityNotFoundException branch of that check (missing connection name) passes; only this ValidationException message diverges. Handler: services/service-com-amazonaws-glue-awsglue/src/ops/test_connection.rs lines 229-238 and 324-333; message constant: services/service-com-amazonaws-glue-awsglue/src/state/messages.rs.

## Evidence
Cause 1 — StartCrawlerSchedule(CrawlerName=<a name no crawler has>):
  Expected (AWS): HTTP 400, Error.Code=EntityNotFoundException, Error.Message="Crawler with name <CrawlerName> does not exist not found."
  Actual (emulator): HTTP 400, Error.Code=EntityNotFoundException, Error.Message="Crawler entry with name <CrawlerName> does not exist"
  Fixed text expected: "Crawler with name " ... " does not exist not found."; variable part: CrawlerName, echoed from the request.
  Replay line: AssertionError: expected 'Crawler with name sim2real-behav-glue-crmissing-cf2552c68932 does not exist not found.', got 'Crawler entry with name sim2real-behav-glue-crmissing-cf2552c68932 does not exist'

Cause 2 — TestConnection(ConnectionName=<an existing JDBC connection>):
  Expected (AWS): HTTP 400, Error.Code=ValidationException, Error.Message="This operation is only supported for the Unified Connections" (entirely fixed text)
  Actual (emulator): HTTP 400, Error.Code=ValidationException, Error.Message="TestConnection is only supported for the Unified Connections"
  Replay line: AssertionError: unexpected ValidationException message: 'TestConnection is only supported for the Unified Connections'

## Steps to Reproduce
Cause 1:
1. CreateCrawler with a Schedule of cron(15 12 * * ? *) and an S3 target; CreateCrawler without a Schedule.
2. StopCrawlerSchedule then StartCrawlerSchedule on the scheduled crawler (these pass: state toggles NOT_SCHEDULED/SCHEDULED, cron preserved).
3. StartCrawlerSchedule(CrawlerName=<name no crawler has>).
Observed: EntityNotFoundException with message 'Crawler entry with name <name> does not exist'.
Expected: EntityNotFoundException with message 'Crawler with name <name> does not exist not found.'

Cause 2:
1. CreateConnection with ConnectionType=JDBC and JDBC_CONNECTION_URL/USERNAME/PASSWORD properties.
2. TestConnection(ConnectionName=<a name nobody created>) -> EntityNotFoundException (passes).
3. TestConnection(ConnectionName=<the JDBC connection created in step 1>).
Observed: ValidationException with message 'TestConnection is only supported for the Unified Connections'.
Expected: ValidationException with message 'This operation is only supported for the Unified Connections'.
