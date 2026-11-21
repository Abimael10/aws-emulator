Issue-Source: trace-sweep-divergence
Issue-Trace: athena-glue-metadata-exploration
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: athena-glue-metadata-exploration
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena|ListDatabases|4xx->4xx|$.Message
Issue-Commit: 7b99816a4fb2396e47812944893a16c8ececa4c9

# Athena ListDatabases absent-catalog refusal is emulator-authored instead of AWS's 'Cannot find catalog' text

> _Note: this issue was identified against the code base at commit `7b99816a4fb2396e47812944893a16c8ececa4c9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDatabases

## Description
1 cause. ListDatabases, asked for a catalog that does not exist, refuses with the right HTTP status and the right error code but with an emulator-authored message, so the refusal text does not match AWS's for this error class.

Cause 1 of 1: ListDatabases with a CatalogName that names no registered data catalog.
AWS: Smithy (services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json) ListDatabases takes required CatalogName (target CatalogNameString, @length min 1 max 256, @pattern ^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$) plus optional NextToken (@length 1-1024), MaxResults (@range 1-50) and WorkGroup (@pattern ^[a-zA-Z0-9._-]{1,128}$), is @paginated, and models errors InternalServerException (server, HTTP 500), InvalidRequestException (client, HTTP 400) and MetadataException (client, HTTP 400). The model does not state message text, so for the text this is recorded behavior only: the recording shows AWS answering HTTP 400 for an unregistered catalog with the message 'Cannot find catalog <catalogName>.' — the fixed text is 'Cannot find catalog ' and the trailing '.', and <catalogName> is the request's own CatalogName member echoed verbatim.
Emulator: answers the same HTTP 400 and the same error code (only $.Message diverges in the replay) but with Message 'DataCatalog <catalogName> was not found.' — a differently worded, emulator-authored sentence ('DataCatalog ' / ' was not found.' is the emulator's own fixed text) around the same request-derived catalog name.
Why: the error code for this refusal is shared with other metadata failures, so a client that distinguishes an unregistered catalog from a malformed request or a backing-catalog failure does it by matching AWS's message. Against the emulator that match never succeeds, so register-catalog-on-demand and catalog-fallback logic takes the wrong branch and the absent-catalog condition surfaces as an opaque metadata error.
Fix: the athena crate's ListDatabases handler, in its catalog-lookup/not-found branch — render the refusal message as AWS's 'Cannot find catalog <catalogName>.' template with the name taken from the request, rather than the handler's own 'DataCatalog ... was not found.' wording. The neighbouring metadata reads author their own per-handler text too, so the renderer this branch uses should be the one shared across the catalog-metadata read path.
Traces: athena-glue-metadata-exploration#task#8.

Related OPEN issue that does NOT cover this cause: 20261116T142606Z-athena-metadata-lookup-refusals-omit-the-wrapped-amazondatacatalog.md covers GetTableMetadata for an absent TABLE and ListTableMetadata for an absent DATABASE (both needing the wrapped AmazonDataCatalog EntityNotFoundException envelope). This cause is a different operation and a different condition — an absent CATALOG on ListDatabases, whose AWS message is a plain Athena-authored sentence with no relayed envelope — and the replay reports this cluster as unowned, so that issue carries no replay key for this step.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, steps_total=433, steps_passed=394, score_pct=91.0).

Cause 1 — fixture athena-glue-metadata-exploration, step task#8, operation ListDatabases, verdict mismatch:
  expected_status 400, actual_status 400 (status matches)
  $.Message: value_mismatch (expected 'Cannot find catalog sim2real-probe-athena-nonexistent-catalog-b56f00c2.', got 'DataCatalog sim2real-probe-athena-nonexistent-catalog-b56f00c2 was not found.')
  cluster key: service=athena|ListDatabases|status=400->400|keys=$.Message
  affected (1): athena-glue-metadata-exploration#task#8
  owned_by: (none) — not covered by any open issue's replay key; stub_path: (none) — the operation is implemented and answers.

Message decomposition: in the expected message 'Cannot find catalog ' and the final '.' are the fixed text of this error class; 'sim2real-probe-athena-nonexistent-catalog-b56f00c2' is a per-run probe name supplied as the request's CatalogName and is NOT part of the requirement. In the actual message 'DataCatalog ' and ' was not found.' are the emulator's own fixed text around the same request-derived name.
Status and error code agree; only $.Message diverges.

## Steps to Reproduce
1. git_clone the repo and let the wire corpus build and boot the emulator.
2. Replay trace set simparity-wire-aws-v1 and inspect fixture athena-glue-metadata-exploration, step task#8.
3. Manual repro: call athena.list_databases(CatalogName=<a name that is not a registered data catalog>). Expect HTTP 400 with Message 'Cannot find catalog <catalogName>.'; observe HTTP 400 with Message 'DataCatalog <catalogName> was not found.'
4. Re-replay athena-glue-metadata-exploration#task#8 after changing the handler to confirm the cause is closed.
