Issue-Source: trace-sweep-divergence
Issue-Trace: athena-named-query-crud-and-batch-retrieval
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: athena-named-query-crud-and-batch-retrieval
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena|BatchGetNamedQuery|2xx->2xx|$.NamedQueries[*]
Issue-Commit: 9335e6a6f0eaf1e6ae0c62968f13439343c093f5

# Athena BatchGetNamedQuery serves stale pre-update named queries and in a different order

> _Note: this issue was identified against the code base at commit `9335e6a6f0eaf1e6ae0c62968f13439343c093f5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetNamedQuery

## Description
One cause. Cause 1 of 1: BatchGetNamedQuery (fixture athena-named-query-crud-and-batch-retrieval, step task#7) answers 200 but its NamedQueries list diverges from AWS in two ways at once: every entry for a named query that had been updated carries the PRE-update Name and QueryString, and the entries are emitted in a different position order than AWS emits them.

AWS: Smithy (services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json): BatchGetNamedQuery input NamedQueryIds (target NamedQueryIdList, required, @length min 1 max 50); output NamedQueries (NamedQueryList) and UnprocessedNamedQueryIds; errors InternalServerException (500) and InvalidRequestException (400). Each NamedQuery carries Name, Database, QueryString, NamedQueryId and WorkGroup. Because BatchGetNamedQuery is a batched read of the same records GetNamedQuery serves, the contract is that it reflects the CURRENT stored state: after an UpdateNamedQuery (@idempotent; required members NamedQueryId, Name, QueryString, optional Description) has replaced a named query's Name and QueryString, a BatchGetNamedQuery that includes that NamedQueryId must return the new Name and the new QueryString, exactly as GetNamedQuery must. The recording confirms this: the batch response contains the updated query with its post-update name (the '...-updated-<suffix>' form) and its post-update SQL ('SELECT 100'), the other query unchanged. Ordering: recorded behavior only — the model places no @uniqueItems or ordering trait on NamedQueryList, and the recording shows AWS returning the two resolved entries in an order that is NOT the emulator's internal creation order (the recording has the second-created query at index 0 and the updated first-created query at index 1). Hypothesis: AWS emits NamedQueries in the order the request's NamedQueryIds list them; the requirement grounded in the recording is only that the batch's entry order follows the request rather than the store's insertion order.

Emulator: the same call answers 200 with the batch entries in insertion order and with pre-update content for the mutated record — $.NamedQueries[0] is the first-created query still holding its creation-time Name and QueryString (the un-updated name, QueryString 'SELECT 1') where the recording has the second-created query, and $.NamedQueries[1] is the second-created query where the recording has the updated first-created one. No entry anywhere in the list carries the updated Name/QueryString, so the stale read is not merely a reordering. The names, ids and the literal SQL strings in the recording are that run's values filling the template; the requirement is that each returned NamedQuery equals the record as last written by Create/UpdateNamedQuery, and that entry order follows the request.

Why: a client that edits a saved query and then confirms the edit with a single batched read (the normal way a console or IaC tool refreshes a list of saved queries) sees the old definition and concludes the update failed, or silently re-runs stale SQL. Independently, a client that zips the returned NamedQueries against the NamedQueryIds it sent — valid only when the response follows the request order — pairs each id with the wrong query's Name/QueryString/Database, which mis-attributes every saved query in the batch even when none of them is stale.

Fix: the BatchGetNamedQuery handler and the named-query state store in the athena crate — the batch read must project the records' current (post-UpdateNamedQuery) Name/QueryString/Description, and must assemble NamedQueries by walking the request's NamedQueryIds rather than iterating the store. The underlying write defect (UpdateNamedQuery not applying to stored state) is described by the open issue 20261113T230556Z-athena-named-query-mutations-are-not-applied-updatenamedquery.md, but that issue carries only the GetNamedQuery replay keys (task#6, task#10) and does NOT carry this BatchGetNamedQuery step, and it says nothing about batch entry ordering, so this step must be re-replayed on its own. The open behavioral issue 20261114T225116Z-athena-6-net-new-causes-batchgetnamedquery-over-limit.md covers only BatchGetNamedQuery's over-50-ids refusal classification, and the stub issue 20261114T225116Z-athena-batchgetnamedquery-stub.md only asks for the operation to exist; neither covers this positive-path divergence.

Traces: athena-named-query-crud-and-batch-retrieval#task#7 (BatchGetNamedQuery, status 200 -> 200).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 319/394 steps passed, score 81.0%, 28 divergence clusters). This is cluster 20 of 28, verdict=mismatch, service=athena, operation=BatchGetNamedQuery, expected_status 200 / actual_status 200, stub_path empty and owned_by empty — no open issue carries its replay key and no unimplemented operation blocks it.

Fixture athena-named-query-crud-and-batch-retrieval, step task#7, divergence_key service=athena|BatchGetNamedQuery|status=200->200|keys=$.NamedQueries[0],$.NamedQueries[1]:
  $.NamedQueries[1]: missing (expected {'Database': 'default', 'Name': 'sim2real-probe-athena-named-query-one-updated-00eaff9b', 'NamedQueryId': '50154e64-34bd-4f65-b8d1-186da3167997', 'QueryString': 'SELECT 100', 'WorkGroup': 'sim2real-probe-athena-named-query-wg-00eaff9b'}, got {'Name': 'sim2real-probe-athena-named-query-two-00eaff9b', 'Database': 'default', 'QueryString': 'SELECT 2', 'NamedQueryId': 'db67056d-128b-49ce-a5fd-2f31904202d2', 'WorkGroup': 'sim2real-probe-athena-named-query-wg-00eaff9b'})
  $.NamedQueries[0]: extra (expected {'Database': 'default', 'Name': 'sim2real-probe-athena-named-query-two-00eaff9b', 'NamedQueryId': 'db67056d-128b-49ce-a5fd-2f31904202d2', 'QueryString': 'SELECT 2', 'WorkGroup': 'sim2real-probe-athena-named-query-wg-00eaff9b'}, got {'Name': 'sim2real-probe-athena-named-query-one-00eaff9b', 'Database': 'default', 'QueryString': 'SELECT 1', 'NamedQueryId': '50154e64-34bd-4f65-b8d1-186da3167997', 'WorkGroup': 'sim2real-probe-athena-named-query-wg-00eaff9b'})

Read positionally: for NamedQueryId 50154e64-... the recording carries Name '...-one-updated-...' / QueryString 'SELECT 100' at index 1, while the emulator carries Name '...-one-...' / QueryString 'SELECT 1' at index 0. The per-run suffix ('00eaff9b'), the generated NamedQueryIds and the workgroup name are environment values, not requirements; the diverging facts are (a) the updated record's Name and QueryString are served at their pre-update values and (b) the two resolved entries occupy swapped positions in NamedQueries.

Model evidence: smithy_query(query="operation", operation="BatchGetNamedQuery,UpdateNamedQuery", model_path="services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json") — BatchGetNamedQuery.NamedQueryIds required @length 1..50, output NamedQueries/UnprocessedNamedQueryIds, errors InternalServerException|InvalidRequestException; UpdateNamedQuery idempotent with required NamedQueryId/Name/QueryString and an empty output.

## Steps to Reproduce
1. git_clone() and boot the emulator built from the checkout.
2. Replay fixture athena-named-query-crud-and-batch-retrieval from trace set simparity-wire-aws-v1 and inspect step task#7.
3. Manual repro:
   a. CreateWorkGroup(Name=W).
   b. CreateNamedQuery(Name='q-one', Database='default', QueryString='SELECT 1', WorkGroup=W) -> id Q1.
   c. CreateNamedQuery(Name='q-two', Database='default', QueryString='SELECT 2', WorkGroup=W) -> id Q2.
   d. UpdateNamedQuery(NamedQueryId=Q1, Name='q-one-updated', QueryString='SELECT 100').
   e. BatchGetNamedQuery(NamedQueryIds=[Q2, Q1]).
   Expected: 200 with NamedQueries in the order the ids were sent — index 0 the Q2 record, index 1 the Q1 record carrying Name 'q-one-updated' and QueryString 'SELECT 100'.
   Observed: 200 with index 0 the Q1 record still holding Name 'q-one' and QueryString 'SELECT 1', and index 1 the Q2 record — the post-update values appear nowhere in the response and the entries are in store-insertion order instead of request order.
