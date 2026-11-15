Issue-Source: trace-sweep-divergence
Issue-Trace: s3-bucket-metadata-and-metadata-table-configuration-management
Issue-Step: teardown#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: s3-bucket-metadata-and-metadata-table-configuration-management s3-metadata-config-with-inventory-and-annotation
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3tables|DeleteTable|2xx->4xx|status
Issue-Key: s3tables|DeleteNamespace|2xx->4xx|status
Issue-Commit: a84a0a47137d3eec61135ddc1c28c473b0a18687

# s3tables deletes of the metadata table and its namespace are refused 400 instead of answering 204

> _Note: this issue was identified against the code base at commit `a84a0a47137d3eec61135ddc1c28c473b0a18687`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteTable / DeleteNamespace

## Description
2 net-new root causes in the s3tables wire surface, from the two divergence clusters the replay report marks neither stubbed nor OWNED. Both are deletes, during fixture teardown, of the s3tables resources that back an S3 bucket metadata-table configuration: the emulator refuses the delete with HTTP 400 where AWS deletes the resource and answers 204.

Cause 1 of 2: DeleteTable on an existing table answers 400 instead of deleting it and answering 204.
AWS: Smithy (com.amazonaws.s3tables) binds DeleteTable to DELETE /tables/{tableBucketARN}/{namespace}/{name} with http code 204, output Unit (no body), and marks it @idempotent. tableBucketARN is @required with pattern ^(arn:aws[-a-z0-9]*:[a-z0-9]+:[-a-z0-9]*:[0-9]{12}:bucket/[a-z0-9_-]{3,63})$; namespace and name are @required, @length min 1 max 255, pattern ^[0-9a-z_]*$; versionToken is an optional httpQuery param with @length 1..2048. Its only modeled failures are BadRequestException (400), ConflictException (409), ForbiddenException (403), NotFoundException (404), TooManyRequestsException (429) and InternalServerErrorException (500) - none of which describes deleting a table that exists and whose identifiers satisfy those constraints. Recorded behavior: the teardown DeleteTable of the metadata table created earlier in the same fixture answers 204 with an empty body.
Emulator: the same request answers 400. Earlier steps of the fixture (the ones that create the bucket, its metadata table configuration and the table) pass, so the table exists at the point of the delete; the emulator still takes a 400 refusal branch rather than the 204 success path. Hypothesis: the delete path re-validates or fails to resolve a table that was materialised through the S3 bucket-metadata-table-configuration path rather than through a direct s3tables CreateTable, and falls into the service's generic 400 BadRequestException branch (a genuinely absent table would be the modeled 404 NotFoundException, not 400).
Why: a client cannot delete the table it created. Teardown never completes, so the table bucket, its namespace and the table leak; a later CreateTable or metadata-configuration create for the same name then conflicts against a resource the client believes it removed, and an @idempotent delete that answers 400 breaks every retry-until-gone cleanup loop (CDK/CloudFormation stack deletion, test fixtures, scripted teardown).
Fix: the s3tables DeleteTable handler and the table state it resolves against - a table created via the S3 bucket metadata-table-configuration path must be deletable by its (tableBucketARN, namespace, name) triple and must answer 204 with an empty body; refusals must be limited to the modeled cases (404 when the table does not exist, 409 on a versionToken precondition failure).
Traces: s3-bucket-metadata-and-metadata-table-configuration-management#teardown#6 (representative), s3-metadata-config-with-inventory-and-annotation#teardown#7, s3-metadata-config-with-inventory-and-annotation#teardown#8.

Cause 2 of 2: DeleteNamespace on an existing, emptied namespace answers 400 instead of deleting it and answering 204.
AWS: Smithy binds DeleteNamespace to DELETE /namespaces/{tableBucketARN}/{namespace} with http code 204, output Unit (no body), @idempotent; tableBucketARN and namespace carry the same @required/@length/@pattern constraints as above, and the same six modeled errors apply. Recorded behavior: the teardown DeleteNamespace, issued after the namespace's table has been deleted, answers 204 with an empty body.
Emulator: the same request answers 400.
Why: the namespace that backs the bucket's metadata table configuration is undeletable, so the table bucket cannot be emptied and then deleted; the client is left with an orphaned namespace it has no API to remove and re-running the same provisioning flow collides with it.
Fix: the s3tables DeleteNamespace handler and the namespace state behind it - an existing namespace with no remaining tables must delete and answer 204 with an empty body, with refusals restricted to the modeled cases (404 when absent, 409 when the namespace still holds tables).
Traces: s3-bucket-metadata-and-metadata-table-configuration-management#teardown#7.

Related OPEN issues, neither of which carries either of these two fixture/step keys and neither of which covers the metadata-table teardown path: 20261101T233620Z-s3tables-refusals-diverge-enum-pattern-violations-answer-validationexception.md (reports a NON-EMPTY DeleteNamespace answering 409, i.e. the opposite situation to cause 2) and 20261102T055316Z-s3tables-constraint-violation-refusals-are-now-blanket-400.md (reports constraint-violation refusals collapsing to a blanket 400 BadRequestException with the framework's generic validator text, which is the likely mechanism behind both 400s here but is scoped to requests that genuinely violate a constraint, not to valid deletes of existing resources).

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false, score 94.5%). Two mismatch clusters, both with stub_path empty and owned_by empty:

Cluster 1 (affected_count=3):
  service=s3tables | DeleteTable | status=204->400 | keys=status
  fixture_id: s3-bucket-metadata-and-metadata-table-configuration-management, step_key: teardown#6
  detail: status: expected 204, got 400
  affected: s3-bucket-metadata-and-metadata-table-configuration-management#teardown#6, s3-metadata-config-with-inventory-and-annotation#teardown#7, s3-metadata-config-with-inventory-and-annotation#teardown#8

Cluster 2 (affected_count=1):
  service=s3tables | DeleteNamespace | status=204->400 | keys=status
  fixture_id: s3-bucket-metadata-and-metadata-table-configuration-management, step_key: teardown#7
  detail: status: expected 204, got 400

Expected (AWS, recorded; and the modeled http code 204 with output Unit for both operations): HTTP 204 No Content, empty body, the table / namespace gone.
Actual (emulator): HTTP 400 for both, and the resource is not deleted. The fixture's preceding create steps pass, so the table and namespace exist when the delete is issued. Table-bucket ARNs, namespace and table names in the fixtures are per-run values and are not part of the requirement; the requirement is that a valid delete of an existing table / emptied namespace answers 204 with an empty body and that any refusal be one of the operation's modeled errors (404 absent, 409 precondition/non-empty).

## Steps to Reproduce
1. Build and boot the emulator from this checkout (s3tables is bootstrapped and serving).
2. CreateTableBucket, then set up an S3 general purpose bucket metadata table configuration whose S3 Tables destination points at that table bucket (as the fixture's task phase does) so that the metadata namespace and table exist.
3. Call s3tables DeleteTable with the table bucket ARN, the namespace and the table name (no versionToken). Expect HTTP 204 with an empty body and the table gone; observe HTTP 400.
4. Call s3tables DeleteNamespace with the table bucket ARN and that now-empty namespace. Expect HTTP 204 with an empty body; observe HTTP 400.
5. Equivalently: re-replay s3-bucket-metadata-and-metadata-table-configuration-management steps teardown#6 and teardown#7, and s3-metadata-config-with-inventory-and-annotation steps teardown#7 and teardown#8.
