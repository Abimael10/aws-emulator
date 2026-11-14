Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena.athena-batch-get-named-query-limit-exceeded-rejected athena.batch-get-named-query-exceeds-limit-rejected athena.data-catalog-type-mismatch-update-rejected athena.data-catalog-update-type-immutable-rejected athena.op-contract-update-data-catalog-rejects-type-mismatch athena.op-contract-getNamedQuery athena.workgroup-delete-conflict-rejected athena.deploy-named-query-cloudformation-retrievable-and-removed-on-stack-delete athena.list-named-queries-invalid-next-token-rejected
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena|BatchGetNamedQuery|ValidationException
Issue-Key: athena.batch-get-named-query-exceeds-limit-rejected|run#6|AssertionError|assert code == "InvalidRequestException", (
Issue-Key: athena.data-catalog-type-mismatch-update-rejected|run#12|AssertionError|assert message == "Lambda catalogs require that either the metadata-function and record-function parameters or the function parameter be set, but not both.", (
Issue-Key: athena.data-catalog-update-type-immutable-rejected|run#9|AssertionError|assert message == "Lambda catalogs require that either the metadata-function and record-function parameters or the function parameter be set, but not both.", (
Issue-Key: athena.op-contract-update-data-catalog-rejects-type-mismatch|run#17|AssertionError|assert message == expected_message, f"expected {expected_message!r}, got {message!r}"
Issue-Key: athena.op-contract-getNamedQuery|run#18|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: athena|GetNamedQuery|InvalidRequestException
Issue-Key: athena.list-named-queries-invalid-next-token-rejected|run#9|AssertionError|assert code == "InvalidRequestException", f"expected InvalidRequestException, got {code!r}"
Issue-Commit: a6c2e348e539d87c7f8cd286303d9f9234b5e1b8

# athena: 6 net-new causes — BatchGetNamedQuery over-limit refusal is a ValidationException, UpdateDataCatalog LAMBDA-parameter and GetNamedQuery not-found messages are emulator-authored, a recursive workgroup delete cascade-deletes named queries, CFN AWS::Athena::NamedQuery yields no retrievable query, and ListNamedQueries answers NotImplemented for a malformed NextToken

> _Note: this issue was identified against the code base at commit `a6c2e348e539d87c7f8cd286303d9f9234b5e1b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetNamedQuery

## Description
Six independent root causes on the Athena control plane, all grounded in failing behavioral checks that no open issue carries. They share one service and are bundled here; a fixer must close all six before submitting.

Cause 1 of 6: BatchGetNamedQuery with more than 50 NamedQueryIds is refused with an unmodeled error code.
AWS: Smithy (services/service-com-amazonaws-athena-amazonathena/smithy-model): BatchGetNamedQuery input NamedQueryIds is required with @length min 1 / max 50, and the operation binds only InternalServerException (500) and InvalidRequestException (400) — no ValidationException. 51 ids are refused outright, with no partial processing, as HTTP 400 InvalidRequestException whose Message is "1 validation error detected: Value at 'namedQueryIds' failed to satisfy constraint: Member must have length less than or equal to 50"; the fixed text is everything but the member name ('namedQueryIds') and the limit (50), both taken from the model. At exactly 50 ids the call succeeds: the resolvable id comes back in NamedQueries and the rest in UnprocessedNamedQueryIds.
Emulator: Code 'ValidationException' (a code bound to no Athena operation), so the modeled refusal and its documented member/limit wording are both lost.
Why: a client catching Athena's InvalidRequestException sees an unknown error code and cannot classify an over-limit batch at all, and a client reading the message gets no member or limit to repair the request with.
Fix: BatchGetNamedQuery's input validation in the athena crate plus the shared Athena error classifier that chooses the exception shape.
Note: the open issue 20261104T163600Z-athena-constraint-refusals-raise-an-unmodeled-validationexception-instead.md describes this same classification defect as its Cause 4, but it carries neither of the two check keys below, so neither check is covered by it.
Traces: athena.athena-batch-get-named-query-limit-exceeded-rejected, athena.batch-get-named-query-exceeds-limit-rejected.

Cause 2 of 6: UpdateDataCatalog switching a catalog to Type=LAMBDA with only one of the required parameter keys is refused with the wrong sentence.
AWS: Smithy: UpdateDataCatalog input Name/Type required, Parameters is a ParametersMap; the operation binds only InternalServerException and InvalidRequestException (400). A LAMBDA catalog takes either the pair 'metadata-function' + 'record-function' or the single 'function' key; supplying 'metadata-function' alone is refused HTTP 400 InvalidRequestException with the wholly fixed Message "Lambda catalogs require that either the metadata-function and record-function parameters or the function parameter be set, but not both." — no request-derived part. The catalog's stored Type, Description and Parameters are left unchanged, and a same-Type update still succeeds.
Emulator: status 400 and Code InvalidRequestException are right, but Message is the emulator-authored "Parameters for a LAMBDA data catalog must contain either function or both metadata-function and record-function."
Why: the message is part of the error contract; a client or IaC tool that branches on AWS's sentence (or compares a recorded response) does not recognise the refusal and cannot auto-repair the parameter map.
Fix: the UpdateDataCatalog LAMBDA-parameter validation path in the athena crate (message text only).
Note: 20261104T163600Z-athena-conflict-not-found-and-output-location-refusals.md Cause 6 covers the DIFFERENT CreateDataCatalog rule for an EMPTY parameter map ('Lambda catalog parameters cannot be empty.') and carries none of the keys below; the missing-key rule on UpdateDataCatalog is not in it.
Traces: athena.data-catalog-type-mismatch-update-rejected, athena.data-catalog-update-type-immutable-rejected, athena.op-contract-update-data-catalog-rejects-type-mismatch.

Cause 3 of 6: GetNamedQuery's not-found refusal is re-worded.
AWS: Smithy: GetNamedQuery input NamedQueryId required (@length 1..128, @pattern ^\\S+$), errors InternalServerException and InvalidRequestException (400). A well-formed NamedQueryId that names no named query — whether it never existed or was just deleted — is refused HTTP 400 InvalidRequestException with a Message of the fixed form 'NamedQuery: ' + the NamedQueryId from the request + ' does not exist'.
Emulator: same status and code, but Message 'NamedQuery <id> was not found' — AWS's colon after 'NamedQuery' is dropped and 'does not exist' is replaced by 'was not found'.
Why: clients (and the emulator's own checks) that distinguish an absent named query by AWS's sentence mis-read every GetNamedQuery miss; the same wrong sentence surfaces wherever a named query is read, so the divergence is systematic, not incidental.
Fix: the GetNamedQuery not-found path in the athena crate (message text).
Traces: athena.op-contract-getNamedQuery.

Cause 4 of 6: DeleteWorkGroup with RecursiveDeleteOption=true destroys the workgroup's named queries.
AWS: recorded behavior only — Smithy documents RecursiveDeleteOption as 'the option to delete the workgroup and its contents even if the workgroup contains any named queries, query executions, or notebooks', and the recording shows that the option only PERMITS deleting a non-empty workgroup: after DeleteWorkGroup(WorkGroup=W, RecursiveDeleteOption=true), GetNamedQuery on a named query that was saved with WorkGroup=W still returns 200 with that query's NamedQueryId and WorkGroup=W. (The preceding legs agree with the emulator: a non-recursive delete of the non-empty workgroup is refused 400 InvalidRequestException 'WorkGroup <name> is not empty' and the workgroup survives, and an empty sibling workgroup deletes cleanly.)
Emulator: the recursive delete also removes the named queries created under that workgroup — the follow-up GetNamedQuery is refused 400 InvalidRequestException ('NamedQuery <id> was not found', see cause 3) for an id that must still resolve.
Why: a client that deletes a workgroup recursively silently loses saved queries it can still read on AWS; the loss is unrecoverable and invisible, since the delete reports success.
Fix: the DeleteWorkGroup handler's recursive-delete state transition in the athena crate — it must not cascade into named-query state.
Traces: athena.workgroup-delete-conflict-rejected.

Cause 5 of 6: a CloudFormation stack containing AWS::Athena::NamedQuery reports success but exposes no usable NamedQueryId and no retrievable named query.
AWS: recorded behavior only (CFN resource, not in the Athena Smithy model). A stack whose template declares AWS::Athena::NamedQuery with Name/Database/Description/QueryString reaches CREATE_COMPLETE, and !GetAtt <logical>.NamedQueryId resolves to the Athena-minted NamedQueryId (an opaque id, not the Name). GetNamedQuery on that id returns 200 with the Name, Database, QueryString and Description the template authored, the id appears in ListNamedQueries, and deleting the stack removes it from that listing.
Emulator: the stack reaches CREATE_COMPLETE and the NamedQueryId output is the template's Name value, and GetNamedQuery on that output is refused 400 InvalidRequestException reporting that identifier as not found — so nothing retrievable was created and the output is not a NamedQueryId at all.
Why: every CFN-driven Athena saved-query deployment is a false success: the stack says the query exists, the output cannot be used to read or delete it, and no drift is reported.
Fix: the CloudFormation provisioner's AWS::Athena::NamedQuery resource — create the named query through the Athena control plane and use the minted NamedQueryId as the resource's physical id and GetAtt NamedQueryId.
Note: the open issue 20261104T163600Z-athena-cloudformation-stacks-containing-aws-athena-workgroup-aws-athena.md reports the same class for AWS::Athena::WorkGroup and AWS::Athena::DataCatalog and 20261109T123922Z-athena-cloudformation-aws-athena-preparedstatement-is-not-a.md for AWS::Athena::PreparedStatement; neither covers AWS::Athena::NamedQuery or carries the key below.
Traces: athena.deploy-named-query-cloudformation-retrievable-and-removed-on-stack-delete.

Cause 6 of 6: ListNamedQueries never validates NextToken — the whole operation answers NotImplemented.
AWS: Smithy: ListNamedQueries is @paginated (inputToken/outputToken NextToken, pageSize MaxResults), NextToken is a Token (@length 1..1024), and the operation binds only InternalServerException and InvalidRequestException (400). A syntactically acceptable but unparseable NextToken is refused HTTP 400 InvalidRequestException with a Message of the fixed form 'The nextPageToken is malformed: ' + the token value from the request, and the rejected call leaves state untouched: an unpaginated ListNamedQueries(WorkGroup=W) still returns the ids created under W.
Emulator: Code 'NotImplemented' — the operation is a scaffold stub, so neither the malformed-token refusal nor the positive listing exists.
Why: a paginating client cannot distinguish a bad continuation token from an unimplemented service, and cannot enumerate named queries at all.
Fix: implement ListNamedQueries in the athena crate (services/service-com-amazonaws-athena-amazonathena/src/ops/list_named_queries.rs), including NextToken parsing and the malformed-token refusal with AWS's message.
Note: the open stub issue 20261104T163600Z-athena-listnamedqueries-stub.md asks for the operation to exist; the suite did not mark this check as covered by it, and the token-validation refusal and its exact message are stated here so a fix is not submitted with the stub merely answering 200.
Traces: athena.list-named-queries-invalid-next-token-rejected.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout (athena: 114/314 checks passing, so the service answers; these nine checks are the failures no open issue owns).

Cause 1 — athena.athena-batch-get-named-query-limit-exceeded-rejected, BatchGetNamedQuery(NamedQueryIds=[1 real id] + 50 bogus ids): AssertionError 'expected InvalidRequestException, got ValidationException' (the check then asserts Message "1 validation error detected: Value at 'namedQueryIds' failed to satisfy constraint: Member must have length less than or equal to 50" and that the 50-id call resolves the real id with 49 entries in UnprocessedNamedQueryIds). athena.batch-get-named-query-exceeds-limit-rejected: AssertionError 'expected InvalidRequestException for 51 ids (1 real + 50 bogus), got ValidationException'.

Cause 2 — athena.op-contract-update-data-catalog-rejects-type-mismatch, after a successful same-Type UpdateDataCatalog, UpdateDataCatalog(Name=<catalog>, Type='LAMBDA', Parameters={'metadata-function': <lambda arn>}): expected Message 'Lambda catalogs require that either the metadata-function and record-function parameters or the function parameter be set, but not both.'; actual 'Parameters for a LAMBDA data catalog must contain either function or both metadata-function and record-function.' Identical expected/actual pair in athena.data-catalog-type-mismatch-update-rejected and athena.data-catalog-update-type-immutable-rejected. Code is InvalidRequestException on both sides.

Cause 3 — athena.op-contract-getNamedQuery, GetNamedQuery(NamedQueryId=<fresh uuid that names nothing>): expected Message 'NamedQuery: <id> does not exist'; actual 'NamedQuery <id> was not found'. The check asserts the same sentence again for the id of a named query it had just deleted.

Cause 4 — athena.workgroup-delete-conflict-rejected: the non-recursive-delete refusal, the surviving workgroup, the empty-sibling delete and DeleteWorkGroup(WorkGroup=W, RecursiveDeleteOption=True) all passed; the check then failed on the next call with botocore.errorfactory.InvalidRequestException: 'An error occurred (InvalidRequestException) when calling the GetNamedQuery operation: NamedQuery <id> was not found' for the named query created with WorkGroup=W, which must still return 200 with WorkGroup=W.

Cause 5 — athena.deploy-named-query-cloudformation-retrievable-and-removed-on-stack-delete: the stack (AWS::Athena::NamedQuery with Name=!Ref QueryName, Database 'default', QueryString 'SELECT 1 AS answer', Outputs.NamedQueryId=!GetAtt MyNamedQuery.NamedQueryId) reached stack_create_complete; GetNamedQuery(NamedQueryId=<that output>) raised botocore.errorfactory.InvalidRequestException '... NamedQuery sim2real-behav-nq-<suffix> was not found' — the identifier in the output (and echoed in the message) is the template's Name, not an Athena-minted NamedQueryId.

Cause 6 — athena.list-named-queries-invalid-next-token-rejected, ListNamedQueries(WorkGroup=<new workgroup>, NextToken='not-a-real-pagination-token=='): AssertionError "expected InvalidRequestException, got 'NotImplemented'" (the check then asserts Message 'The nextPageToken is malformed: <token>' and that a plain ListNamedQueries(WorkGroup=...) still lists the named query created beforehand).

Model check (services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json): BatchGetNamedQuery.NamedQueryIds @length 1..50; BatchGetNamedQuery, GetNamedQuery, ListNamedQueries, DeleteWorkGroup and UpdateDataCatalog each bind only InternalServerException and InvalidRequestException — none binds ValidationException or any NotImplemented shape.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from this checkout.
2. Replay suite simparity-behavioral-aws-v1 and inspect the nine athena checks listed in trace_ids.
3. Minimal manual repro:
   a. CreateNamedQuery(Name=N, Database='default', QueryString='SELECT 1') -> id R. BatchGetNamedQuery(NamedQueryIds=[R] + 50 fresh uuids) must be 400 InvalidRequestException with Message "1 validation error detected: Value at 'namedQueryIds' failed to satisfy constraint: Member must have length less than or equal to 50"; with [R] + 49 of the same uuids it must be 200 with NamedQueries=[R] and 49 UnprocessedNamedQueryIds.
   b. CreateDataCatalog(Name=C, Type='GLUE', Parameters={'catalog-id': <account>}); UpdateDataCatalog(Name=C, Type='GLUE', Description='updated', ...) must succeed; UpdateDataCatalog(Name=C, Type='LAMBDA', Parameters={'metadata-function': <arn>}) must be 400 InvalidRequestException 'Lambda catalogs require that either the metadata-function and record-function parameters or the function parameter be set, but not both.' and leave Type=GLUE.
   c. GetNamedQuery(NamedQueryId=<fresh uuid>) must be 400 InvalidRequestException 'NamedQuery: <that uuid> does not exist'; repeat after DeleteNamedQuery(R) for R.
   d. CreateWorkGroup(Name=W); CreateNamedQuery(..., WorkGroup=W) -> id Q; DeleteWorkGroup(WorkGroup=W) must be refused 'WorkGroup W is not empty'; DeleteWorkGroup(WorkGroup=W, RecursiveDeleteOption=True) must succeed and GetNamedQuery(NamedQueryId=Q) must still be 200 with WorkGroup=W.
   e. CreateStack with an AWS::Athena::NamedQuery resource and Output !GetAtt <logical>.NamedQueryId; after CREATE_COMPLETE the output must be an Athena-minted NamedQueryId whose GetNamedQuery returns the template's Name/Database/QueryString/Description, it must appear in ListNamedQueries, and DeleteStack must remove it from that listing.
   f. ListNamedQueries(WorkGroup=W2, NextToken='not-a-real-pagination-token==') must be 400 InvalidRequestException 'The nextPageToken is malformed: not-a-real-pagination-token==', and ListNamedQueries(WorkGroup=W2) must then still list the named queries saved under W2.
