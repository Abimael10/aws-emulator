Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.deep-dynamodb-1-ddbvec-update-table-backfills-preexisting-items
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.deep-dynamodb-1-ddbvec-update-table-backfills-preexisting-items|run#21|AssertionError|assert ('CREATING', True) in states, (
Issue-Commit: 7ce5552927764f5e5adc0c494ca5a7d589c732e6

# dynamodb: a vector index added by UpdateTable over pre-existing items never reports Backfilling=true — it goes CREATING/Backfilling=false straight to ACTIVE

> _Note: this issue was identified against the code base at commit `7ce5552927764f5e5adc0c494ca5a7d589c732e6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateTable

## Description
This issue carries 1 net-new root cause.

Cause 1 of 1: UpdateTable with VectorIndexUpdates=[{Create: ...}] on a table that already holds items builds the index without ever exposing a backfill window — DescribeTable's VectorIndexDescription reports Backfilling=false for the whole CREATING phase and then ACTIVE.
Failing check ids: dynamodb.deep-dynamodb-1-ddbvec-update-table-backfills-preexisting-items.

AWS: Smithy (com.amazonaws.dynamodb#DynamoDB_20120810): VectorIndexDescription carries members IndexName, SearchSchema, Projection, VectorAttribute, Dimensions, DistanceFunction, IndexStatus (enum IndexStatus: CREATING "The index is being created", ACTIVE "The index is ready for use", DELETING), Backfilling (boolean) documented as "Specifies whether the index is currently backfilling. During backfill, SearchVectors operations might return incomplete results", IndexSizeBytes, ItemCount, IndexArn. UpdateTable.VectorIndexUpdates is modelled with errors InternalServerError, InvalidEndpointException, LimitExceededException, ResourceInUseException, ResourceNotFoundException. Recorded behavior: when a vector index is added to a table that already contains items, the index is first described as IndexStatus=CREATING with Backfilling=true while the pre-existing items are indexed, and only then as IndexStatus=ACTIVE with Backfilling absent/false; writes accepted during that window are indexed too, and the Backfilling=true observation always precedes the ACTIVE/not-backfilling one. An index created on an empty table (CreateTable, or UpdateTable on a table with no items) has nothing to backfill, so Backfilling=false throughout is correct only in that case — the flag is driven by whether pre-existing items still have to be indexed, never by a fixed value.

Emulator: for an index added over 30 pre-existing items, every DescribeTable poll (0.2 s interval from the moment UpdateTable returned) reports only the state pairs (IndexStatus, Backfilling) = ('CREATING', False) then ('ACTIVE', False). The check repeated the add/drop cycle 3 times and never saw ('CREATING', True): "attempt 1 states [('CREATING', False), ('ACTIVE', False)]", "attempt 2 states [('CREATING', False), ('ACTIVE', False)]", "attempt 3 states [('CREATING', False), ('ACTIVE', False)]". The Backfilling member is therefore never set true (or never emitted) for a backfilling index, and because the window never opens there is no state in which a client can observe a write landing mid-backfill. The CREATING-state refusal of SearchVectors itself was observed, so the divergence is confined to the backfill signalling.

Why: a client that adds a vector index to a populated table uses Backfilling to decide when SearchVectors results can be trusted — the model states results "might return incomplete results" during backfill. Against the emulator that flag is a constant false, so any gate written as "wait until Backfilling is false" passes immediately and any logic that must observe, log or report the backfill phase (progress reporting, infrastructure tooling that waits for an index to be fully populated, tests that assert a backfill happened) never fires. Code that treats Backfilling=false plus ACTIVE as "index fully populated" cannot be exercised, and a consumer cannot distinguish an index with nothing to backfill from one that is still being filled.

Fix: the UpdateTable handler's VectorIndexUpdates Create branch and the vector-index state it maintains — the index's state machine must carry a backfill phase for the items already present in the table, and the DescribeTable/UpdateTable serializer for VectorIndexDescription must report Backfilling=true for the duration of that phase, then IndexStatus=ACTIVE with Backfilling false, in that order.

Related open issues (neither carries this check's replay key for this failure mode, so neither covers it): 20261108T011128Z-dynamodb-2-net-new-causes-describetable-invents-an.md listed this same check under a NotImplemented key for UpdateTable+VectorIndexUpdates Create — that refusal is gone (the call is now accepted and the index does reach ACTIVE), and what remains is the missing Backfilling phase; 20261111T170613Z-dynamodb-vector-index-validation-is-not-enforced-its.md covers vector-index validation, CREATING-state refusal wording and vector capacity sizing, not the Backfilling flag.

Traces: dynamodb.deep-dynamodb-1-ddbvec-update-table-backfills-preexisting-items.

## Evidence
Cause 1 — Expected: after UpdateTable adds vector index 'late-euc' (VectorAttribute emb, Dimensions 4, DistanceFunction EUCLIDEAN, Projection ALL) to a PAY_PER_REQUEST table holding 30 items, the sequence of distinct (IndexStatus, Backfilling) pairs observed via DescribeTable contains ('CREATING', True) and ends with ('ACTIVE', False), with ('CREATING', True) occurring before ('ACTIVE', False).
Actual: AssertionError: a vector index created over 30 existing items never reported Backfilling=true in 3 attempts: ["attempt 1 states [('CREATING', False), ('ACTIVE', False)]", "attempt 2 states [('CREATING', False), ('ACTIVE', False)]", "attempt 3 states [('CREATING', False), ('ACTIVE', False)]"] (check file check_dynamodb_deep_dynamodb_1_ddbvec_update_table_backfills_preexisting_items.py, line 126). UpdateTable itself was accepted and the index was echoed as CREATING with Dimensions 4 / EUCLIDEAN, and it did reach ACTIVE — only the Backfilling=true phase is missing, so the item written mid-backfill was never attempted either.

## Steps to Reproduce
1. CreateTable with a single HASH key 'pk' (S) and BillingMode PAY_PER_REQUEST; wait for the table to be ACTIVE.
2. PutItem 30 items, each with a 4-number list attribute 'emb' and a 'payload' string.
3. Call UpdateTable with VectorIndexUpdates=[{'Create': {'IndexName': 'late-euc', 'VectorAttribute': {'AttributeName': 'emb'}, 'Dimensions': 4, 'DistanceFunction': 'EUCLIDEAN', 'Projection': {'ProjectionType': 'ALL'}}}].
4. Poll DescribeTable every ~0.2 s and record each distinct (VectorIndexes[late-euc].IndexStatus, bool(Backfilling)) pair until the index is ACTIVE and not backfilling.
5. Observe only ('CREATING', False) then ('ACTIVE', False); ('CREATING', True) is never reported, so no write can be made during the backfill window.
6. Or replay the check id in trace_ids.
