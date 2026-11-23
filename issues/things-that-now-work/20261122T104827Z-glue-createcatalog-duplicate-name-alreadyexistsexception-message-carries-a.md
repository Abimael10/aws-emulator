Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue.catalog-duplicate-create-rejected
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue.catalog-duplicate-create-rejected|run#16|AssertionError|assert message == expected_message, (
Issue-Commit: eb235f107f11a58ccabfb1e09a1e98cf48d2a7c2

# glue CreateCatalog duplicate-name AlreadyExistsException message carries a terminal period AWS does not emit

> _Note: this issue was identified against the code base at commit `eb235f107f11a58ccabfb1e09a1e98cf48d2a7c2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCatalog

## Description
One net-new root cause in the AWS Glue control plane. The failing check is not marked ALREADY FILED and is not OWNED by any open issue.

Cause 1 of 1: CreateCatalog's duplicate-name refusal message has a trailing period appended to the sentence.
  AWS:      Smithy (com.amazonaws.glue#AWSGlue): CreateCatalog input {Name (required, CatalogNameString, @length 1..64, pattern ^(?!(.*[.\/\\]|aws:)).*$), CatalogInput (required), Tags (TagsMap, @length 0..50)}, output empty; modeled errors include AlreadyExistsException (errorKind client, httpError 400). The model does not state the message text, so the wording is recorded behavior only: creating a catalog whose Name is already in use is refused HTTP 400 with Error Code 'AlreadyExistsException' and the message 'Catalog already exists' — the whole message is FIXED text with no request- or state-derived part (no name, id, limit or member name is interpolated), and it ends WITHOUT a terminal period. The rejected call leaves the existing catalog untouched (its Description is not replaced by the rejected CatalogInput), and once the original is deleted the identical call succeeds.
  Emulator: the status (400) and the Error Code ('AlreadyExistsException') are right; the message is served as 'Catalog already exists.' — one extra terminal period on otherwise-correct fixed text.
  Why:      an error message is part of the contract. A client that string-matches the already-exists sentence to decide whether a create is idempotent (SDK conformance suites, catalog/registry provisioning tools, CI golden-text gates) sees a message AWS never emits and takes the wrong branch or fails the assertion, even though the code and status are correct.
  Fix:      the message constant in the AlreadyExistsException construction of the CreateCatalog handler's duplicate-name pre-check. Only the trailing character of that fixed sentence is wrong; no other part of the refusal diverges in this check.
  NOTE for the fixer — a CONFLICT with an already-open issue: open issue 20261120T202955Z-glue-2-net-new-causes-checkschemaversionvalidity-s-json.md, Cause 2 of 2, asserts the OPPOSITE requirement for the same refusal (it is grounded in check glue.create-catalog-duplicate-name-rejected, which asserts the message IS 'Catalog already exists.' with the period, and that check passes in this run). The two recorded checks disagree about the terminal period for one and the same CreateCatalog duplicate-name refusal, so a fix that satisfies one flips the other. Do not change the constant blind: reconcile the two recordings first (re-replay BOTH glue.catalog-duplicate-create-rejected and glue.create-catalog-duplicate-name-rejected after any change) and close that issue's Cause 2 together with this one. Everything else in this check beyond the message (the AlreadyExistsException code, the original catalog's Description surviving the rejected call, GetCatalog's 'Catalog not found.' after DeleteCatalog, and the re-create succeeding once the Name conflict is lifted) is asserted AFTER the message assertion and is therefore still unverified — re-replay to cover it.
  Traces:   check glue.catalog-duplicate-create-rejected. Handler: services/service-com-amazonaws-glue-awsglue/src/ops/create_catalog.rs (310 lines).

## Evidence
Replay output (behavioral suite, section ### glue):

[mismatch] glue.catalog-duplicate-create-rejected
  check_glue_catalog_duplicate_create_rejected.py, line 53, in run
  AssertionError: expected CreateCatalog's duplicate-name message 'Catalog already exists', got 'Catalog already exists.'

Request sequence that produced it (from the check source):
  s3tables CreateTableBucket(name=<bucket>) -> <bucket arn>
  glue CreateCatalog(Name=<name>, CatalogInput={Description:'original catalog description', FederatedCatalog:{ConnectionName:'aws:s3tables', Identifier:<bucket arn>}}, Tags={...})  -> 200
  glue GetCatalog(CatalogId=<name>) -> Catalog.Description == 'original catalog description'   (positive control, passes)
  glue CreateCatalog(Name=<same name>, CatalogInput={Description:'a completely different description', FederatedCatalog:{ConnectionName:'aws:s3tables', Identifier:<bucket arn>}}, Tags={...})

Expected (AWS): HTTP 400, Error Code 'AlreadyExistsException', Error Message 'Catalog already exists'
Actual (emulator): HTTP 400, Error Code 'AlreadyExistsException' (correct), Error Message 'Catalog already exists.'

Fixed text vs values: the entire message is fixed text — it interpolates nothing from the request (<name>, the bucket ARN and the Descriptions above are this run's values and appear nowhere in the message) and nothing from emulator state. The sole divergence is the terminal '.' character.

Conflicting recording to reconcile: open issue 20261120T202955Z-glue-2-net-new-causes-checkschemaversionvalidity-s-json.md (Cause 2) records check glue.create-catalog-duplicate-name-rejected asserting message == 'Catalog already exists.' for the same refusal; that check is NOT in this run's failing list, i.e. it passes against the period-bearing message this check rejects.

## Steps to Reproduce
1. Build and boot the emulator from this checkout (glue and s3tables both serve) and point an AWS SDK/CLI at it.
2. s3tables create-table-bucket --name <bucket>   (note the returned ARN)
3. glue create-catalog --name <catalog> --catalog-input '{"Description":"original catalog description","FederatedCatalog":{"ConnectionName":"aws:s3tables","Identifier":"<bucket arn>"}}'
4. glue get-catalog --catalog-id <catalog> — control: Description is 'original catalog description'.
5. Re-issue create-catalog with the SAME --name and a different Description.
6. Observe the refusal: HTTP 400 / AlreadyExistsException (both correct) with Message 'Catalog already exists.' where AWS serves 'Catalog already exists' (no terminal period).
7. Then (currently unreached, verify after the message fix): glue get-catalog --catalog-id <catalog> still reports Description 'original catalog description'; glue delete-catalog --catalog-id <catalog>; glue get-catalog is refused EntityNotFoundException 'Catalog not found.'; the identical create-catalog then succeeds and GetCatalog reports the new Description.
8. Or re-replay: glue.catalog-duplicate-create-rejected (and, for the conflicting expectation, glue.create-catalog-duplicate-name-rejected).
