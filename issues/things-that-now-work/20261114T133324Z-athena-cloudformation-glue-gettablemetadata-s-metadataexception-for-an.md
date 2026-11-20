Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena__cloudformation__glue.glue-get-table-for-absent-entity-rejected
Issue-Key: athena__cloudformation__glue.glue-get-table-for-absent-entity-rejected|run#18|AssertionError|assert (code, message) == ("MetadataException", expected_message), (
Issue-Commit: f408e2feb866bce0b1ce46e353689f133faa9408

# athena__cloudformation__glue: GetTableMetadata's MetadataException for an absent table carries the bare 'Entity Not Found' text, not the wrapped AmazonDataCatalog EntityNotFoundException envelope

> _Note: this issue was identified against the code base at commit `f408e2feb866bce0b1ce46e353689f133faa9408`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTableMetadata

## Description
1 cause.

Cause 1 of 1: Athena GetTableMetadata's not-found refusal message omits the wrapped catalog-exception envelope.
Failing check ids: athena__cloudformation__glue.glue-get-table-for-absent-entity-rejected.

AWS: Smithy (services/service-com-amazonaws-athena-amazonathena): GetTableMetadata input CatalogName (required, length 1-256), DatabaseName (required, length 1-128), TableName (required, length 1-128), WorkGroup (optional); modeled errors InternalServerException (server, HTTP 500), InvalidRequestException (client, HTTP 400), MetadataException (client, HTTP 400). The model does not state the message text, so for the text this is recorded behavior only: when the named table does not exist in an existing database, Athena fails the call with HTTP 400 and error code MetadataException whose Message is the Glue/AmazonDataCatalog exception as relayed by Athena, of the fixed form
  'Entity Not Found (Service: AmazonDataCatalog; Status Code: 400; Error Code: EntityNotFoundException; Request ID: <request id>; Proxy: null)'.
Every token of that message is fixed text except the per-request Request ID, which is a value only the serving call can produce (the check normalises it to <id>, so it is not part of the requirement). The leading 'Entity Not Found' is the relayed Glue message, and the parenthesised envelope names the relaying service, the relayed HTTP status, the relayed error code EntityNotFoundException and the proxy. In the same check Glue's own GetTable on an absent table, and on a table in an absent database, is EntityNotFoundException with message 'Entity Not Found' — those two assertions pass; only the Athena relay diverges.
Emulator: the status and error code are right (HTTP 400, MetadataException) but the Message is the bare 'Entity Not Found' — the relayed Glue text alone, with the whole '(Service: AmazonDataCatalog; Status Code: 400; Error Code: EntityNotFoundException; Request ID: ...; Proxy: null)' envelope missing.
Why: a client (or a human reading a stack trace) that parses the envelope to recover the underlying catalog error code EntityNotFoundException, or that matches on AWS's full message, cannot tell this MetadataException apart from any other metadata failure, so an absent-table condition is not recognised and retry/fallback logic keyed on EntityNotFoundException never fires.
Fix: the athena crate's GetTableMetadata handler — the not-found branch must render the relayed AmazonDataCatalog EntityNotFoundException as the wrapped envelope message instead of emitting the relayed text alone.
Traces: athena__cloudformation__glue.glue-get-table-for-absent-entity-rejected.

Related OPEN issue that does NOT cover this cause: 20261104T082656Z-athena-glue-gettablemetadata-s-metadataexception-for-a-dropped.md carries the replay key for athena__glue.athena-table-metadata-follows-a-glue-table-drop-and-recreate (a table DROPPED after a successful read, where the emulator answered 'Table does not exist: <table>'). It does not carry this check's key, and the emulator's answer here is already different text ('Entity Not Found'), so that issue's drop-and-recreate example does not exercise or close this absent-table path. Both end in the same envelope requirement, so a fixer should re-replay this check after touching that handler.

## Evidence
Replay of the simparity-behavioral-aws-v1 behavioral suite against the emulator built from this checkout; service heading athena__cloudformation__glue (25/27 passed, 2 failed).

[mismatch] athena__cloudformation__glue.glue-get-table-for-absent-entity-rejected
  check_athena__cloudformation__glue_glue_get_table_for_absent_entity_rejected.py, line 49, in run
  assert (code, message) == ("MetadataException", expected_message)
  AssertionError: GetTableMetadata(absent table) returned ('MetadataException', 'Entity Not Found')

Expected (code, message), after the check normalises the per-call request id to <id>:
  ('MetadataException',
   'Entity Not Found (Service: AmazonDataCatalog; Status Code: 400; Error Code: EntityNotFoundException; Request ID: <id>; Proxy: null)')
Actual:
  ('MetadataException', 'Entity Not Found')

Fixed text of the expected message: 'Entity Not Found (Service: AmazonDataCatalog; Status Code: 400; Error Code: EntityNotFoundException; Request ID: ' + <per-request id> + '; Proxy: null)'. The only variable part is the request id, which the check normalises away; nothing else in the message comes from the request or from state.

The earlier assertions in the same check pass: glue.get_table on an absent table name, and on a table in an absent database, both answer EntityNotFoundException / 'Entity Not Found', and both the Glue and the Athena read of the REAL table resolve the table by name.

## Steps to Reproduce
1. git_clone the repo and let the behavioral suite build and boot the emulator.
2. Run the simparity-behavioral-aws-v1 suite and inspect athena__cloudformation__glue.glue-get-table-for-absent-entity-rejected.
3. Minimal manual repro: create an S3 bucket, a Glue database, and a Glue table in it (TextInputFormat / HiveIgnoreKeyTextOutputFormat, bare LazySimpleSerDe, no TableType). Confirm glue.get_table(DatabaseName=<db>, Name=<tbl>) and athena.get_table_metadata(CatalogName='AwsDataCatalog', DatabaseName=<db>, TableName=<tbl>) both resolve the table.
4. Call athena.get_table_metadata(CatalogName='AwsDataCatalog', DatabaseName=<db>, TableName='sim2real_absent_tbl').
5. Expect HTTP 400, Code 'MetadataException', Message 'Entity Not Found (Service: AmazonDataCatalog; Status Code: 400; Error Code: EntityNotFoundException; Request ID: <request id>; Proxy: null)'. Observe Code 'MetadataException' with Message 'Entity Not Found' — the wrapped envelope is absent.
