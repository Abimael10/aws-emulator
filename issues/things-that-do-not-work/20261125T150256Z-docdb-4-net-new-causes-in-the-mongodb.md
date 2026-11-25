Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: docdb.mongo-aggregation-pipeline-groups-sums-and-sorts-the-matched-documents docdb.mongo-change-stream-delivers-insert-update-and-delete-events-in-commit-order docdb.mongo-transaction-writes-stay-invisible-until-commit-and-an-abort-discards-them
Issue-Shape-Id: com.amazonaws.docdb#AmazonRDSv19
Issue-Key: docdb.mongo-aggregation-pipeline-groups-sums-and-sorts-the-matched-documents|run#11|AssertionError|assert got == want, (
Issue-Key: docdb.mongo-change-stream-delivers-insert-update-and-delete-events-in-commit-order|_ok#5|AssertionError|assert reply.get("ok") == 1, f"{what} was refused: {reply!r}"
Issue-Key: docdb.mongo-transaction-writes-stay-invisible-until-commit-and-an-abort-discards-them|_ok#8|AssertionError|assert reply.get("ok") == 1, f"{what} was refused: {reply!r}"
Issue-Commit: a11c67ddfc7199c8950e9df279eea2cbb1f1bbbc

# docdb: 4 net-new causes in the MongoDB data plane — $group accumulators over expressions sum to 0, $project cannot rename a field path, modifyChangeStreams is CommandNotFound, and multi-document transactions are refused

> _Note: this issue was identified against the code base at commit `a11c67ddfc7199c8950e9df279eea2cbb1f1bbbc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
aggregate

## Description
Four distinct root causes in the emulator's native MongoDB (DocumentDB) data plane, all reached over the MongoDB wire protocol on the cluster writer endpoint. Each cause is independent and must be closed separately.

Cause 1 of 4: a $group accumulator whose operand is an expression evaluates to the accumulator's identity instead of the computed value.
AWS: recorded behavior only (native MongoDB wire protocol; the DocumentDB API has no Smithy model for the data plane). DocumentDB's aggregation framework evaluates an accumulator's operand expression per input document before accumulating, so {"$sum": {"$multiply": ["$qty", "$price"]}} in a $group sums qty*price over each group's documents and returns a non-zero numeric total for every group that has numeric qty and price. In the recording, the paid orders grouped by region yield revenue 85 (west), 70 (east) and 10 (north).
Emulator: every group's accumulated value is 0 — the reply is [{'revenue': 0, 'orders': 2, 'ids': [1, 5]}, {'revenue': 0, 'orders': 2, 'ids': [2, 6]}, {'revenue': 0, 'orders': 1, 'ids': [4]}]. The constant-operand form {"$sum": 1} and {"$push": "$_id"} are correct (orders and ids match), so only the expression-operand path is broken: the operand expression is never evaluated and contributes nothing. A consequence is that the following {"$sort": {"revenue": -1}} sorts a column of equal zeros, so the groups come back in insertion order (east, west, north) rather than by descending revenue; fixing the accumulator restores the ordering, it is not a separate sort defect.
Why: any client that aggregates a derived measure — revenue, totals, weighted counts, byte sums — reads 0 for every group and any ranking/top-N built on that measure is arbitrary. The failure is silent: ok:1 with well-formed rows, so no client-side error surfaces and dashboards/reports simply show zeros.
Fix: the aggregation pipeline's $group stage in the docdb native data plane — the accumulator implementation must evaluate its operand through the same expression evaluator used by $project/$match instead of only accepting a literal or a bare field path.
Traces: docdb.mongo-aggregation-pipeline-groups-sums-and-sorts-the-matched-documents.

Cause 2 of 4: a $project stage that assigns a field path to a new name emits no such field.
AWS: recorded behavior only. In DocumentDB, {"$project": {"_id": 0, "region": "$_id", "revenue": 1, "orders": 1, "ids": 1}} suppresses _id and emits a field named region carrying the value of the input document's _id, so each output row has exactly the keys region, revenue, orders and ids.
Emulator: the output rows carry only revenue, orders and ids — the renamed field is absent entirely (no key, not a null). The inclusion flags (revenue/orders/ids: 1) and the _id: 0 suppression are honoured, so only the computed-field form "<new name>": "<field path>" is dropped.
Why: the standard rename/reshape idiom — the one every $group result uses to turn the grouping key _id back into a named field — silently loses data. A client indexes row["region"] and gets a KeyError, or serializes rows that are missing the only field identifying the group.
Fix: the $project stage of the aggregation pipeline in the docdb native data plane — a member whose value is a field-path or expression must be evaluated and emitted under its new name, not treated as an unrecognized projection spec and skipped.
Traces: docdb.mongo-aggregation-pipeline-groups-sums-and-sorts-the-matched-documents (the same check reaches this with the grouping key; the $unwind + $count assertion after it was never reached and is not claimed here).

Cause 3 of 4: the modifyChangeStreams admin command is not implemented, so change streams cannot be enabled.
AWS: recorded behavior only. DocumentDB requires change streams to be turned on per database/collection with {"modifyChangeStreams": 1, "database": <db>, "collection": <coll>, "enable": true} against the admin database, and answers {"ok": 1}. This is the documented prerequisite for opening a $changeStream aggregation.
Emulator: the command is refused with {'ok': 0.0, 'errmsg': "no such command: 'modifyChangeStreams'", 'code': 59, 'codeName': 'CommandNotFound'}. Fixed text of that message is "no such command: '<command name>'"; the quoted command name is the variable part taken from the request's first key. Required instead: ok:1 with the change stream enabled for the named database/collection, and a subsequent aggregate with a $changeStream stage returning an open (non-zero) cursor id whose getMore batches deliver insert/update/delete events in commit order, each with operationType, documentKey and — for fullDocument: "updateLookup" — the looked-up document.
Why: a CDC client cannot enable change streams at all, so the entire change-data-capture capability is unreachable; the CommandNotFound also misreports the cause as an unknown command rather than an unsupported feature, so a client cannot distinguish "wrong name" from "not enabled".
Fix: the MongoDB command dispatcher of the docdb native data plane must register modifyChangeStreams (admin database) and record the per-collection enable flag in the data-plane state the $changeStream aggregation stage reads.
Traces: docdb.mongo-change-stream-delivers-insert-update-and-delete-events-in-commit-order.

Cause 4 of 4: multi-document transactions are refused outright.
AWS: recorded behavior only. DocumentDB supports multi-document transactions: a command carrying lsid, txnNumber, autocommit: false and startTransaction: true opens a transaction and returns ok:1; writes inside it are visible to reads in the same session but not to reads outside it; commitTransaction makes them all visible at once; abortTransaction discards them; and committing an already-aborted txnNumber is refused with NoSuchTransaction (code 251).
Emulator: the very first transactional write is refused with {'ok': 0.0, 'errmsg': 'this endpoint does not support multi-document transactions', 'code': 20, 'codeName': 'IllegalOperation'} — a blanket refusal of the session/transaction fields, not a per-case error. Fixed text is the whole message 'this endpoint does not support multi-document transactions' (it carries no variable parts), and it is not a message DocumentDB produces for a supported cluster.
Why: every client using a session with a transaction — the standard pattern for a transfer/ledger update — fails on its first statement, and the IllegalOperation (20) it gets is not a retryable or classifiable transaction error, so driver-level retry logic cannot recover. Read-isolation and rollback semantics are untestable.
Fix: the docdb native data-plane command handling for sessions and transactions — honour lsid/txnNumber/autocommit/startTransaction by buffering the session's writes in transaction state, serve reads in that session from the buffer, and implement commitTransaction/abortTransaction (including the NoSuchTransaction 251 refusal for a committed/aborted txnNumber).
Traces: docdb.mongo-transaction-writes-stay-invisible-until-commit-and-an-abort-discards-them.

Note on already-open issues: several open docdb issues (20261109T120625Z-docdb-cluster-instance-endpoints-are-published-as-real.md, 20261112T142502Z-docdb-cluster-endpoint-and-reader-endpoint-are-published.md, 20261115T142542Z-docdb-dbinstance-endpoint-address-is-published-as-a.md) concern advertised endpoint hostnames. They do not cover any cause here: these three checks did reach the MongoDB data plane and received protocol-level replies, so the defects are in command handling, not in endpoint publication.

## Evidence
Cause 1 — docdb.mongo-aggregation-pipeline-groups-sums-and-sorts-the-matched-documents:
  pipeline: [{$match:{status:'paid'}}, {$group:{_id:'$region', revenue:{$sum:{$multiply:['$qty','$price']}}, orders:{$sum:1}, ids:{$push:'$_id'}}}, {$sort:{revenue:-1}}, {$project:{_id:0, region:'$_id', revenue:1, orders:1, ids:1}}]
  Expected: [{'region':'west','revenue':85,'orders':2,'ids':[2,6]}, {'region':'east','revenue':70,'orders':2,'ids':[1,5]}, {'region':'north','revenue':10,'orders':1,'ids':[4]}]
  Actual:   [{'revenue':0,'orders':2,'ids':[1,5]}, {'revenue':0,'orders':2,'ids':[2,6]}, {'revenue':0,'orders':1,'ids':[4]}]
  -> every expression-operand $sum is 0; $sum:1 and $push are correct.

Cause 2 — same check, same reply: the rows carry no 'region' key at all, while 'revenue'/'orders'/'ids' (inclusion flags) and the '_id':0 suppression are honoured. Group order also follows input order (east, west, north) rather than descending revenue, which is downstream of Cause 1.

Cause 3 — docdb.mongo-change-stream-delivers-insert-update-and-delete-events-in-commit-order:
  request: {"modifyChangeStreams":1, "database":"sim2real", "collection":"events", "enable":true, "$db":"admin"}
  Expected: {'ok': 1}
  Actual:   {'ok': 0.0, 'errmsg': "no such command: 'modifyChangeStreams'", 'code': 59, 'codeName': 'CommandNotFound'}

Cause 4 — docdb.mongo-transaction-writes-stay-invisible-until-commit-and-an-abort-discards-them:
  request: {"update":"ledger", "updates":[{"q":{"_id":"acct-a"},"u":{"$inc":{"balance":-30}}}], "lsid":{...}, "txnNumber":1, "autocommit":false, "startTransaction":true, "$db":"sim2real"}
  Expected: {'ok': 1} (and subsequently: in-session read sees balance 70 plus acct-b; outside read still sees balance 100; commitTransaction makes both visible; abortTransaction discards; re-commit of the aborted txnNumber -> code 251 NoSuchTransaction)
  Actual:   {'ok': 0.0, 'errmsg': 'this endpoint does not support multi-document transactions', 'code': 20, 'codeName': 'IllegalOperation'}

## Steps to Reproduce
1. Boot the emulator and create/resolve a DocDB cluster writer endpoint reachable over the MongoDB wire protocol; authenticate as the cluster admin user.
2. Cause 1 and 2: insert documents with numeric qty/price and a string region, then run aggregate with pipeline [{$match:{status:'paid'}}, {$group:{_id:'$region', revenue:{$sum:{$multiply:['$qty','$price']}}, orders:{$sum:1}, ids:{$push:'$_id'}}}, {$sort:{revenue:-1}}, {$project:{_id:0, region:'$_id', revenue:1, orders:1, ids:1}}]. Observe revenue 0 in every row and no 'region' key.
3. Cause 3: send {"modifyChangeStreams":1, "database":"<db>", "collection":"<coll>", "enable":true, "$db":"admin"}. Observe code 59 CommandNotFound.
4. Cause 4: send any write command with lsid, txnNumber, autocommit:false and startTransaction:true. Observe code 20 IllegalOperation.
5. Re-replay: docdb.mongo-aggregation-pipeline-groups-sums-and-sorts-the-matched-documents, docdb.mongo-change-stream-delivers-insert-update-and-delete-events-in-commit-order, docdb.mongo-transaction-writes-stay-invisible-until-commit-and-an-abort-discards-them.
