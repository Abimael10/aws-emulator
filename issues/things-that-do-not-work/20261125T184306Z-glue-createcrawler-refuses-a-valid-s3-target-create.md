Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue.crawler-create-start-poll-ready glue.crawler-schedule-state-machine glue.create-crawler-no-targets-fails glue.reject-get-nonexistent-crawler glue.start-crawler-already-running-fails
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue.crawler-create-start-poll-ready|_create_crawler_with_retry#7|Timeout|time.sleep(interval_s)
Issue-Key: glue.crawler-schedule-state-machine|_retry_iam_propagation#7|Timeout|time.sleep(interval_s)
Issue-Key: glue.create-crawler-no-targets-fails|_create_crawler_when_role_ready#7|Timeout|time.sleep(interval_s)
Issue-Key: glue.reject-get-nonexistent-crawler|_create_crawler_with_role_propagation_retry#7|Timeout|time.sleep(5)
Issue-Key: glue.start-crawler-already-running-fails|_retry_call#7|Timeout|time.sleep(interval_s)
Issue-Commit: c8c1a6886de773856b080208b347842cb2a65b75

# glue: CreateCrawler refuses a valid S3-target create (InvalidInputException), so the whole crawler family of checks never reaches a verdict

> _Note: this issue was identified against the code base at commit `c8c1a6886de773856b080208b347842cb2a65b75`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCrawler

## Description
Cause 1 of 1: a CreateCrawler whose Targets name an ordinary S3 path is refused with InvalidInputException instead of being accepted, and every check in the Glue crawler family is cut off at the harness cap while retrying it.

AWS: Smithy (glue CreateCrawler) requires Name, Role and Targets and models AlreadyExistsException / InvalidInputException / OperationTimeoutException / ResourceNumberLimitExceededException, all HTTP 400; the model states NO precondition that the bucket behind an S3Target$Path must be one the caller's account holds — S3Target$Path is documented as "the path to the Amazon S3 target", i.e. data another service owns that a LATER crawl walks. Recorded behavior: CreateCrawler with Targets={"S3Targets":[{"Path":"s3://example-bucket/data/"}]}, a role whose trust policy admits glue.amazonaws.com, and an existing DatabaseName is ACCEPTED (200), and the following GetCrawler reads the crawler back with State READY. The suite's own checks state the rule they rely on explicitly: the bucket-existence probe is a HeadBucket done as the caller, and a bucket that exists outside the account answers 403 (not 404), which clears the probe — which is why "example-bucket" is the placeholder used across these checks.

Emulator: all five checks fail with `Error: Check timed out after 60.0s` and never reach their assertions. Each one creates the crawler through a bounded retry helper that retries ONLY while CreateCrawler answers InvalidInputException (`glue.create_crawler(...)` in `_create_crawler_with_retry` / `_create_crawler_when_role_ready` / `_create_crawler_with_role_propagation_retry`, deadline 60s), so the replay evidence is that CreateCrawler answered InvalidInputException continuously for a request real AWS accepts — the helper then burns the whole budget and the check is cut off before its graded call. Hypothesis (narrowing which of the handler's two pre-checks refuses): the S3-target probe. CreateCrawler resolves each distinct target bucket with an in-process HeadBucket and refuses the create with InvalidInputException whenever that store answers 404 NoSuchBucket; in this emulator nothing has created "example-bucket", so a bucket that on AWS answers 403 to a stranger answers 404 here and every such create is refused. The message that refusal carries is `S3 target <Path> does not exist` plus the appended diagnostic clause naming Amazon S3 / 404 / NoSuchBucket — its fixed text is "S3 target"+" does not exist" and the diagnostic clause, its variable parts are the request's target Path and the per-call request id. The other candidate, refused with the same code, is the role pre-check answering `Service is unable to assume provided role. Please verify role's TrustPolicy` for a role these checks created with a glue.amazonaws.com trust statement.

Why: a client cannot register a crawler at all over an S3 location the emulator's own object store does not hold — the normal case, since a crawler is routinely created before (or independently of) the data it crawls, and over buckets owned by other accounts. Instead of a crawler in READY it gets a permanent 400 InvalidInputException, so no crawl, schedule, metrics or crawl-history behaviour downstream of a crawler can be exercised, and a retrying client hangs until its own deadline.

Fix: CreateCrawler's input pre-checks (the handler's S3-target bucket resolution and its role-assumability probe) — a target bucket the in-process object store merely does not hold must not refuse the create: only an answer that genuinely establishes the target names no data anywhere may, and a bucket that is present-but-unreadable, or an unanswered probe, must let the create through, which is the behaviour the recordings show for an out-of-account bucket. Equally, a role whose trust policy admits glue.amazonaws.com must resolve as assumable.

Traces: glue.crawler-create-start-poll-ready, glue.crawler-schedule-state-machine, glue.create-crawler-no-targets-fails, glue.reject-get-nonexistent-crawler, glue.start-crawler-already-running-fails. Note: the open issue 20261117T135333Z-glue-iam-glue-crawler-schedule-refusal-checks-never-complete-the records the same 60s-timeout symptom for glue__iam.crawler-schedule-no-schedule-and-unknown-crawler-rejected and hypothesises a blocking schedule handler; it carries none of the five check keys above, and the evidence here points at the CreateCrawler refusal that precedes every one of those scenarios.

## Evidence
Expected (recorded AWS): CreateCrawler(Name=<name>, Role=<arn of a role trusting glue.amazonaws.com>, DatabaseName=<existing db>, Targets={"S3Targets":[{"Path":"s3://example-bucket/data/"}]}) -> HTTP 200, and GetCrawler(Name=<name>) -> Crawler.State == "READY".
Actual (emulator): the create is answered with HTTP 400 InvalidInputException for every attempt over a 60s window — the checks' retry loops retry on exactly and only that code — so the replay reports `Error: Check timed out after 60.0s` for all five crawler checks and none reaches its graded assertion (the EntityNotFoundException "Crawler entry with name <name> does not exist" refusal, the "Targets are not defined" refusal, the START/READY state machine, or the CrawlerRunningException on a second StartCrawler).
The refusal text the handler can produce for this input: "S3 target s3://example-bucket/data/ does not exist" + the Amazon S3 / 404 / NoSuchBucket diagnostic clause (fixed text + request-supplied Path + per-call request id).

## Steps to Reproduce
1. CreateRole with a trust policy allowing sts:AssumeRole to Service glue.amazonaws.com; attach arn:aws:iam::aws:policy/service-role/AWSGlueServiceRole.
2. glue.CreateDatabase(DatabaseInput={Name: <db>}).
3. glue.CreateCrawler(Name=<crawler>, Role=<role arn>, DatabaseName=<db>, Targets={"S3Targets": [{"Path": "s3://example-bucket/data/"}]}) — note no bucket named example-bucket was created in the emulator.
Observed: HTTP 400 InvalidInputException on every attempt; a client that retries the code (as these checks do) never gets a crawler and times out.
Expected: HTTP 200 and GetCrawler(Name=<crawler>) reporting State READY, after which the family's graded calls (GetCrawler on an unknown name, CreateCrawler with Targets={}, StartCrawler twice, UpdateCrawlerSchedule/StartCrawlerSchedule/StopCrawlerSchedule, GetCrawlerMetrics, ListCrawls) can be exercised.
