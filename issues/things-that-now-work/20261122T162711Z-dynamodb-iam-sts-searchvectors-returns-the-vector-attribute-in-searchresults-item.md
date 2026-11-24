Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__iam__sts.search-vectors-table-read-role-denied-index-arn-grant-allowed
Issue-Key: dynamodb__iam__sts.search-vectors-table-read-role-denied-index-arn-grant-allowed|run#39|AssertionError|assert results[0]["Item"] == {"pk": {"S": "item-1"}}, (
Issue-Commit: 8b5969fac7c1c7431c4928bdfe595aafe4e06568

# SearchVectors returns the vector attribute in SearchResults[].Item instead of only the projected non-vector attributes

> _Note: this issue was identified against the code base at commit `8b5969fac7c1c7431c4928bdfe595aafe4e06568`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SearchVectors

## Description
Cause 1 of 1: DynamoDB `SearchVectors` over-reports the attributes of each search result, returning the item's vector attribute alongside the key attributes.

AWS: Smithy (services/service-com-amazonaws-dynamodb-dynamodb-20120810/smithy-model/com-amazonaws-dynamodb-dynamodb-20120810.json) models SearchVectorsOutput.SearchResults as a `SearchResultList` of `SearchResultItem`, whose `Item` member is documented as "A map of attribute names to AttributeValue objects, representing the **projected attributes** of the item returned by the vector search", and SearchVectorsInput.ProjectionExpression as "If not specified, the operation returns all attributes projected into the vector index... Only attributes projected into the vector index can be retrieved." The model does not itself enumerate which attributes a vector index projects, so beyond that this is recorded behavior only: against real AWS, a table whose only non-key attribute is the vector attribute named by the index's `VectorAttribute`, with `Projection: {ProjectionType: ALL}`, answers a SearchVectors (no ProjectionExpression) with `Item` containing the KEY attributes only — the vector attribute itself is not returned. Generally: the vector attribute an index is built on is not part of the attribute set SearchVectors projects back, so it must be omitted from every `SearchResults[].Item` unless a ProjectionExpression names it. The call is a 200; no error code is involved.

Emulator: the same request returns `Item` = `{'pk': {'S': 'item-1'}, 'embedding': {'L': [{'N': '0.1'}, {'N': '0.2'}, {'N': '0.3'}]}}` — the stored item verbatim, including the `embedding` vector attribute the index was created on — where AWS returns `{'pk': {'S': 'item-1'}}`. Everything before this in the check passes: CreateTable accepts VectorIndexes, the index reaches ACTIVE, the index ARN is published, and the SearchVectors authorization behaviour (AccessDeniedException naming `dynamodb:SearchVectors` on the index ARN for the role without the grant, success for the role granted it on the index ARN) is correct. The sole divergence is the content of the returned item.

Why: a client that round-trips search results back into its own store, hashes them, or sizes its response on them sees an extra, potentially very large attribute per hit that AWS never sends. Code that treats `Item` as "the projected attributes" (e.g. re-embedding only when `embedding` is absent, or asserting the vector is not echoed back for cost/bandwidth reasons) behaves differently against the emulator than against AWS, and payload sizes and ConsumedCapacity estimates derived from the results are inflated.

Fix: the SearchVectors handler's result projection — the step that builds each `SearchResultItem.Item` from the stored item must apply the vector index's projection and exclude the index's `VectorAttribute` attribute name (honouring `ProjectionExpression` when supplied), rather than cloning the stored item.

Traces: failing check `dynamodb__iam__sts.search-vectors-table-read-role-denied-index-arn-grant-allowed`; the assertion is at check line 222 (`assert results[0]["Item"] == {"pk": {"S": "item-1"}}`). Handler: services/service-com-amazonaws-dynamodb-dynamodb-20120810/src/ops/search_vectors.rs.

## Evidence
Check: dynamodb__iam__sts.search-vectors-table-read-role-denied-index-arn-grant-allowed [mismatch]

Setup: CreateTable with KeySchema pk(HASH), AttributeDefinitions pk(S), PAY_PER_REQUEST, VectorIndexes=[{IndexName: 'vec-idx', VectorAttribute: {AttributeName: 'embedding'}, Dimensions: 3, DistanceFunction: COSINE, Projection: {ProjectionType: ALL}}]; PutItem Item={'pk': {'S':'item-1'}, 'embedding': {'L':[0.1,0.2,0.3]}}.
Call: SearchVectors(TableName=<table>, IndexName='vec-idx', SearchVector=[0.1,0.2,0.3], TopK=1) under a role granted dynamodb:SearchVectors on the vector index ARN.

Expected (AWS): HTTP 200, SearchResults length 1, SearchResults[0].Item == {'pk': {'S': 'item-1'}}  (key attributes only; the index's vector attribute is not projected back).

Actual (emulator): HTTP 200, SearchResults length 1, SearchResults[0].Item == {'pk': {'S': 'item-1'}, 'embedding': {'L': [{'N': '0.1'}, {'N': '0.2'}, {'N': '0.3'}]}}

AssertionError: SearchVectors returned an unexpected item: {'pk': {'S': 'item-1'}, 'embedding': {'L': [{'N': '0.1'}, {'N': '0.2'}, {'N': '0.3'}]}}

Note: the per-run values ('item-1', the unique table name, the 3-element vector) are the request's own values filling the template; the requirement is that the attribute named by the index's VectorAttribute is absent from Item.

## Steps to Reproduce
1. CreateTable with a single HASH key `pk` (S), BillingMode PAY_PER_REQUEST, and VectorIndexes=[{IndexName:'vec-idx', VectorAttribute:{AttributeName:'embedding'}, Dimensions:3, DistanceFunction:'COSINE', Projection:{ProjectionType:'ALL'}}]; wait for the table and for VectorIndexes[0].IndexStatus == ACTIVE.
2. PutItem Item={'pk':{'S':'item-1'},'embedding':{'L':[{'N':'0.1'},{'N':'0.2'},{'N':'0.3'}]}}.
3. Create an IAM role trusted by the account root with an inline policy allowing dynamodb:GetItem/Query/Scan on the table ARN plus dynamodb:SearchVectors on the vector index ARN; sts:AssumeRole it.
4. As that session call SearchVectors(TableName=<table>, IndexName='vec-idx', SearchVector=[{'N':'0.1'},{'N':'0.2'},{'N':'0.3'}], TopK=1).
5. Inspect SearchResults[0]['Item']: the emulator includes the 'embedding' attribute; AWS returns only {'pk': {'S':'item-1'}}.
