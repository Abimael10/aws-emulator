Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue.catalog-duplicate-create-rejected
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue.catalog-duplicate-create-rejected|run#16|AssertionError|assert message == expected_message, (
Issue-Commit: 41f45c3261c18c20ba6cde2b04a98f3773ee8053

# glue CreateCatalog duplicate-name AlreadyExistsException message carries a trailing period AWS does not send (regression)

> _Note: this issue was identified against the code base at commit `41f45c3261c18c20ba6cde2b04a98f3773ee8053`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCatalog

## Description
REOPENS 20261124T113921Z-glue-createcatalog-s-duplicate-name-alreadyexistsexception-message-carries.md: its fix left the call refused. The refusal class itself is right (HTTP 400 / AlreadyExistsException); what that fix was supposed to correct — the message TEXT of that refusal — is diverging again, so check glue.catalog-duplicate-create-rejected is failing on the same assertion the resolved issue recorded. The earlier 20261122T104827Z-glue-createcatalog-duplicate-name-alreadyexistsexception-message-carries-a.md claimed the same fix, and the still-open 20261120T202955Z-glue-2-net-new-causes-checkschemaversionvalidity-s-json.md asks for the OPPOSITE punctuation in its (already reconciled, do-not-act) Cause 2 — that reconciliation note must stay in force; the recording in this check wins.

Cause 1 of 1: CreateCatalog's duplicate-name AlreadyExistsException message has an extra terminal period.
  AWS:      Smithy (com.amazonaws.glue#AWSGlue): CreateCatalog input {Name (CatalogNameString, required, @length 1..64, @pattern ^(?!(.*[.\/\\]|aws:)).*$), CatalogInput (required), Tags (@length 0..50)}; modeled errors include AlreadyExistsException (client, HTTP 400, "A resource to be created or added already exists."). The model does not state the message string, so the wording is recorded behavior only: a CreateCatalog whose Name is already held is answered HTTP 400, Error.Code AlreadyExistsException, Error.Message exactly `Catalog already exists` — the entire message is FIXED text with no request- or state-derived part (no catalog name, id, account or timestamp), and it is NOT period-terminated. The punctuation is per-message, not per-service: the same recording's GetCatalog miss IS period-terminated (`Catalog not found.`), so the sentences must not be normalized to one style. The rejected create also leaves the stored catalog untouched (its Description is not replaced by the conflicting CatalogInput).
  Emulator: the status (400) and the code (AlreadyExistsException) are correct, but the message served is `Catalog already exists.` — the same fixed text with a '.' appended. Replay: "AssertionError: expected CreateCatalog's duplicate-name message 'Catalog already exists', got 'Catalog already exists.'".
  Why:      the error message is part of the error contract. A client, test suite, log scraper, error-mapping table or Hive/Spark metastore shim that matches AWS's exact duplicate-catalog sentence to decide a create is a harmless duplicate does not match the emulator's sentence, so a correctly-detected duplicate surfaces as an unrecognized failure and the caller takes the wrong branch.
  Fix:      the shared Glue message constant the CreateCatalog handler's duplicate-name branch formats (crate::state::messages::CATALOG_ALREADY_EXISTS), not the handler's control flow, which already selects the right error. The GetCatalog/DeleteCatalog not-found wording must be left alone. Because this text has now regressed twice, the constant needs a regression guard rather than another one-line edit.
  Traces:   check glue.catalog-duplicate-create-rejected. Handler branch: services/service-com-amazonaws-glue-awsglue/src/ops/create_catalog.rs lines 273-284 (AlreadyExistsException built from crate::state::messages::CATALOG_ALREADY_EXISTS); the constant lives in services/service-com-amazonaws-glue-awsglue/src/state/messages.rs.

## Evidence
Check glue.catalog-duplicate-create-rejected (verdict: mismatch), failing at check_glue_catalog_duplicate_create_rejected.py line 53:
  AssertionError: expected CreateCatalog's duplicate-name message 'Catalog already exists', got 'Catalog already exists.'

Expected (AWS, recorded): HTTP 400 / Error.Code = "AlreadyExistsException" / Error.Message = "Catalog already exists"
Actual (emulator):        HTTP 400 / Error.Code = "AlreadyExistsException" / Error.Message = "Catalog already exists."

Fixed text vs values: the whole message is fixed text; it embeds no value from the request or from state, so the only divergence is the appended '.'. The preceding assertion on Error.Code passed, and the positive control before it (GetCatalog reporting the freshly created Description) passed.

Not reached because the check aborts on the message assertion, so still unverified: that the rejected create does not overwrite the original Description; that DeleteCatalog then makes GetCatalog answer EntityNotFoundException with 'Catalog not found.'; that the identical create succeeds once the name conflict is lifted. Re-replay the check after the message fix.

## Steps to Reproduce
1. Boot the emulator and point an AWS SDK/CLI at it.
2. s3tables create-table-bucket --name <unique-bucket> ; note the returned ARN.
3. glue create-catalog --name <unique-catalog> --catalog-input '{"Description":"original catalog description","FederatedCatalog":{"ConnectionName":"aws:s3tables","Identifier":"<bucket arn>"}}' -> 200.
4. glue get-catalog --catalog-id <unique-catalog> -> control: Description is "original catalog description".
5. Re-issue create-catalog with the SAME --name and a different Description.
6. Observe the refusal: HTTP 400, Code AlreadyExistsException (correct), Message "Catalog already exists." — AWS sends "Catalog already exists" with no trailing period.
7. Or replay the suite check glue.catalog-duplicate-create-rejected.
