Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: opensearch.index-settings-unicode-analyzer-rejected opensearch.overly-permissive-attach-data-source-cross-account-rejected opensearch.overly-permissive-attach-data-source-unsupported-type-rejected opensearch.start-migration-nonexistent-application-rejected
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: opensearch.index-settings-unicode-analyzer-rejected|_sustained_error_code#19|AssertionError|raise AssertionError(
Issue-Key: opensearch.overly-permissive-attach-data-source-cross-account-rejected|run#13|AssertionError|assert message == (
Issue-Key: opensearch.overly-permissive-attach-data-source-unsupported-type-rejected|run#11|AssertionError|assert negative_message == expected_message, (
Issue-Key: opensearch.start-migration-nonexistent-application-rejected|_error_detail#11|AssertionError|raise AssertionError(f"expected a ClientError, call succeeded with {result!r}")
Issue-Commit: 844d925c58d220632d295e47347c398f1151f4e4

# opensearch: 4 net-new causes — CreateIndex accepts a non-ASCII custom-analyzer name, two AttachDataSource ValidationException messages are emulator-worded, and StartMigration/ListMigrations do no existence checks

> _Note: this issue was identified against the code base at commit `844d925c58d220632d295e47347c398f1151f4e4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartMigration

## Description
Four net-new root causes on the OpenSearch (es) control plane, all grounded in behavioral checks that the suite did not mark as already filed or owned. None is covered by an open issue; the closest open/resolved neighbours are named per cause.

Cause 1 of 4: CreateIndex accepts an index schema whose custom-analyzer name is non-ASCII, instead of refusing it.
AWS: Smithy — CreateIndex (POST /2021-01-01/opensearch/domain/{DomainName}/index) models DependencyFailureException (httpError 424) among its errors; IndexSchema is an untyped `document`, so the analyzer-name rule is not in the model: recorded behavior only. The recording shows that a schema carrying `settings.index.analysis.analyzer.<non-ASCII name>` (e.g. a name made of CJK/Latin-diacritic/Cyrillic characters) is refused, sustained across retries, with HTTP 424 DependencyFailureException and the message "The domain is not available now, please try again later." — whose text is entirely fixed (no request- or state-derived parts) — while the byte-identical schema with an ASCII analyzer name on the same domain is accepted with Status CREATED, and the refused CreateIndex leaves nothing behind (GetIndex then answers ResourceNotFoundException "Resource index <IndexName> does not exist", the index name being the request's).
Emulator: the non-ASCII arm is ACCEPTED — HTTP 200 with body {"Status": "CREATED"} — so no refusal ever occurs and the index is created.
Why: a client that relies on AWS refusing a non-ASCII analyzer name (and on retrying the 424 before giving up) sees a success here; the index it believes AWS rejected exists in the emulator, so subsequent name reuse, cleanup and error-handling paths all diverge, and code that treats DependencyFailureException as retryable is never exercised.
Fix: the CreateIndex handler's IndexSchema validation (the analyzer-name admission check), which must refuse before any index state is written.
Traces: check opensearch.index-settings-unicode-analyzer-rejected.

Cause 2 of 4: AttachDataSource's cross-account refusal carries an emulator-authored message.
AWS: Smithy — AttachDataSource models ValidationException (httpError 400); dataSourceArn is an ARN with length 20..2048 and pattern ".*", so the account rule is not in the model: recorded behavior only. The recording shows AttachDataSource with a dataSourceArn whose account segment is not the caller's account is refused with ValidationException and the message "dataSourceArn account <account-from-the-ARN> does not match caller account <caller-account>" — fixed text "dataSourceArn account " and " does not match caller account ", with both values taken from the request (the 5th colon-segment of dataSourceArn) and from the caller's identity. No attachment record is created.
Emulator: the code is right (ValidationException) but the message is rewritten: "dataSourceArn <full-arn> names a resource of account <account>, which is not the Amazon Web Services account ID this request was made for." — different fixed text, and it interpolates the whole ARN where AWS interpolates only the account.
Why: a client or test that matches on AWS's wording (or parses the two account ids out of it) does not recognise this refusal; the divergence is invisible to code that only reads the error code, and breaks everything that reads the message.
Fix: the AttachDataSource handler's account-mismatch validation message (the ValidationException formatter), not the check itself.
Traces: check opensearch.overly-permissive-attach-data-source-cross-account-rejected. Related but not covering: 20261103T141919Z-opensearch-attachdatasource-is-not-idempotent-after-its-attachment.md and 20261114T172414Z-opensearch-reopens-opensearch-attachdatasource-refuses-an-opensearch-serverless-collection.md (different causes on the same operation).

Cause 3 of 4: AttachDataSource's dataSourceArn pattern refusal quotes the wrong regular expression (and the emulator's pattern is laxer than AWS's).
AWS: Smithy — the modeled dataSourceArn pattern is ".*", so the enforced shape is recorded behavior only. The recording shows an ARN whose service segment is not a pure-letter service (e.g. an EC2 instance ARN) refused with ValidationException and the message "1 validation error detected: Value '<dataSourceArn as sent>' at 'dataSourceArn' failed to satisfy constraint: Member must satisfy regular expression pattern: arn:aws[a-z\\-]*:[a-z]+:[a-z0-9\\-]*:[0-9]*:[a-z0-9\\-]+/[a-z0-9_\\-]+". The only variable part is the echoed value; the member name and the pattern are fixed AWS text — the resource segment admits letters, digits, underscore and hyphen, NOT a dot.
Emulator: same code and same sentence, but the pattern it reports is "...:[a-z0-9\\-]+/[a-z0-9_\\-\\.]+" — the resource character class carries an extra \\. — so the quoted requirement differs from AWS's and, by implication, the enforced regex admits dataSourceArn resource segments containing dots that AWS refuses.
Why: a client that reproduces the advertised constraint (or validates client-side against the pattern AWS reports) derives a wrong rule, and ARNs with a dot in the resource id are accepted here and rejected on AWS.
Fix: the dataSourceArn pattern constant used by the AttachDataSource handler's validation and by its ValidationException message builder (one source of truth for both).
Traces: check opensearch.overly-permissive-attach-data-source-unsupported-type-rejected.

Cause 4 of 4: the application-migration API performs no existence lookup — StartMigration mints a migration for an unknown data source and for an unknown applicationId, and ListMigrations answers an empty list for an unknown applicationId.
AWS: Smithy — StartMigration (POST /2021-01-01/opensearch/app-migrations) models ResourceNotFoundException (httpError 409) and ValidationException (400); ListMigrations (GET /2021-01-01/opensearch/app-migrations) models ValidationException but not ResourceNotFoundException, so its refusal is recorded behavior only. The recording shows: (a) for an ACTIVE application whose migrationOptions.source.datasourceArn is a well-formed AOSS collection ARN that names no existing collection, StartMigration is refused ResourceNotFoundException "Data source <datasourceArn> not found for application <applicationId>" — fixed text "Data source " / " not found for application ", both values from the request; (b) for an applicationId that does not exist, StartMigration is refused ResourceNotFoundException "Application <applicationId> does not exist"; (c) ListMigrations for that same unknown applicationId is refused ResourceNotFoundException "Application <applicationId> not found" rather than returning migrations: [].
Emulator: StartMigration answers HTTP 200 with {"migrationId": "<generated>", "status": "PENDING"} in both (a) and (b) — the data source is never resolved and the applicationId is never looked up — so a migration job is minted against resources that do not exist; (c) is consequently never refused either (the suite stops at the first accepted call).
Why: a caller gets a migration id for a job that can never run, and cannot distinguish "application/data source missing" from "migration started"; retry and reconciliation logic keyed on ResourceNotFoundException never fires, and bogus migration records accumulate in listings.
Fix: the StartMigration handler's pre-flight resolution (application lookup, then data-source-attachment lookup) and the same application lookup in the ListMigrations handler, both refusing before any migration state is created. Note the open stub issue 20261027T004336Z-opensearch-startmigration-stub.md is stale — the operation now answers 200 rather than a 501 stub — and the resolved 20261027T200529Z-opensearch-startmigration-accepts-a-non-collection-arn-as.md covered only the ARN SHAPE check, not existence of the collection or of the application; neither covers this cause.
Traces: check opensearch.start-migration-nonexistent-application-rejected.

## Evidence
Cause 1 — opensearch.index-settings-unicode-analyzer-rejected:
  AssertionError: real AWS refuses this call, but attempt 0 SUCCEEDED with {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'Status': 'CREATED'}
  expected: code DependencyFailureException (HTTP 424), message "The domain is not available now, please try again later.", sustained across retries; the ASCII-analyzer control on the same domain returned CREATED, so the index surface itself is usable.

Cause 2 — opensearch.overly-permissive-attach-data-source-cross-account-rejected:
  expected message: "dataSourceArn account <arn-account> does not match caller account <caller-account>"
  actual message:   "dataSourceArn arn:aws:es:us-west-2:111122223333:domain/sim2real-xacct-b10a3c59 names a resource of account 111122223333, which is not the Amazon Web Services account ID this request was made for."
  (code was ValidationException in both; the ARN, the two account ids and the domain name above are this run's values, not part of the requirement.)

Cause 3 — opensearch.overly-permissive-attach-data-source-unsupported-type-rejected:
  expected: "1 validation error detected: Value '<arn>' at 'dataSourceArn' failed to satisfy constraint: Member must satisfy regular expression pattern: arn:aws[a-z\\-]*:[a-z]+:[a-z0-9\\-]*:[0-9]*:[a-z0-9\\-]+/[a-z0-9_\\-]+"
  actual:   "... Member must satisfy regular expression pattern: arn:aws[a-z\\-]*:[a-z]+:[a-z0-9\\-]*:[0-9]*:[a-z0-9\\-]+/[a-z0-9_\\-\\.]+"
  (difference: the extra \\. in the trailing resource character class.)

Cause 4 — opensearch.start-migration-nonexistent-application-rejected:
  AssertionError: expected a ClientError, call succeeded with {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'migrationId': 'sfi9sv1baaaa-gy8c079qmaaa', 'status': 'PENDING'}
  expected at that point: ResourceNotFoundException "Data source <datasourceArn> not found for application <applicationId>"; then, for an unknown applicationId, ResourceNotFoundException "Application <applicationId> does not exist"; and ListMigrations(applicationId=<unknown>) -> ResourceNotFoundException "Application <applicationId> not found" instead of an empty list. The migrationId above is a per-run generated value.

## Steps to Reproduce
All four against the emulator's opensearch (es) endpoint with a SigV4-signed client.

Cause 1: CreateDomain a minimal single-node domain and wait for Active. CreateIndex(DomainName, IndexName=<a>, IndexSchema={settings.index.analysis.analyzer.{"s2r_custom_analyzer":{type:custom,tokenizer:standard}}, mappings.properties.title.analyzer="s2r_custom_analyzer"}) -> CREATED (control). Repeat with the SAME schema but the analyzer key replaced by a non-ASCII name -> emulator returns 200 CREATED; AWS refuses 424 DependencyFailureException "The domain is not available now, please try again later." and GetIndex on that index then answers ResourceNotFoundException.

Cause 2: CreateApplication, wait ACTIVE; CreateDomain, wait Active, take its ARN. AttachDataSource(id=<appId>, dataSourceArn=<the domain ARN with the account segment replaced by another account>) -> observe the ValidationException message; compare with "dataSourceArn account <that account> does not match caller account <caller account>".

Cause 3: CreateApplication, wait ACTIVE. AttachDataSource(id=<appId>, dataSourceArn="arn:aws:ec2:<region>:<account>:instance/i-0123456789abcdef0") -> ValidationException; compare the quoted regular expression with AWS's (no dot in the resource class).

Cause 4: CreateApplication, wait ACTIVE. StartMigration(applicationId=<appId>, migrationOptions={source:{datasourceArn:"arn:aws:aoss:<region>:<account>:collection/<id-that-does-not-exist>"}, workspace:{createWorkspace:true,name:<n>,type:SEARCH}}) -> emulator 200 PENDING; AWS refuses ResourceNotFoundException. Then StartMigration with the same options and an applicationId that was never created -> emulator 200 PENDING; AWS refuses ResourceNotFoundException "Application <id> does not exist". Then ListMigrations(applicationId=<that unknown id>) -> emulator returns an empty list; AWS refuses ResourceNotFoundException "Application <id> not found".
