Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__dynamodb.appsync-batch-get-item-resolver-preserves-arity-and-appends-per-key-errors appsync__dynamodb.appsync-ddb-query-resolver-malformed-next-token-rejected
Issue-Key: appsync__dynamodb.appsync-batch-get-item-resolver-preserves-arity-and-appends-per-key-errors|run#25|AssertionError|assert len(items) == FANOUT, (
Issue-Key: appsync__dynamodb.appsync-ddb-query-resolver-malformed-next-token-rejected|run#17|AssertionError|assert error["errorType"] == "IllegalArgument", f"unexpected errorType {error['errorType']!r}"
Issue-Commit: a3c668f6bb53d78c1d1232fe39d40072881572f4

# appsync__dynamodb: 2 net-new causes — a BatchGetItem resolver compacts the result list instead of returning one slot per requested key, and a malformed nextToken is refused with errorType 'Code' instead of 'IllegalArgument'

> _Note: this issue was identified against the code base at commit `a3c668f6bb53d78c1d1232fe39d40072881572f4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetItem

## Description
Two net-new root causes in the AppSync GraphQL data plane over an AMAZON_DYNAMODB data source. Both are newly observable because the data plane now answers resolver requests at all: the open issue 20261114T140817Z-appsync-dynamodb-every-dynamodb-data-source-resolver-check.md (every DynamoDB-data-source resolver check hangs) does not cover either verdict, and 20261116T135132Z-appsync-2-net-new-causes-a-graphql-error.md (util.error carries no errorType) is a different defect — here an errorType IS emitted, with the wrong value. Each cause needs its own fix.

Cause 1 of 2: a DynamoDB `BatchGetItem` resolver operation returns a COMPACTED list, losing one slot per absent key.
AWS: recorded behavior only — AppSync's GraphQL data plane is not described by the appsync Smithy model (the model covers the control plane; CreateResolver/runtime APPSYNC_JS 1.0.0 are modeled, the invoke path is not). AWS's recorded contract: for a `{operation: 'BatchGetItem', tables: {<table>: {keys: [...]}}}` request, `ctx.result.data[<table>]` is a list of the SAME LENGTH and SAME ORDER as the requested keys, with `null` at each position whose key was not found, and `ctx.result.unprocessedKeys[<table>]` is an empty list when the request was fully served. A missing key is a hole, not a dropped element and not a whole-request error; the envelope is HTTP 200 with partial `data`.
Emulator: for five requested keys of which three are seeded, `data.batchLookup` comes back with 3 elements (the found items only), so the response function never sees a `null` at index 1 or 3, `util.appendError` is never called for a hole, and the per-key error entries (errorType, message naming the absent key, errorInfo.index) AWS would produce are all absent. The all-present control batch of 3 keys is served correctly, so the resolver itself runs — only the arity/ordering of the per-table result is wrong.
Why: every client that correlates the result list positionally with the keys it asked for silently misaligns — item i in the response is attributed to key i of the request, so values are read against the wrong ids, and a per-key "not found" is indistinguishable from a shifted hit. Nothing in the envelope reveals the shift.
Fix: the AppSync resolver-invocation path's DynamoDB `BatchGetItem` arm — build `ctx.result.data[<table>]` by mapping over the REQUESTED key list (null for an unfound key) rather than from the set of returned items, and set `unprocessedKeys[<table>]` to an empty list when nothing was throttled.
Traces: appsync__dynamodb.appsync-batch-get-item-resolver-preserves-arity-and-appends-per-key-errors.

Cause 2 of 2: a DynamoDB `Query` resolver refuses a nextToken AppSync never minted with the wrong errorType.
AWS: recorded behavior only (data plane, unmodeled). A `nextToken` that is not one the API minted is refused inside a 200 GraphQL envelope with `errors[0].errorType` = `IllegalArgument` and `errors[0].message` = `Invalid pagination token given.` (fixed text, no variable part), and `data.listItems` is null. The identical call with no token still serves the page.
Emulator: the token IS refused (an errors entry is present) but `errors[0].errorType` is `Code`. Hypothesis: the serializer is emitting the KEY name of the underlying error envelope (`Code`) as the errorType instead of the error's classification, so no AppSync errorType value can ever be served from this path.
Why: clients and client libraries branch on errorType to tell a malformed-pagination argument (do not retry, drop the token) from a transient failure (retry); `Code` matches no AppSync errorType, so the refusal is unclassifiable and pagination loops cannot recover.
Fix: the GraphQL error serializer for data-plane resolver refusals — map the pagination-token refusal to errorType `IllegalArgument` with AWS's fixed message, and stop using the error document's member name as the errorType.
Traces: appsync__dynamodb.appsync-ddb-query-resolver-malformed-next-token-rejected.

## Evidence
Verbatim from the replay:

Cause 1 — appsync__dynamodb.appsync-batch-get-item-resolver-preserves-arity-and-appends-per-key-errors:
  AssertionError: BatchGetItem must return one slot per REQUESTED key: expected 5 slots for 5 keys (3 present, 2 absent), got 3 -- a compacted list silently misaligns every client that indexes the result: [{'id': 'k0-…', 'value': 'seeded-0'}, {'id': 'k2-…', 'value': 'seeded-2'}, {'id': 'k4-…', 'value': 'seeded-4'}]
  (The generated key names and the 5/3/2 fan-out are this run's request values; the requirement is len(result) == len(requested keys), null at each unfound position, request order preserved, plus one appended error per hole carrying errorInfo.index and errors[].path == ["<fieldName>"].) The all-present 3-key control passed, so the resolver executed and read the seeded items correctly.

Cause 2 — appsync__dynamodb.appsync-ddb-query-resolver-malformed-next-token-rejected:
  AssertionError: unexpected errorType 'Code'
  Expected errors[0].errorType == 'IllegalArgument' and errors[0].message == 'Invalid pagination token given.' with data.listItems == null; got errorType 'Code'. The message was never reached by the check because the errorType assertion fires first — 'Invalid pagination token given.' is AWS's fixed text with no variable part.

## Steps to Reproduce
Cause 1:
1. Create a PAY_PER_REQUEST DynamoDB table with one STRING hash key `id`; create an IAM role trusted by appsync.amazonaws.com allowing dynamodb:BatchGetItem/GetItem on it.
2. Create an API_KEY GraphQL API with `type Query { batchLookup(ids: [ID!]!): [Item] }`, an AMAZON_DYNAMODB data source on that table, and a UNIT resolver (runtime APPSYNC_JS 1.0.0) whose request returns `{operation:'BatchGetItem', tables:{'<table>':{keys: ctx.args.ids.map(id => util.dynamodb.toMapValues({id}))}}}` and whose response calls util.appendError once per null slot and returns the list.
3. Seed 3 of 5 ids. Query with all 5 ids: `data.batchLookup` must have 5 entries, null at the 2 unseeded positions, in request order, with exactly 2 appended errors (errorInfo.index = the hole positions, path = ["batchLookup"]) and no unprocessedKeys.

Cause 2:
1. Create a table with pk/sk, put one item, and a GraphQL API whose `Query.listItems` UNIT resolver issues a DynamoDB `Query` with `nextToken` taken from the field argument.
2. Run the query with `nextToken: "not-a-real-token"`: the response must be HTTP 200 with data.listItems null and errors[0] = {errorType: 'IllegalArgument', message: 'Invalid pagination token given.'}.
3. Re-run the identical query with no token: it must serve the page.
