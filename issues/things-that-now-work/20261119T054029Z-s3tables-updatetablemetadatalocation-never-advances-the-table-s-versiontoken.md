Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3tables.update-table-metadata-location-stale-token-leaves-location-and-token-unchanged
Issue-Shape-Id: com.amazonaws.s3tables#S3TableBuckets
Issue-Key: s3tables.update-table-metadata-location-stale-token-leaves-location-and-token-unchanged|run#15|AssertionError|assert first_token != original_token, (
Issue-Commit: 7b8b2bccc64d7535c4900457416769419fe3c7af

# s3tables UpdateTableMetadataLocation never advances the table's versionToken, so the accepted write answers with the token it was given and a stale token is never a conflict

> _Note: this issue was identified against the code base at commit `7b8b2bccc64d7535c4900457416769419fe3c7af`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateTableMetadataLocation

## Description
ONE net-new cause in s3tables, grounded in behavioral check s3tables.update-table-metadata-location-stale-token-leaves-location-and-token-unchanged.

Cause 1 of 1: a successful UpdateTableMetadataLocation leaves the table's versionToken unchanged and echoes the caller's token back, so the table's optimistic-concurrency version never moves and the stale-token refusal path is unreachable. Failing check: s3tables.update-table-metadata-location-stale-token-leaves-location-and-token-unchanged.

AWS: Smithy UpdateTableMetadataLocation — PUT /tables/{tableBucketARN}/{namespace}/{name}/metadata-location, required input versionToken (VersionToken, @length 1..2048) and metadataLocation, output versionToken required; modeled errors include ConflictException @httpError 409 ("The request failed because there is a conflict with a previous write. You can retry the request."). Recorded behavior only for the token's lifecycle, as the replayed check records it: versionToken is the table's optimistic-concurrency precondition, and a SUCCESSFUL metadata-location write advances it — the response carries a NEW token, different from the one the request presented. That new token is thereafter the table's current token: GetTableMetadataLocation returns it alongside the new metadataLocation, a further UpdateTableMetadataLocation presenting it succeeds (and advances the token again), and the previously presented token is now stale, so presenting it is refused with HTTP 409 ConflictException whose message is the fixed sentence for that class, "Provided version token does not match the table version token." (entirely fixed text — it substitutes no value from the request or state). After such a refusal the stored metadataLocation and versionToken stay at the last successful values.
Emulator: the accepted write answers HTTP 200 with $.versionToken equal to the token the request supplied — the check's first assertion fails with "the accepted update returned the same token 'b34109334c45de3a965d' it was given" (that literal is one run's generated token; the requirement is that the value differs from the presented one, not any particular value). Because the stored token never changes, the token the caller already holds remains valid forever: the follow-up call the check makes with the ORIGINAL token, which AWS refuses 409, is accepted 200 instead, and the ConflictException path of this operation is unreachable for a stale token.
Why: a client relying on AWS's semantics uses versionToken as its commit precondition on DeleteTable, RenameTable and UpdateTableMetadataLocation. With a token that never advances, two writers that both read the same table version both commit: the second write is not detected as a lost update, so an Iceberg metadata-pointer commit silently overwrites a concurrent commit instead of failing the precondition and forcing a re-read. Equally, a client that expects a fresh token from the write and stores it for its next commit cannot distinguish "my write landed" from "somebody else wrote", and any test or workflow asserting the 409 on a stale token cannot complete.
Fix: the version-token handling in this operation's handler and the table record's version token in the service state — on a successful metadata-location write, advance the table record's version token and answer with the advanced value (and keep refusing a presented token that is not the table's current one with the existing 409 ConflictException). The handler currently documents the opposite rule as intentional and deliberately does not re-mint, which is what this check contradicts. Related but NOT covering this: open issue 20261114T185644Z-s3tables-3-net-new-causes-gettable-accepts-both.md filed the opposite direction for the same member (a freshly minted, unrelated UUID in $.versionToken); the two must be reconciled — the requirement is a token that ADVANCES on a successful write and is the table's current token afterwards, not one that is re-minted at random on every read-back nor one that never moves. Open issue 20261109T141301Z-s3tables-renametable-rotates-the-table-s-versiontoken-so.md concerns RenameTable, a different operation.
Traces: s3tables.update-table-metadata-location-stale-token-leaves-location-and-token-unchanged.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (134/158 passed; this is the only failing check not already filed or owned).

- [mismatch] s3tables.update-table-metadata-location-stale-token-leaves-location-and-token-unchanged:
    File "check_s3tables_update_table_metadata_location_stale_token_leaves_location_and_token_unchanged.py", line 92, in run
      assert first_token != original_token, (
    AssertionError: the accepted update returned the same token 'b34109334c45de3a965d' it was given
  The check creates a table bucket, namespace and ICEBERG table, captures CreateTable's versionToken, then PUTs a metadata location under the table's warehouseLocation with that token. Expected: 200 with a versionToken different from the one presented. Got: 200 with the identical token.
  Because the check aborts at that assertion, the remaining AWS assertions it encodes are also unreachable against this emulator: the second PUT with the now-stale original token must be refused 409 ConflictException with message 'Provided version token does not match the table version token.'; GetTableMetadataLocation must then still report the first location and the first (advanced) token; and a third PUT presenting the current token must succeed and advance the token again.
- Mechanism in the checkout: services/service-com-amazonaws-s3tables-s3tablebuckets/src/ops/update_table_metadata_location.rs:237-269 writes table.metadata_location and calls table.record_modification(...) but, per its own comment ("the table's `versionToken` is NOT re-minted here"), returns table.version_token unchanged; the precondition compare at lines 208-215 therefore keeps matching the caller's original token on every subsequent call, so the ConflictException there can never fire for a stale token.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the behavioral suite check s3tables.update-table-metadata-location-stale-token-leaves-location-and-token-unchanged.
2. CreateTableBucket, CreateNamespace, then CreateTable(format=ICEBERG) and keep the response's versionToken as T0; read warehouseLocation with GetTable.
3. PUT /tables/{tableBucketARN}/{namespace}/{name}/metadata-location with metadataLocation=<warehouse>/metadata/00001-first.metadata.json and versionToken=T0. Observe 200 whose $.versionToken is T0 again, where AWS returns a new token T1 != T0.
4. Repeat the same PUT with a different metadataLocation and versionToken=T0 (now stale). Observe 200 and the location rewritten, where AWS refuses HTTP 409 ConflictException with message 'Provided version token does not match the table version token.' and leaves metadataLocation and versionToken at their last successful values.
5. GET the same path and observe the versionToken is still T0, where AWS reports the advanced token.
