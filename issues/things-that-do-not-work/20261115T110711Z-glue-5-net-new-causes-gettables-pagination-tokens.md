Issue-Source: trace-sweep-divergence
Issue-Trace: glue-cflog-get-tables-paginated-and-filtered
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-cflog-get-tables-paginated-and-filtered glue-tsd-next-token-reused-across-databases glue-gjwr-traj-catalog-is-regional-under-one-catalog-id glue-tsd-expression-filter-mistakes glue-gjwr-traj-start-the-copied-job-fails-before-provisioning glue-glue-table-round-trips-the-cloudfront-log-schema-and-fills-catalog-defaults
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|GetTables|2xx->2xx|$.NextToken
Issue-Key: glue|GetTables|2xx->2xx|$.TableList[*]
Issue-Key: glue|GetTables|4xx->2xx|$.Message
Issue-Key: glue|GetTables|4xx->2xx|$.__type
Issue-Key: glue|GetTables|4xx->2xx|status
Issue-Key: glue|GetJobRun|2xx->4xx|$.JobRun
Issue-Key: glue|GetJobRun|2xx->4xx|status
Issue-Key: glue|GetTables|2xx->2xx|$.TableList[*].StorageDescriptor.Parameters
Issue-Commit: 51f1471086a88eea31713bb27037df7ba8542a2d

# glue: 5 net-new causes — GetTables pagination tokens and page boundaries, an absent-database read, an echoed empty StorageDescriptor.Parameters and a refused job-run read

> _Note: this issue was identified against the code base at commit `51f1471086a88eea31713bb27037df7ba8542a2d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTables

## Description
Five net-new wire causes on the glue service, all reached AFTER a live handler answered the operation (every step below returned a 200 or a 400 from an implemented handler, so none is a bootstrap gap or an unimplemented stub). None of them is owned by an open issue's replay key. A fixer must close ALL FIVE before submitting; each cause names its own fixture and step.

Cause 1 of 5 — GetTables' NextToken is not an opaque AWS-form pagination token: it is a hex-encoded cleartext record of the page cursor.
AWS: Smithy: GetTables is @paginated (inputToken NextToken, outputToken NextToken, pageSize MaxResults); GetTablesResponse.NextToken targets Token (a plain string with no modeled constraints), so its concrete form is recorded behavior only. The corpus asserts the member by SHAPE ('<shaped-not-asserted>'), i.e. AWS returns a freshly generated continuation token whose FORM is the requirement — an opaque, base64-encoded blob (the same corpus records exactly that form for Glue's other paginated reads, e.g. GetResourcePolicies' NextToken 'eyJleHBpcmF0aW9uIjp7...' ). The requirement is that the token be an opaque token of that form, carrying no readable catalog identity.
Emulator: emits a long lowercase-hex string whose bytes decode to cleartext — a 32-byte prefix followed by the ASCII operation name 'GetTables', a 0x1f separator, the catalog id, the database name and the table name (replayed value decodes to 'GetTables\x1f<account-id>\x1f<database-name>\x1e<table-name>'). It satisfies neither the recorded token's form nor its opacity.
Why: a client must treat NextToken as opaque, but the emulator's token is a different alphabet and length class than AWS's, so any assertion, length budget, logging redaction or round-trip through a store sized for AWS's token behaves differently; worse, the token leaks catalog identity (account, database, table) in cleartext to anyone who sees a paging cursor, which AWS's token does not.
Fix: the pagination-token encoder shared by the glue catalog read handlers (the cursor serializer behind GetTables) — emit an opaque base64 token of AWS's form instead of hex-encoded cleartext cursor fields.
Traces: glue-cflog-get-tables-paginated-and-filtered#task#6 (cluster also covers glue-tsd-next-token-reused-across-databases#task#8).

Cause 2 of 5 — a GetTables continuation request does not honour MaxResults: the second page returns every remaining table and drops NextToken, so the recorded page boundary disappears.
AWS: Smithy: GetTables @paginated with pageSize MaxResults (CatalogGetterPageSize, @range min 1 max 100) and input NextToken (Token). AWS applies MaxResults to EVERY page, including a page requested with a NextToken: a continuation that still has more tables than MaxResults returns exactly MaxResults entries in TableList and a further NextToken; only the final page omits NextToken. The recorded continuation returns a single-entry TableList plus a NextToken.
Emulator: for the same continuation request it returns the whole remainder in one response (TableList carries an extra entry beyond the recorded page size) and omits NextToken entirely, terminating pagination a page early.
Why: a client paging the catalog with a fixed page size gets pages of the wrong size and a truncated page sequence — code that allocates/batches per page over-reads, and any test or workflow that depends on more than one continuation (or on seeing the Nth page) never observes it. Combined with cause 1, cursor-based catalog traversal is unreliable.
Fix: the GetTables handler's pagination logic (apply MaxResults on the token-continued path, and emit NextToken whenever entries remain after the page is cut).
Traces: glue-cflog-get-tables-paginated-and-filtered#task#7.

Cause 3 of 5 — GetTables over a database that does not exist in the requested scope answers 200 with a table list instead of refusing.
AWS: Smithy: GetTables declares DatabaseName @required and models EntityNotFoundException (errorKind client, httpError 400, doc 'A specified entity does not exist'). The recording shows 400 EntityNotFoundException with Message 'Database <name> not found.' — fixed text 'Database ' + the request's DatabaseName + ' not found.'; the Glue Data Catalog is regional, so a database created in another region is absent for this request.
Emulator: answers HTTP 200 with a GetTablesResponse body and no __type/Message at all, i.e. it reports the database as existing (empty/implicit table list) rather than refusing.
Why: a client cannot distinguish 'database has no tables' from 'database does not exist', so a create-then-verify or region-scoping sequence passes against a catalog that does not hold the database, and error-handling paths keyed on EntityNotFoundException are never exercised.
Fix: the GetTables handler's database existence check, scoped to the request's catalog/region, in glue catalog state. Related open issue that does NOT cover this step: 20261102T132146Z-glue-answers-200-for-absent-entities-and-an.md carries the same shape for GetDatabase/GetConnection/GetJob/CreateJob and its replay key does not include this GetTables cluster; 20261108T175930Z-athena-cloudformation-glue-glue-gettable-over-a-database.md is about GetTable's message wording, not GetTables answering 200.
Traces: glue-gjwr-traj-catalog-is-regional-under-one-catalog-id#task#4 (cluster also covers glue-tsd-expression-filter-mistakes#task#8).

Cause 4 of 5 — GetJobRun refuses with 400 a run that exists and is terminal, so the failed run of a job created by copying an existing definition can never be read.
AWS: Smithy: GetJobRun (JobName @required, RunId @required) returns GetJobRunResponse.JobRun and models EntityNotFoundException only when the job or run does not exist. The recording shows 200 with the complete JobRun record for the run id StartJobRun returned — JobRunState FAILED, Attempt 0, ExecutionTime 0, PredecessorRuns [], the job's GlueVersion/WorkerType/NumberOfWorkers/MaxCapacity/Timeout as created, and ErrorMessage in the job-executor envelope. The requirement is that a run id handed out by StartJobRun resolves for its job until it is deleted, whatever terminal state it settled in.
Emulator: answers HTTP 400 and the response body carries no JobRun member at all, so the run is reported as a nonexistent entity.
Why: the terminal record is the only way a client learns WHY a run failed; refusing the read makes a run that was accepted and did settle unobservable, and a poller that distinguishes 'not found' from 'still running' concludes the run never existed. Fix-verification and orchestration both break.
Fix: the glue job-run state/lookup behind GetJobRun — register the run under the job it was started for (including a job created from a copied definition) and keep it resolvable by RunId after it settles, instead of letting the lookup miss and raise the not-found path. Related open issue that does NOT cover this: 20261114T041346Z-glue-5-net-new-causes-job-run-reads.md covers task#4/task#5 of the same fixture (the 200-answered record's ErrorMessage/ExecutionArguments/ExecutionTime) and its replay key does not include task#8, where the read is refused outright.
Traces: glue-gjwr-traj-start-the-copied-job-fails-before-provisioning#task#8.

Cause 5 of 5 — GetTables emits StorageDescriptor.Parameters as an empty map for a table created without storage-descriptor parameters, where AWS omits the member.
AWS: Smithy: StorageDescriptor.Parameters targets ParametersMap, documented 'The user-supplied properties in key-value form', with no default and no @required. The recording shows that when a table is created with a StorageDescriptor that supplies no Parameters, the member is ABSENT from every table in GetTables' TableList — AWS does not synthesise an empty map for user-supplied properties that were never supplied.
Emulator: serialises Parameters as {} on the storage descriptor, so 'no storage-descriptor properties' is indistinguishable from 'an explicitly empty property map'.
Why: a client that uses member presence to decide whether to preserve, merge or overwrite user-supplied properties (the usual read-modify-write over a table definition) sees a property map that was never set, and round-trips an empty map back into UpdateTable; serializer-strictness checks and diffing tools also report a member AWS never sent.
Fix: the glue table/StorageDescriptor serializer behind the catalog reads — omit Parameters when no entries were supplied rather than defaulting it to an empty map. Related open issue that does NOT cover this: 20261115T052937Z-firehose-glue-kinesis-s3-glue-gettable-reports-parameters.md is about the TABLE-level Parameters member on GetTable; this cluster is StorageDescriptor.Parameters on GetTables and is not on that issue's replay key.
Traces: glue-glue-table-round-trips-the-cloudfront-log-schema-and-fills-catalog-defaults#task#5.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 1300 steps, 1153 passed). All five clusters below are glue, verdict 'mismatch', with empty stub_path and empty owned_by.

Cause 1 — fixture glue-cflog-get-tables-paginated-and-filtered, step task#6, GetTables, status 200 expected / 200 observed: $.NextToken value_mismatch — expected a shape-asserted opaque token ('<shaped-not-asserted>'), got '666361a4f8f8ea59f1991402303d6d3d3fc045a5935302b344bec2e9bbca54e04765745461626c65731f3030303030303030303030301f73696d327265616c5f70726f62655f63666c6f675f70675f34313861613965331e636c6f756466726f6e745f6163636573735f6c6f6773', whose hex bytes decode to '…GetTables\x1f000000000000\x1fsim2real_probe_cflog_pg_418aa9e3\x1ecloudfront_access_logs'. The cleartext fields are this run's catalog/database/table names (per-run values); the requirement is the token's opaque base64 form, not these values. Cluster also covers glue-tsd-next-token-reused-across-databases#task#8.

Cause 2 — same fixture, step task#7, GetTables, status 200/200: $.NextToken missing (expected '<shaped-not-asserted>', got None) and $.TableList[1] extra (expected None, got a second full table record). The recorded continuation page carries exactly one table plus a NextToken; the emulator returns two tables and no token.

Cause 3 — fixture glue-gjwr-traj-catalog-is-regional-under-one-catalog-id, step task#4, GetTables: status expected 400, got 200; $.__type missing (expected 'EntityNotFoundException', got None); $.Message missing (expected 'Database sim2real-probe-gjwr-db-3a0f88c9 not found.' — 'Database ' and ' not found.' are fixed text, the name is the request's DatabaseName). Cluster also covers glue-tsd-expression-filter-mistakes#task#8.

Cause 4 — fixture glue-gjwr-traj-start-the-copied-job-fails-before-provisioning, step task#8, GetJobRun: status expected 200, got 400; $.JobRun missing — expected the full terminal record (JobRunState 'FAILED', Attempt 0, ExecutionTime 0, PredecessorRuns [], GlueVersion '4.0', JobMode 'SCRIPT', WorkerType 'G.1X', NumberOfWorkers 2, MaxCapacity 2.0, Timeout 5, LogGroupName '/aws-glue/jobs', and the AWSGlueJobExecutor-enveloped ErrorMessage), got None. The run id, job name, role arn, request id and timestamps in that record are per-run values; the requirement is that the run resolves 200 with its terminal record.

Cause 5 — fixture glue-glue-table-round-trips-the-cloudfront-log-schema-and-fills-catalog-defaults, step task#5, GetTables, status 200/200: $.TableList[0].StorageDescriptor.Parameters unexpected_present — expected None (member absent), got {}.

## Steps to Reproduce
All five are reproduced by replaying the glue wire corpus (trace set simparity-wire-aws-v1) against the emulator.

Cause 1: replay glue-cflog-get-tables-paginated-and-filtered, step task#6 — CreateDatabase plus several CreateTable calls, then GetTables(DatabaseName, MaxResults=1) and inspect the returned NextToken's form (hex-decode it to see the leaked cursor fields).
Cause 2: replay the same fixture, step task#7 — repeat GetTables(DatabaseName, MaxResults=1, NextToken=<token from task#6>) and count TableList entries and check for a further NextToken.
Cause 3: replay glue-gjwr-traj-catalog-is-regional-under-one-catalog-id, step task#4 — GetTables for a DatabaseName that does not exist in the requested catalog/region, and read the status, __type and Message.
Cause 4: replay glue-gjwr-traj-start-the-copied-job-fails-before-provisioning, step task#8 — GetJob the original job, CreateJob a copy of that definition with an execution role that does not trust Glue, StartJobRun the copy, then GetJobRun(JobName=<copied job>, RunId=<id StartJobRun returned>) once the run is terminal.
Cause 5: replay glue-glue-table-round-trips-the-cloudfront-log-schema-and-fills-catalog-defaults, step task#5 — CreateTable with a StorageDescriptor that supplies no Parameters, then GetTables(DatabaseName) and check whether TableList[0].StorageDescriptor.Parameters is present.
