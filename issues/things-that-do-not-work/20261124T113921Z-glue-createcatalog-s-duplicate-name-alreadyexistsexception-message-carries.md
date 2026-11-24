Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue.catalog-duplicate-create-rejected
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue.catalog-duplicate-create-rejected|run#16|AssertionError|assert message == expected_message, (
Issue-Commit: df3e5af3e6ae07ab9a03dea7418cd4cc91eea7e3

# glue: CreateCatalog's duplicate-name AlreadyExistsException message carries a trailing period AWS does not send

> _Note: this issue was identified against the code base at commit `df3e5af3e6ae07ab9a03dea7418cd4cc91eea7e3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCatalog

## Description
Cause 1 of 1: CreateCatalog correctly refuses a Name already in use with AlreadyExistsException, but the message text it serves is punctuated differently from AWS's.

AWS: Smithy (com.amazonaws.glue#AWSGlue, awsJson1_1) models CreateCatalog with input Name (CatalogNameString, required, @length 1..64, @pattern ^(?!(.*[.\/\\]|aws:)).*$) and CatalogInput (required), and declares AlreadyExistsException (client error, HTTP 400, doc: "A resource to be created or added already exists.") among its errors. The message string itself is not modeled, so this is recorded behavior only: for a CreateCatalog whose Name is already taken AWS answers HTTP 400, Code AlreadyExistsException, Message exactly `Catalog already exists` — fixed text, no variable part, and NO terminating period. (AWS is deliberately inconsistent across the catalog surface here: GetCatalog's not-found message in the same recording IS period-terminated, `Catalog not found.`, so the punctuation cannot be normalized one way across the service — each message must be reproduced as AWS words it for that error class.)

Emulator: the status (400) and the code (AlreadyExistsException) are right, but the message is `Catalog already exists.` — the same fixed text with an extra trailing '.' appended. Replay: "AssertionError: expected CreateCatalog's duplicate-name message 'Catalog already exists', got 'Catalog already exists.'".

Why: the message is part of the error contract. A client (or a test, a log scraper, an error-mapping table, a Hive/Spark metastore shim that pattern-matches Glue's catalog messages) that compares or matches on AWS's exact duplicate-catalog sentence does not match the emulator's, so a correctly-detected duplicate is reported to the caller as an unrecognized failure. The divergence is purely in the message text; the refusal itself and its effect-absence (the original Description is not overwritten) are otherwise correct.

Fix: the Glue catalog message constant used by the CreateCatalog handler's duplicate-name branch — the shared `messages` item it formats, not the handler's control flow, which already picks the right error. The GetCatalog/DeleteCatalog not-found wording must not be changed along with it.

Traces: glue.catalog-duplicate-create-rejected.

## Evidence
Check glue.catalog-duplicate-create-rejected (mismatch):
  AssertionError: expected CreateCatalog's duplicate-name message 'Catalog already exists', got 'Catalog already exists.'

Expected (AWS, recorded): HTTP 400 / Error.Code = "AlreadyExistsException" / Error.Message = "Catalog already exists"
Actual (emulator):        HTTP 400 / Error.Code = "AlreadyExistsException" / Error.Message = "Catalog already exists."

Fixed vs variable parts: the message is entirely fixed text ("Catalog already exists") — it embeds no catalog name, id, account or timestamp — so the only divergence is the appended '.'.

Emulator source carrying the diverging string: services/service-com-amazonaws-glue-awsglue/src/ops/create_catalog.rs lines 278-284 return AlreadyExistsException with message = crate::state::messages::CATALOG_ALREADY_EXISTS.

Controls in the same check that passed up to this point: the error CODE was AlreadyExistsException as expected (the assertion on code precedes the message assertion and did not fire).

## Steps to Reproduce
1. Replay the behavioral suite and run check glue.catalog-duplicate-create-rejected.
2. Minimal reproduction against the emulator with any SDK:
   a. s3tables CreateTableBucket(name=<unique>) -> note the returned bucket arn.
   b. glue CreateCatalog(Name=<unique-name>, CatalogInput={"Description": "original catalog description", "FederatedCatalog": {"ConnectionName": "aws:s3tables", "Identifier": <bucket arn>}}) -> 200.
   c. Call glue CreateCatalog again with the SAME Name and a different Description.
3. Observe the refusal: HTTP 400, Code AlreadyExistsException, Message "Catalog already exists." — AWS sends "Catalog already exists" with no trailing period.
