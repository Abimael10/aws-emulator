Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena.op-contract-get-data-catalog athena.data-catalog-delete-reserved-aws-data-catalog-fails athena.op-contract-batchGetNamedQuery athena.prepared-statement-list-and-batch-get-partial athena.prepared-statement-execute-with-parameters athena.get-session-endpoint-status-nonexistent-resource-not-found athena.get-session-endpoint-unknown-session-not-found athena.query-execution-result-reuse-ttl-window
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena.op-contract-get-data-catalog|_error#7|AssertionError|raise AssertionError(f"expected this call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Key: athena.data-catalog-delete-reserved-aws-data-catalog-fails|run#18|AssertionError|assert message == "Cannot delete AwsDataCatalog.", message
Issue-Key: athena.op-contract-batchGetNamedQuery|run#15|AssertionError|assert unprocessed[bogus].get("ErrorCode") == "NAMED_QUERY_NOT_FOUND", (
Issue-Key: athena.prepared-statement-list-and-batch-get-partial|run#19|AssertionError|assert unprocessed[bogus_name].get("ErrorCode") == "STATEMENTNAME_NOT_FOUND", (
Issue-Key: athena.prepared-statement-execute-with-parameters|run#16|AssertionError|assert "ExecutionParameters" not in exec_info, (
Issue-Key: athena.get-session-endpoint-status-nonexistent-resource-not-found|run#7|AssertionError|assert code == "InvalidRequestException", f"GetSessionStatus: expected InvalidRequestException, got {code}"
Issue-Key: athena.get-session-endpoint-unknown-session-not-found|run#8|AssertionError|assert code == "InvalidRequestException", f"GOT={code!r}"
Issue-Key: athena.query-execution-result-reuse-ttl-window|_poll_until_reused#8|Timeout|last_qid, last_reused = _run_with_reuse(athena, bucket, query, max_age_minutes, workgroup)
Issue-Commit: 57917fbf623074cc27ff2073937536de1df2e679

# athena: 7 net-new causes — the built-in AwsDataCatalog is gettable and its delete refusal is wrong, batch-get partial-failure ErrorCodes are emulator-authored, GetQueryExecution echoes ExecutionParameters, GetSessionEndpoint/GetSessionStatus answer NotImplemented, and the result-reuse TTL window is never honoured

> _Note: this issue was identified against the code base at commit `57917fbf623074cc27ff2073937536de1df2e679`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDataCatalog

## Description
Seven net-new root causes on the Athena control plane, all grounded in failing behavioral checks listed under `### athena` that the replay report marked neither ALREADY FILED nor OWNED. Related open issues that do NOT cover these checks: 20261114T204410Z-athena-workgroup-and-data-catalog-reads-still-drop.md and 20261111T103101Z-athena-updatedatacatalog-refuses-with-notimplemented-instead-of-invalidrequestexception.md (data-catalog reads/updates, not the reserved built-in catalog), 20261108T073030Z-athena-batchgetqueryexecution-classifies-an-unknown-execution-id-as.md (the same INVALID_INPUT defect in BatchGetQueryExecution, a different operation), 20261114T225116Z-athena-6-net-new-causes-batchgetnamedquery-over-limit.md (BatchGetNamedQuery over-limit refusal only), 20261112T101318Z-athena-prepared-statement-reads-drop-the-description-member.md and 20261112T175044Z-athena-4-net-new-causes-execute-using-of.md (prepared-statement reads / EXECUTE ... USING), 20261104T163600Z-athena-getsession-stub.md plus the GetSessionStatus scaffold-stub issue filed from the checkout (GetSession/GetSessionStatus stubs, not GetSessionEndpoint), and 20261111T090701Z-athena-query-execution-metadata-and-result-configuration-semantics.md (its Cause 2 is reuse being always false; Cause 7 below is the MaxAgeInMinutes TTL window itself).

Cause 1 of 7: GetDataCatalog returns the built-in AwsDataCatalog instead of refusing it.
AWS: Smithy GetDataCatalog(Name: CatalogNameString, required) -> GetDataCatalogOutput{DataCatalog}, errors InternalServerException (500) and InvalidRequestException (400). AwsDataCatalog is the implicit built-in catalog: it is enumerated by ListDataCatalogs but it is not a retrievable resource by name, so GetDataCatalog on it is refused with HTTP 400, error code InvalidRequestException and the message `DataCatalog AwsDataCatalog was not found.` — the fixed text is `DataCatalog ` + ` was not found.` and the variable part is the Name taken from the request (recorded behavior only for the reserved-name rule; the same template is what AWS returns for any absent catalog name).
Emulator: the call SUCCEEDS with HTTP 200 and a body `{'DataCatalog': {'Name': 'AwsDataCatalog', 'Type': 'GLUE', 'Status': 'CREATE_COMPLETE'}}`.
Why: a client that uses GetDataCatalog to decide whether a name is a customer-owned, manageable catalog (before UpdateDataCatalog/DeleteDataCatalog, or when reconciling IaC state) is told the built-in catalog is an ordinary managed resource, so it will try to manage a catalog AWS never lets it manage, and the negative branch for an unknown name is never taken.
Fix: the GetDataCatalog handler in services/service-com-amazonaws-athena-amazonathena (the reserved built-in catalog must be visible to ListDataCatalogs but not resolvable by GetDataCatalog) and its InvalidRequestException refusal path.
Traces: athena.op-contract-get-data-catalog.

Cause 2 of 7: DeleteDataCatalog on the reserved AwsDataCatalog refuses with the not-found message instead of the protected-resource message.
AWS: Smithy DeleteDataCatalog(Name required, DeleteCatalogOnly default false), errors InternalServerException (500) and InvalidRequestException (400). Deleting the built-in catalog is refused with HTTP 400, code InvalidRequestException and the message `Cannot delete AwsDataCatalog.` — entirely fixed text, no request- or state-derived part (recorded behavior only). The same call on a customer-owned catalog created by CreateDataCatalog genuinely succeeds and the catalog disappears from ListDataCatalogs.
Emulator: the refusal code is InvalidRequestException, but the message is `DataCatalog AwsDataCatalog was not found` — the not-found template (and without AWS's terminating period), i.e. the emulator treats the reserved catalog as absent rather than protected.
Why: the two conditions are operationally different — `was not found` tells a client the catalog is gone and the desired state is already reached, while `Cannot delete AwsDataCatalog.` tells it the catalog exists and is permanently undeletable. Reconcilers that treat not-found as success will loop or report a successful teardown of a catalog that still exists.
Fix: the DeleteDataCatalog handler in services/service-com-amazonaws-athena-amazonathena — the reserved-name guard must run before the lookup and emit AWS's protected-resource message.
Traces: athena.data-catalog-delete-reserved-aws-data-catalog-fails.

Cause 3 of 7: BatchGetNamedQuery reports an unknown id as INVALID_INPUT instead of NAMED_QUERY_NOT_FOUND.
AWS: Smithy BatchGetNamedQuery(NamedQueryIds @length 1-50) -> {NamedQueries, UnprocessedNamedQueryIds}; UnprocessedNamedQueryId has NamedQueryId, ErrorCode (@length 1-256) and ErrorMessage. A well-formed but unknown NamedQueryId is routed into UnprocessedNamedQueryIds (never raised) with ErrorCode `NAMED_QUERY_NOT_FOUND` — a fixed per-item code distinct from the InvalidRequestException the singular GetNamedQuery raises; the resolvable ids still come back fully populated in NamedQueries. Recorded behavior for the code string.
Emulator: the id is correctly routed to UnprocessedNamedQueryIds, but ErrorCode is `INVALID_INPUT`.
Why: INVALID_INPUT means the caller sent a malformed id (a client should fix its request); NAMED_QUERY_NOT_FOUND means the id was well-formed and simply does not exist (a client should drop it or recreate the saved query). A batch caller that branches on the per-item code mis-handles every missing saved query as a client-side input bug. This is the same defect already filed for BatchGetQueryExecution (20261108T073030Z-...), so the per-item classifier is likely shared.
Fix: the per-item error classification in the BatchGetNamedQuery handler in services/service-com-amazonaws-athena-amazonathena.
Traces: athena.op-contract-batchGetNamedQuery.

Cause 4 of 7: BatchGetPreparedStatement reports an unknown statement name as STATEMENT_NOT_FOUND instead of STATEMENTNAME_NOT_FOUND.
AWS: Smithy BatchGetPreparedStatement(PreparedStatementNames required, WorkGroup required) -> {PreparedStatements, UnprocessedPreparedStatementNames}; each UnprocessedPreparedStatementName carries StatementName, ErrorCode and ErrorMessage. A name that does not exist in the workgroup is returned as unprocessed with ErrorCode `STATEMENTNAME_NOT_FOUND` (fixed text), while known names come back in PreparedStatements with their QueryStatement intact. Recorded behavior for the code string.
Emulator: the unknown name is routed to UnprocessedPreparedStatementNames with ErrorCode `STATEMENT_NOT_FOUND`.
Why: the per-item ErrorCode is the only machine-readable discriminator in a partial-success batch response; a client matching AWS's documented code sees no match and treats the entry as an unknown failure class rather than a missing statement.
Fix: the per-item error classification in the BatchGetPreparedStatement handler in services/service-com-amazonaws-athena-amazonathena.
Traces: athena.prepared-statement-list-and-batch-get-partial.

Cause 5 of 7: GetQueryExecution echoes ExecutionParameters that AWS does not return.
AWS: QueryExecution models ExecutionParameters (list, @length min 1), but Athena does not populate it on the GetQueryExecution response for an execution submitted with StartQueryExecution ExecutionParameters — the member is absent both for an `EXECUTE <stmt>` with parameters and for a direct `WHERE id = ?` query (recorded behavior only). The response records Query verbatim and SubstatementType EXECUTE, which the emulator already gets right.
Emulator: GetQueryExecution returns ExecutionParameters populated with the submitted parameter list (the bound values appear in the response).
Why: Athena deliberately does not echo the bound parameter values, which are frequently the sensitive part of a query; a client that treats the presence of ExecutionParameters as a signal (e.g. 'this execution was parameterised', or that it is safe to log the response) behaves differently against the emulator than against AWS, and values AWS never discloses are disclosed.
Fix: the GetQueryExecution (and BatchGetQueryExecution) QueryExecution serializer in services/service-com-amazonaws-athena-amazonathena — the submitted parameters must be kept in execution state but not serialized onto the response.
Traces: athena.prepared-statement-execute-with-parameters.

Cause 6 of 7: GetSessionEndpoint and GetSessionStatus answer NotImplemented instead of the modeled refusal for a session id that was never created.
AWS: Smithy GetSessionStatus(SessionId required) and GetSessionEndpoint(SessionId required) both model InvalidRequestException (400), ResourceNotFoundException (400) and InternalServerException (500). For a well-formed session id that was never created, both are refused with HTTP 400, code InvalidRequestException and the message `Not authorized to make this request.` — entirely fixed text, with no request-derived part (recorded behavior; the same refusal GetSession already produces correctly in this emulator).
Emulator: both calls are refused with error code `NotImplemented` and the scaffold message `operation <Name> is scaffolded but not implemented in this emulator`. GetSession in the same check returns the correct InvalidRequestException, so the divergence is specific to these two handlers. The GetSessionStatus scaffold stub is already filed from the checkout; GetSessionEndpoint has no stub issue, and these two checks were marked neither ALREADY FILED nor OWNED.
Why: NotImplemented is an unmodeled code no Athena client handles; a client probing a session id gets an unretryable, unclassifiable failure instead of the authorization-shaped refusal AWS uses for unknown sessions, so session-polling and endpoint-attachment code cannot distinguish 'no such session' from an emulator gap.
Fix: implement the GetSessionEndpoint handler (services/service-com-amazonaws-athena-amazonathena/src/ops/get_session_endpoint.rs) and the GetSessionStatus handler (.../src/ops/get_session_status.rs), including their unknown-session InvalidRequestException refusal path.
Traces: athena.get-session-endpoint-status-nonexistent-resource-not-found, athena.get-session-endpoint-unknown-session-not-found.

Cause 7 of 7: the result-reuse MaxAgeInMinutes TTL window is never honoured, so the reuse check never reaches a verdict.
AWS: Smithy ResultReuseByAgeConfiguration{Enabled required boolean, MaxAgeInMinutes @range 0-10080 default 60}; QueryExecutionStatistics.ResultReuseInformation.ReusedPreviousResult is a required boolean. An identical DML SELECT over a real external table, re-issued in an engine-version-3 workgroup inside the MaxAgeInMinutes window, reports ReusedPreviousResult true (with a new QueryExecutionId), while the same query submitted with MaxAgeInMinutes=0 — a window that has already elapsed — must run fresh and report false.
Emulator: the check does not complete: it exhausts the harness's 60 s budget (`Check timed out after 60.0s`) because its reuse poll re-issues the identical execution for up to 150 s waiting for ReusedPreviousResult to turn true. Hypothesis: the reuse leg never becomes true, which matches Cause 2 of the open issue 20261111T090701Z-athena-query-execution-metadata-and-result-configuration-semantics.md (ReusedPreviousResult always false); the MaxAgeInMinutes=0 leg — a zero-length TTL window forcing a fresh execution — is additionally not covered there, so the fix must honour the window boundary, not merely be able to report reuse.
Why: a client that enables result reuse for cost and latency can never observe a reuse hit, and cannot use MaxAgeInMinutes to bound how stale a reused result may be — every freshness decision silently takes the no-reuse branch.
Fix: result-reuse lookup keyed on query identity AND the submitted MaxAgeInMinutes window in the Athena query-execution engine, plus the Statistics.ResultReuseInformation serializer on GetQueryExecution (services/service-com-amazonaws-athena-amazonathena).
Traces: athena.query-execution-result-reuse-ttl-window.

## Evidence
Cause 1 — expected: GetDataCatalog(Name='AwsDataCatalog') refused, code InvalidRequestException, message 'DataCatalog AwsDataCatalog was not found.'. Actual: AssertionError "expected this call to be rejected, but it SUCCEEDED and returned {'DataCatalog': {'Name': 'AwsDataCatalog', 'Type': 'GLUE', 'Status': 'CREATE_COMPLETE'}}" with HTTPStatusCode 200. (ListDataCatalogs correctly lists AwsDataCatalog, and the positive leg — GetDataCatalog on a CreateDataCatalog-made catalog — is never reached.)

Cause 2 — expected: DeleteDataCatalog(Name='AwsDataCatalog') refused with InvalidRequestException and message 'Cannot delete AwsDataCatalog.'. Actual: code InvalidRequestException (correct) with message 'DataCatalog AwsDataCatalog was not found' (AssertionError: DataCatalog AwsDataCatalog was not found). The customer-owned-catalog positive control (create -> list -> delete -> absent from list) passed, so only the reserved-name branch diverges.

Cause 3 — expected: UnprocessedNamedQueryIds[<unknown uuid>].ErrorCode == 'NAMED_QUERY_NOT_FOUND'. Actual: AssertionError "expected ErrorCode 'NAMED_QUERY_NOT_FOUND', got 'INVALID_INPUT'". Routing and the resolved-id payload were correct.

Cause 4 — expected: UnprocessedPreparedStatementNames[<unknown name>].ErrorCode == 'STATEMENTNAME_NOT_FOUND'. Actual: AssertionError "expected ErrorCode STATEMENTNAME_NOT_FOUND for unknown name, got 'STATEMENT_NOT_FOUND'". ListPreparedStatements and the known statement's QueryStatement were correct.

Cause 5 — expected: 'ExecutionParameters' not in the GetQueryExecution QueryExecution. Actual: AssertionError "real Athena does not echo ExecutionParameters on QueryExecution, got ['2']" for an execution submitted as `EXECUTE <stmt>` with ExecutionParameters=['2']. Query verbatim and SubstatementType 'EXECUTE' asserted before it both passed.

Cause 6 — expected: GetSessionStatus / GetSessionEndpoint on a never-created uuid refused with InvalidRequestException and message 'Not authorized to make this request.'. Actual: AssertionError "GetSessionStatus: expected InvalidRequestException, got NotImplemented" and AssertionError "GOT='NotImplemented'" for GetSessionEndpoint; the emulator's message is 'operation <Name> is scaffolded but not implemented in this emulator'. GetSession and StartSession's 'WorkGroup is not found.' refusal in the same checks passed.

Cause 7 — expected: second identical execution inside MaxAgeInMinutes=60 reports ReusedPreviousResult True (new QueryExecutionId), and MaxAgeInMinutes=0 reports False. Actual: 'Check timed out after 60.0s' — no verdict; the reuse poll re-issues the identical execution for up to 150 s waiting for a reuse hit that never arrives.

## Steps to Reproduce
1. Build and boot the emulator; point botocore athena (plus s3 and sts) clients at it.
2. Cause 1: call list_data_catalogs() (AwsDataCatalog is listed), then get_data_catalog(Name='AwsDataCatalog') — it returns 200 with a DataCatalog body instead of InvalidRequestException 'DataCatalog AwsDataCatalog was not found.'.
3. Cause 2: create_data_catalog(Name=<new>, Type='GLUE', Parameters={'catalog-id': <account>}), confirm it lists and delete_data_catalog removes it (control passes), then delete_data_catalog(Name='AwsDataCatalog') — the message is 'DataCatalog AwsDataCatalog was not found' instead of 'Cannot delete AwsDataCatalog.'.
4. Cause 3: create two named queries, call batch_get_named_query(NamedQueryIds=[<real>, <random uuid>]) and read UnprocessedNamedQueryIds[0]['ErrorCode'] — 'INVALID_INPUT'.
5. Cause 4: create a workgroup and a prepared statement in it, call batch_get_prepared_statement(PreparedStatementNames=[<real>, <missing>], WorkGroup=<wg>) and read UnprocessedPreparedStatementNames[0]['ErrorCode'] — 'STATEMENT_NOT_FOUND'.
6. Cause 5: create a prepared statement `SELECT id, name FROM "<db>"."<tbl>" WHERE id = ?`, start_query_execution(QueryString='EXECUTE <stmt>', WorkGroup=<wg>, ExecutionParameters=['2']), wait for SUCCEEDED and read get_query_execution(...)['QueryExecution'] — 'ExecutionParameters' is present with ['2'].
7. Cause 6: call get_session_status(SessionId=<random uuid>) and get_session_endpoint(SessionId=<random uuid>) — both fail with error code NotImplemented instead of InvalidRequestException 'Not authorized to make this request.' (get_session on the same id already returns the correct refusal).
8. Cause 7: create a workgroup pinned to 'Athena engine version 3', put a small CSV in S3, CREATE EXTERNAL TABLE over it, then start_query_execution the same `SELECT count(*)` twice with ResultReuseConfiguration={'ResultReuseByAgeConfiguration': {'Enabled': True, 'MaxAgeInMinutes': 60}} and read Statistics.ResultReuseInformation.ReusedPreviousResult on the second (never True); then repeat with MaxAgeInMinutes=0 and confirm it must be False.
