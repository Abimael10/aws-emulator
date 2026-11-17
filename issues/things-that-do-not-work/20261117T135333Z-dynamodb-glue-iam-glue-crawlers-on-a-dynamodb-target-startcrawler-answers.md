Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__glue__iam.dynamodb-crawler-infers-column-schema dynamodb__glue__iam.glue-crawler-on-dynamodb-target-populates-catalog-table dynamodb__glue__iam.dynamodb-crawler-missing-scan-permission-rejected dynamodb__glue__iam.glue-crawler-missing-table-scan-permission-fails-crawl
Issue-Key: glue|StartCrawler|NotImplemented
Issue-Key: dynamodb__glue__iam.dynamodb-crawler-missing-scan-permission-rejected|run#25|AssertionError|assert code == "InvalidInputException", (
Issue-Key: dynamodb__glue__iam.glue-crawler-missing-table-scan-permission-fails-crawl|_create_crawler#20|AssertionError|raise AssertionError("CreateCrawler unexpectedly SUCCEEDED for a role with no table permission")
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# Glue crawlers on a DynamoDB target: StartCrawler answers NotImplemented, and CreateCrawler accepts a role with no table permission

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartCrawler

## Description
Cause 1 of 2: StartCrawler is unimplemented for any crawler whose Targets name no Amazon S3 path (e.g. a DynamoDB target).
AWS: Smithy (glue StartCrawler, errors EntityNotFoundException, CrawlerRunningException, OperationTimeoutException) — StartCrawler accepts a crawler with DynamoDBTargets exactly as it does an S3 target, returns 200, and the crawl populates the Data Catalog table with the inferred column schema (GetTable afterwards shows the columns).
Emulator: the call is refused with a scaffold stub: `ClientError: An error occurred (NotImplemented) when calling the StartCrawler operation: operation StartCrawler for a crawler whose Targets name no Amazon S3 path is scaffolded but not implemented in this emulator`. NotImplemented is not a modeled error for StartCrawler, so no client can handle it. (This failure is NOT marked ALREADY FILED by the suite — the stub is conditional on the target kind, so no issue names it.)
Why: every DynamoDB-sourced catalog ingestion flow is impossible, and the client receives an unmodeled error code instead of either a successful crawl or a modeled refusal.
Fix: the StartCrawler handler's non-S3 target path (DynamoDBTargets): run the crawl and write the inferred table/columns into the Data Catalog.
Traces: dynamodb__glue__iam.dynamodb-crawler-infers-column-schema, dynamodb__glue__iam.glue-crawler-on-dynamodb-target-populates-catalog-table.

Cause 2 of 2: CreateCrawler accepts a Role that cannot read the crawl target.
AWS: Smithy glue CreateCrawler declares InvalidInputException (400); AWS validates the passed Role at create time and refuses a role whose policy does not allow reading the target (no `dynamodb:DescribeTable`/`dynamodb:Scan` for a DynamoDB target, no table permission for a catalog target), so nothing is created.
Emulator: the create succeeds. One check reports `AssertionError: expected CreateCrawler to refuse a role with no dynamodb:DescribeTable, got code None: None` (no error at all), the other `AssertionError: CreateCrawler unexpectedly SUCCEEDED for a role with no table permission`.
Why: a client's broken crawler is accepted and then fails (or silently does nothing) at crawl time instead of being refused with InvalidInputException naming the missing permission.
Fix: CreateCrawler's role validation against the declared Targets.
Traces: dynamodb__glue__iam.dynamodb-crawler-missing-scan-permission-rejected, dynamodb__glue__iam.glue-crawler-missing-table-scan-permission-fails-crawl.

## Evidence
Cause 1: `botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the StartCrawler operation: operation StartCrawler for a crawler whose Targets name no Amazon S3 path is scaffolded but not implemented in this emulator` (expected: 200 and a crawl that populates the catalog table).
Cause 2: `AssertionError: expected CreateCrawler to refuse a role with no dynamodb:DescribeTable, got code None: None` and `AssertionError: CreateCrawler unexpectedly SUCCEEDED for a role with no table permission` (expected InvalidInputException).

## Steps to Reproduce
Cause 1: create a DynamoDB table, a Glue database, a role Glue can assume with dynamodb read permission, and CreateCrawler with DynamoDBTargets naming that table; call StartCrawler. Observed NotImplemented; expected 200 and, after the crawl, GetTable showing the inferred columns.
Cause 2: CreateCrawler with the same DynamoDB/catalog target but a Role whose policy omits dynamodb:DescribeTable/Scan. Observed success; expected InvalidInputException naming the missing permission and nothing created.
