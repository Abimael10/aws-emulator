Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agent__lambda__opensearchserverless.lambda-opensearchserverless-list-collections-missing-permission-negative bedrock-agent__lambda__opensearchserverless.cli-opensearchserverless-collection-missing-permission-negative bedrock-agent__lambda__opensearchserverless.lambda-opensearchserverless-create-collection-missing-permission-negative
Issue-Key: bedrock-agent__lambda__opensearchserverless.lambda-opensearchserverless-list-collections-missing-permission-negative|run#9|AssertionError|assert bad_result["ok"] is False, bad_result
Issue-Key: bedrock-agent__lambda__opensearchserverless.cli-opensearchserverless-collection-missing-permission-negative|run#14|AssertionError|assert neg_meta.get("FunctionError") == "Unhandled", f"expected an unhandled error, got: {neg_meta}"
Issue-Key: bedrock-agent__lambda__opensearchserverless.lambda-opensearchserverless-create-collection-missing-permission-negative|run#7|AssertionError|assert bad_result["ok"] is False, bad_result
Issue-Commit: ba86a3055ff1a6763001a85bf7b40400defa0252

# bedrock-agent__lambda__opensearchserverless: 2 net-new causes — aoss control-plane calls are never authorized against the caller's IAM identity policy (ListCollections and CreateCollection both succeed for a role with no aoss permissions)

> _Note: this issue was identified against the code base at commit `ba86a3055ff1a6763001a85bf7b40400defa0252`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListCollections

## Description
The emulator executes OpenSearch Serverless control-plane operations without evaluating the SigV4 caller's IAM identity-based policy. Three permission-boundary checks drive the SAME request from a Lambda execution role that deliberately carries NO aoss actions, and every one of them is served a 200 success instead of being refused. Each check also runs an otherwise-identical positive-control role that DOES carry the action, so the only difference between the two legs is the identity policy — the emulator makes no distinction, which means authorization is simply not wired for the aoss control plane.

Two distinct operations must each be closed:

Cause 1 of 2: ListCollections is served to a principal whose role grants no aoss action.
AWS: aoss API calls are authorized by the caller's identity-based policy. A principal whose role policy omits aoss:ListCollections is refused with HTTP 403 and error code AccessDeniedException; no collectionSummaries payload is returned. The message is AWS's standard identity-policy denial, whose fixed text is "User: <caller-arn> is not authorized to perform: <action> because no identity-based policy allows the <action> action" — the variable parts are the caller ARN taken from the request's SigV4 credentials (for a Lambda, the assumed-role ARN arn:aws:sts::<account>:assumed-role/<role-name>/<function-name>) and the action name (aoss:ListCollections); the account id, role name, function name and region in any one recording are values from that run, not part of the template. Smithy (com-amazonaws-opensearchserverless-opensearchserverless.json): ListCollections models only ValidationException (400) and InternalServerException (500) — AccessDeniedException is the protocol's framework-level authorization error and is not in the operation's error list, so its status/code here are recorded behavior only, taken from the refusal the recording expects (AccessDeniedException).
Emulator: the call returns 200 with a normal ListCollectionsResponse. In the in-Lambda SDK leg the handler gets {'ok': True, 'result': {'collectionSummaries': []}}; in the CLI-provisioned-role leg the Invoke comes back {'StatusCode': 200, 'ExecutedVersion': '$LATEST'} with no FunctionError at all, i.e. the handler's list_collections() never raised.
Why: a client cannot test or rely on least-privilege. Any policy audit, permission-boundary test or IAM regression suite passes vacuously against the emulator and then fails on real AWS; conversely code that is supposed to handle AccessDeniedException is never exercised, and a listing leaks resource names to a principal AWS would refuse.
Fix: the aoss service's request-authorization layer — the pre-dispatch identity-policy evaluation that should run for every aoss operation before ListCollections' handler builds a response (and the serializer path that renders the 403 AccessDeniedException envelope).
Traces: bedrock-agent__lambda__opensearchserverless.lambda-opensearchserverless-list-collections-missing-permission-negative, bedrock-agent__lambda__opensearchserverless.cli-opensearchserverless-collection-missing-permission-negative.

Cause 2 of 2: CreateCollection is served — and actually provisions a collection — for a principal whose role grants no aoss action.
AWS: a principal whose identity policy omits aoss:CreateCollection is refused with HTTP 403 AccessDeniedException, the response carries no createCollectionDetail, and NOTHING is provisioned (a subsequent ListCollections filtered on that name returns an empty collectionSummaries). The message is the same identity-policy denial template as Cause 1 with the action aoss:CreateCollection substituted. Smithy: CreateCollection models ConflictException (409), OcuLimitExceededException (402), ServiceQuotaExceededException (402), ValidationException (400), InternalServerException (500); AccessDeniedException is again the framework authorization error, so its status/code are recorded behavior only.
Emulator: the call succeeds and returns a full createCollectionDetail (id, name, status CREATING, type SEARCH, arn, kmsKeyArn, standbyReplicas, deletionProtection, createdDate, lastModifiedDate), so the denied caller both gets a result payload and leaves a real collection behind in state. The concrete ids/names/timestamps in the replay are per-run values; the defect is that a success envelope is produced at all.
Why: an unauthorized caller can create billable, state-changing resources. Least-privilege enforcement for provisioning is unobservable, and the collection left behind diverges the emulator's state from what AWS would hold after the same call sequence.
Fix: the same aoss request-authorization layer, applied to CreateCollection before its handler mutates state — the denial must happen ahead of the state write, not be rolled back after it.
Traces: bedrock-agent__lambda__opensearchserverless.lambda-opensearchserverless-create-collection-missing-permission-negative.

Related but NOT the same as the open issue 20261108T223024Z-opensearchserverless-3-net-new-causes-on-the-collection.md, which is about aoss DATA-access-policy permissions on the collection data plane (aoss:WriteDocument against an index endpoint). This issue is about identity-based IAM policy evaluation on the aoss CONTROL plane (ListCollections / CreateCollection), a separate gate. No open bedrock-agent__lambda__opensearchserverless issue covers it.

## Evidence
Check lambda-opensearchserverless-list-collections-missing-permission-negative (mismatch):
  assert bad_result["ok"] is False
  AssertionError: {'ok': True, 'result': {'collectionSummaries': []}}
  Expected: ok False with error_code == "AccessDeniedException" and NO 'result' key, for a Lambda role created with an empty policy list (no aoss actions). The positive-control role granting aoss:ListCollections passed.

Check cli-opensearchserverless-collection-missing-permission-negative (mismatch):
  assert neg_meta.get("FunctionError") == "Unhandled"
  AssertionError: expected an unhandled error, got: {'StatusCode': 200, 'ExecutedVersion': '$LATEST'}
  The negative role's CLI-attached inline policy granted only logs:CreateLogGroup / logs:CreateLogStream / logs:PutLogEvents; the handler's client.list_collections() should have raised AccessDeniedException and faulted the invocation Unhandled. It returned cleanly instead.

Check lambda-opensearchserverless-create-collection-missing-permission-negative (mismatch):
  assert bad_result["ok"] is False
  AssertionError: {'ok': True, 'result': {'createCollectionDetail': {'id': 'w9qcpy0x3cxgkv4kouev', 'name': 'sim2real-bp-91f1d7cc9ff1', 'status': 'CREATING', 'type': 'SEARCH', 'arn': 'arn:aws:aoss:us-west-2:000000000000:collection/w9qcpy0x3cxgkv4kouev', 'kmsKeyArn': 'auto', 'standbyReplicas': 'ENABLED', 'deletionProtection': 'DISABLED', 'createdDate': 1794691251684, 'lastModifiedDate': 1794691251684}}}
  Expected: ok False, error_code "AccessDeniedException", no 'result' payload, and a following ListCollections(collectionFilters={'name': <bad_name>}) returning collectionSummaries == []. The ids, name, arn account/region and epoch timestamps above are per-run values, not requirements — the requirement is that no success envelope and no collection exist.

Expected vs actual, summarized:
  expected: HTTP 403, Error.Code AccessDeniedException, message "User: <assumed-role-arn> is not authorized to perform: aoss:<Action> because no identity-based policy allows the aoss:<Action> action", no result payload, no state change
  actual:   HTTP 200 with the normal success payload, and for CreateCollection a collection persisted in state

## Steps to Reproduce
1. Create an IAM role trusted by lambda.amazonaws.com with NO aoss actions (either an empty inline policy set, or an inline policy granting only logs:CreateLogGroup/CreateLogStream/PutLogEvents).
2. Create a Lambda function with that role whose handler builds boto3.client('opensearchserverless') and calls list_collections(), and a second one that calls create_collection(name=<unique a-z0-9- name>, type='SEARCH', encryptionConfig={'aWSOwnedKey': True}). Wait for each function to go Active.
3. Invoke the list_collections function. Observed: StatusCode 200, no FunctionError, body {'collectionSummaries': []}. Expected: the handler's call refused with 403 AccessDeniedException (and, when unhandled, the invocation reported FunctionError 'Unhandled').
4. Invoke the create_collection function. Observed: a full createCollectionDetail success payload. Expected: 403 AccessDeniedException, no payload.
5. From the harness credentials call ListCollections(collectionFilters={'name': <that name>}). Observed: the collection the denied call created is listed. Expected: collectionSummaries == [].
6. Repeat steps 1-4 with the same role plus Allow on aoss:ListCollections / aoss:CreateCollection (the positive controls): these already succeed, confirming the only unmodeled difference is that the identity policy is never consulted.
