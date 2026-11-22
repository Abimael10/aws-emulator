Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__iam.crawler-schedule-no-schedule-and-unknown-crawler-rejected
Issue-Key: glue__iam.crawler-schedule-no-schedule-and-unknown-crawler-rejected|run#31|AssertionError|assert message == expected_missing, f"unexpected message for a missing crawler: {message!r}"
Issue-Commit: aa87d0058386f57e2d9444188ae656a8c51d6f38

# glue__iam: StartCrawlerSchedule/StopCrawlerSchedule on an unknown crawler return an emulator-authored EntityNotFoundException message instead of AWS's "Crawler with name <name> does not exist not found."

> _Note: this issue was identified against the code base at commit `aa87d0058386f57e2d9444188ae656a8c51d6f38`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartCrawlerSchedule

## Description
One net-new cause (Cause 1 of 1), reached through the glue__iam crawler-schedule refusal check. The HTTP status and the error code are already right; only the message text diverges.

Cause 1 of 1: StartCrawlerSchedule (and, in the same check, StopCrawlerSchedule) against a crawler name no crawler holds is refused with the wrong Error.Message. Failing check: glue__iam.crawler-schedule-no-schedule-and-unknown-crawler-rejected.
AWS: Smithy (com.amazonaws.glue#AWSGlue) models EntityNotFoundException as a client error, HTTP 400, on both StartCrawlerSchedule and StopCrawlerSchedule (StartCrawlerSchedule errors: EntityNotFoundException, NoScheduleException, OperationTimeoutException, SchedulerRunningException, SchedulerTransitioningException; StopCrawlerSchedule errors: EntityNotFoundException, OperationTimeoutException, SchedulerNotRunningException, SchedulerTransitioningException). The model does not fix message text, so the wording is recorded behavior only: the recording shows HTTP 400, Error.Code=EntityNotFoundException, Error.Message `Crawler with name <CrawlerName> does not exist not found.` — fixed text is `Crawler with name ` + ` does not exist not found.` (AWS's own doubled "does not exist not found." phrasing), and the one variable part is the CrawlerName echoed verbatim from the request. The recording shows the identical message for both operations.
Emulator: answers HTTP 400 / EntityNotFoundException but with `Crawler entry with name <CrawlerName> does not exist` — it inserts the word "entry" and drops the trailing ` not found.`. Replay: `AssertionError: unexpected message for a missing crawler: 'Crawler entry with name sim2real-behav-crawler-missing-147c8267d764 does not exist'` (that name is a per-run generated value, not part of the requirement). Everything the check asserts before this point passes: NoScheduleException with the exact text "No schedule found." for a crawler created without a cron, and the SCHEDULED/NOT_SCHEDULED toggle through Stop/StartCrawlerSchedule on a crawler created with `cron(0 1 * * ? *)`.
Why: a client that branches on, logs or asserts Glue's not-found text (CLI output scrapers, retry/branch logic keyed on the message, golden-output tests) sees a sentence AWS never emits; the error class is right, the human- and machine-readable detail is not AWS's.
Fix: the message rendered for EntityNotFoundException by the StartCrawlerSchedule and StopCrawlerSchedule handlers — i.e. the crawler-not-found message helper those handlers call in the Glue crate's state/messages module. The helper is shared with other crawler reads, so the fix must preserve each operation's own recorded wording rather than globally rewording it.
Related but NOT covering this check: open issue 20261119T084143Z-glue-refusal-messages-diverge-startcrawlerschedule-s-entitynotfoundexception-and.md describes the same StartCrawlerSchedule wording defect but is grounded in glue.crawler-schedule-lifecycle-toggles-state / glue.test-connection-unknown-name-rejected; the replay did not attribute this check to it, and this check additionally exercises the StopCrawlerSchedule branch of the same message. Open issue 20261117T135333Z-glue-iam-glue-crawler-schedule-refusal-checks-never-complete-the.md is about these calls not answering within the budget (a hang) — here they do answer, with the wrong text.
Traces: glue__iam.crawler-schedule-no-schedule-and-unknown-crawler-rejected; handlers services/service-com-amazonaws-glue-awsglue/src/ops/start_crawler_schedule.rs and .../stop_crawler_schedule.rs, message helper services/service-com-amazonaws-glue-awsglue/src/state/messages.rs.

## Evidence
StartCrawlerSchedule(CrawlerName=<a name no crawler has>):
  Expected (AWS): HTTP 400, Error.Code=EntityNotFoundException, Error.Message="Crawler with name <CrawlerName> does not exist not found."
  Actual (emulator): HTTP 400, Error.Code=EntityNotFoundException, Error.Message="Crawler entry with name <CrawlerName> does not exist"

StopCrawlerSchedule(CrawlerName=<the same unknown name>): same expectation; the check asserts the identical message and never reaches it because the Start assertion fails first.

Fixed text expected: "Crawler with name " ... " does not exist not found."; variable part: CrawlerName, echoed from the request.

Replay line:
  [mismatch] glue__iam.crawler-schedule-no-schedule-and-unknown-crawler-rejected
  check_glue__iam_crawler_schedule_no_schedule_and_unknown_crawler_rejected.py line 157:
  AssertionError: unexpected message for a missing crawler: 'Crawler entry with name sim2real-behav-crawler-missing-147c8267d764 does not exist'

Passing controls in the same check (so the divergence is isolated to this message): NoScheduleException / "No schedule found." on a crawler with no cron; Schedule.State toggling SCHEDULED -> NOT_SCHEDULED -> SCHEDULED through StopCrawlerSchedule/StartCrawlerSchedule.

## Steps to Reproduce
1. iam.create_role trusted by glue.amazonaws.com with arn:aws:iam::aws:policy/service-role/AWSGlueServiceRole attached; glue.create_database.
2. glue.create_crawler(Name=<A>, Role=<role arn>, DatabaseName=<db>, Targets={'S3Targets':[{'Path':'s3://example-bucket/data/'}]}) with no Schedule, and glue.create_crawler(Name=<B>, ... , Schedule='cron(0 1 * * ? *)').
3. glue.start_crawler_schedule(CrawlerName=<A>) -> NoScheduleException 'No schedule found.' (passes).
4. glue.stop_crawler_schedule / glue.start_crawler_schedule on <B> -> Schedule.State toggles NOT_SCHEDULED/SCHEDULED (passes).
5. glue.start_crawler_schedule(CrawlerName=<a name nobody created>).
   Observed: EntityNotFoundException, message 'Crawler entry with name <name> does not exist'.
   Expected: EntityNotFoundException, message 'Crawler with name <name> does not exist not found.'
6. glue.stop_crawler_schedule(CrawlerName=<the same unknown name>) — same expected message.
