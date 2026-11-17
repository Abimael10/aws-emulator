Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena.capacity-reservation-provisioned-dpu-account-cap-rejected athena.create-data-catalog-name-space-and-unicode-accepted-control-char-rejected athena.deploy-capacity-reservation-cloudformation-lifecycle athena.deploy-capacity-reservation-rename-with-assigned-workgroup-rejected athena.get-query-results-data-manifest-type-unload-vs-select athena.get-session-endpoint-status-nonexistent-resource-not-found athena.session-endpoint-unavailable-before-session-ready athena.list-executors-fails-after-session-termination athena.list-executors-reflects-session-executors athena.list-notebook-sessions-reflects-active-sessions athena.primary-workgroup-cannot-be-deleted athena.session-idle-timeout-auto-terminates athena.session-idle-ttl-transitions-to-terminated
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena.capacity-reservation-provisioned-dpu-account-cap-rejected|_error#10|AssertionError|raise AssertionError(f"expected a ClientError, but the call succeeded and returned {result!r}")
Issue-Key: athena.create-data-catalog-name-space-and-unicode-accepted-control-char-rejected|run#12|AssertionError|assert message == f"DataCatalog name {space_name} is invalid.", (
Issue-Key: athena|CreateStack|ValidationError
Issue-Key: athena.get-query-results-data-manifest-type-unload-vs-select|run#8|AssertionError|assert unload_qe["Status"]["State"] == "SUCCEEDED", (
Issue-Key: athena.get-session-endpoint-status-nonexistent-resource-not-found|run#10|AssertionError|assert code == "InvalidRequestException", f"GetSessionEndpoint: expected InvalidRequestException, got {code}"
Issue-Key: athena.session-endpoint-unavailable-before-session-ready|run#10|AssertionError|assert code == "InvalidRequestException", f"expected InvalidRequestException while not ready, got {code!r}"
Issue-Key: athena|ListExecutors|InvalidRequestException
Issue-Key: athena.list-notebook-sessions-reflects-active-sessions|run#18|AssertionError|assert session_id in after_ids, (
Issue-Key: athena|GetWorkGroup|InvalidRequestException
Issue-Key: athena.session-idle-timeout-auto-terminates|_poll_until#8|Timeout|time.sleep(interval_s)
Issue-Key: athena.session-idle-ttl-transitions-to-terminated|run#10|AssertionError|assert state == "TERMINATED", (
Issue-Commit: addb8a7148576d7d08097a9e3c6488eb5c13818f

# athena: 9 net-new causes — the Provisioned-DPU account cap is not enforced, 'primary' workgroup is absent, GetSessionEndpoint/ListExecutors refuse live sessions, idle sessions never auto-terminate, AWS::Athena::CapacityReservation is unregistered, and two refusal messages are emulator-authored

> _Note: this issue was identified against the code base at commit `addb8a7148576d7d08097a9e3c6488eb5c13818f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCapacityReservation

## Description
Nine distinct root causes found in this behavioral sweep of the athena surface. All nine are net-new: none of the failing checks below is marked ALREADY FILED or OWNED by an open issue in the replay report. Each cause must be closed before this issue is submitted.

Cause 1 of 9: CreateCapacityReservation does not enforce the account-wide Provisioned DPU quota.
AWS: Smithy: CreateCapacityReservation.TargetDpus is @required integer with @range min 4; the operation's only client error is InvalidRequestException (400). The account-wide "Provisioned DPU" quota is fixed and non-adjustable (AWS General Reference, Athena endpoints and quotas), and a TargetDpus request far past it is refused synchronously with InvalidRequestException and the message "Exceeded the maximum number of active Data Processing Units (DPUs) allowed. If you require more DPUs for your use case, please reach out to athena-feedback@amazon.com." That whole string is fixed text — it interpolates nothing from the request. The refused request creates no reservation, so a following GetCapacityReservation on that name must refuse with InvalidRequestException and "Reservation with name <Name> not found." (fixed text around the requested Name).
Emulator: the call with TargetDpus far past the cap returns HTTP 200 with an empty CreateCapacityReservation body, so the reservation is created.
Why: a client that sizes capacity against the account quota sees over-cap provisioning succeed, so quota-handling and retry-with-smaller-target logic is never exercised and the emulated account holds capacity real AWS would never grant.
Fix: CreateCapacityReservation's handler/validation must apply the account-wide provisioned-DPU ceiling before creating state and raise InvalidRequestException with AWS's fixed message. Related but not covering: 20261116T161544Z-athena-3-net-new-causes-both-targetdpus-refusal.md covers the per-request TargetDpus range wording only, not the account cap.
Traces: athena.capacity-reservation-provisioned-dpu-account-cap-rejected

Cause 2 of 9: CreateDataCatalog's invalid-Name refusal message is emulator-authored.
AWS: Smithy: CreateDataCatalog.Name is @required with @length 1..256 and @pattern ^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$ — U+0020 (space) satisfies the pattern, so a space-bearing name passes wire validation and is refused by the service with InvalidRequestException (400) and the message "DataCatalog name <Name> is invalid.", whose only variable part is the rejected Name copied from the request. A name holding a character below U+0020 fails the modeled pattern and is refused with the framework validation message "1 validation error detected: Value at 'name' failed to satisfy constraint: Member must satisfy regular expression pattern: [\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*" (fixed text; 'name' is the member name). Neither refused name may appear in ListDataCatalogs.
Emulator: the space-and-unicode Name is refused, but with the emulator-authored message "Name contains the character ' '; a data catalog name can use alphanumeric, underscore, at sign, or hyphen characters." — a different rule and a different wording, and it describes a charset AWS's own pattern does not impose.
Why: a client that matches on the refusal text (or surfaces it to an operator) gets a rule that contradicts AWS's: it reads that at-sign/underscore are the only extras and that non-ASCII letters are illegal, which is not AWS's constraint.
Fix: CreateDataCatalog's name validation — emit AWS's "DataCatalog name <Name> is invalid." for a service-level invalid name and the modeled-pattern validation message for a character below U+0020.
Traces: athena.create-data-catalog-name-space-and-unicode-accepted-control-char-rejected

Cause 3 of 9: AWS::Athena::CapacityReservation is not a registered CloudFormation resource type.
AWS: recorded behavior only (the Athena Smithy model does not describe CloudFormation). Real AWS accepts a template whose Resources hold an AWS::Athena::CapacityReservation and CreateStack returns a StackId (200); the reservation is then readable through Athena's GetCapacityReservation and removed when the stack is deleted.
Emulator: CreateStack is refused with HTTP 400 ValidationError and the message "Template format error: Unrecognized resource types: [AWS::Athena::CapacityReservation]" — fixed text "Template format error: Unrecognized resource types: [...]" with the template's resource type listed inside.
Why: every infrastructure-as-code path that provisions Athena capacity through CloudFormation fails at stack creation, so no deploy, rename-rejection or stack-delete behavior for capacity reservations can be exercised at all.
Fix: the CloudFormation resource-type registry plus the AWS::Athena::CapacityReservation resource provider (create/update/delete mapped onto Athena's capacity-reservation state). Sibling, not covering: 20261109T123922Z-athena-cloudformation-aws-athena-preparedstatement-is-not-a.md reports the same gap for AWS::Athena::PreparedStatement only.
Traces: athena.deploy-capacity-reservation-cloudformation-lifecycle, athena.deploy-capacity-reservation-rename-with-assigned-workgroup-rejected

Cause 4 of 9: UNLOAD is not parsed, so GetQueryResults never serves an UNLOAD data manifest.
AWS: recorded behavior only for the SQL dialect. Athena's engine accepts UNLOAD (SELECT ...) TO '<s3 uri>' WITH (format = 'JSON'); the execution reaches SUCCEEDED, GetQueryExecution exposes the DataManifestLocation, and GetQueryResults for the UNLOAD reports the manifest-typed result rather than SELECT rows.
Emulator: the execution settles State=FAILED with StateChangeReason and AthenaError.ErrorMessage 'near "UNLOAD": syntax error in UNLOAD (SELECT 1 AS col) TO '<uri>' WITH (format = 'JSON') at offset 0' — a SQLite parser diagnostic, so nothing downstream (manifest, result type) is produced.
Why: a client that unloads results to S3 and reads the manifest gets a failed query and no manifest, so the UNLOAD-vs-SELECT distinction in GetQueryResults is unobservable.
Fix: the query engine's statement parser/planner for UNLOAD, and the GetQueryResults result-type/manifest serializer. Related but not covering this check (the report leaves it unowned): 20261111T040849Z-athena-s3-athena-s-sql-engine-parses-neither-ctas-with.md and 20261111T090701Z-athena-s-sql-engine-refuses-trino-hive-statements.md.
Traces: athena.get-query-results-data-manifest-type-unload-vs-select

Cause 5 of 9: GetSessionEndpoint answers NotImplemented for every session id.
AWS: Smithy: GetSessionEndpoint takes a @required SessionId and returns required EndpointUrl, AuthToken and AuthTokenExpirationTime; its modeled client errors are InvalidRequestException (400) and ResourceNotFoundException (400). For a session id that was never issued Athena refuses with InvalidRequestException and the fixed message "Not authorized to make this request." For a session that exists but is not ready it refuses with InvalidRequestException and "Can not generate Session endpoint URL for Session in PROVISIONING state" (fixed text with the session's current state substituted), and once the session is IDLE the same call returns an endpoint URL, auth token and expiration time.
Emulator: every GetSessionEndpoint call is refused with error code NotImplemented ("operation GetSessionEndpoint is scaffolded but not implemented in this emulator"), both for a never-issued id and for a live, not-yet-IDLE session.
Why: NotImplemented is not a modeled error for this operation, so an SDK client cannot catch it as InvalidRequestException/ResourceNotFoundException; and no client can obtain the endpoint/token pair needed to talk to a Spark session.
Fix: implement GetSessionEndpoint in its handler — the not-ready and unknown-session refusals plus the endpoint/token response. Related but not covering these two checks: 20261104T163600Z-athena-interactive-session-calculation-notebook-and-tagging-operations.md.
Traces: athena.get-session-endpoint-status-nonexistent-resource-not-found, athena.session-endpoint-unavailable-before-session-ready

Cause 6 of 9: ListExecutors refuses a session the emulator itself just created.
AWS: Smithy: ListExecutors takes a @required SessionId and returns a required SessionId plus ExecutorsSummary (and NextToken); modeled client errors are InvalidRequestException and ResourceNotFoundException (both 400). For a session started by StartSession the call succeeds (200) and lists at least the coordinator, each entry carrying ExecutorId, ExecutorType (COORDINATOR reaches REGISTERED) and an ExecutorState from the modeled enum; after TerminateSession the call keeps succeeding with the same roster settled to TERMINATING/TERMINATED/FAILED. "Not authorized to make this request." is the message AWS uses for a session id the caller's account never held — not for its own live session.
Emulator: ListExecutors on the live session id returned by StartSession raises InvalidRequestException with "Not authorized to make this request.", i.e. it recognises no session at all.
Why: a client cannot observe DPU/executor allocation for a session it owns, and the refusal misattributes the failure to authorization, sending callers to fix IAM for a session that exists.
Fix: ListExecutors' handler must resolve the SessionId against session state and serialize the executor roster (including post-termination terminal states), refusing only ids that do not belong to the account.
Traces: athena.list-executors-fails-after-session-termination, athena.list-executors-reflects-session-executors

Cause 7 of 9: ListNotebookSessions drops a session once it is terminated.
AWS: Smithy: ListNotebookSessions returns a @required NotebookSessionsList (length 0..10) whose entries carry only SessionId and CreationTime — there is no state member and no state filter, so the list is a lifetime roster: a session started against a notebook stays listed after GetSessionStatus reports TERMINATED.
Emulator: after TerminateSession the notebook's roster reads back empty (set() of SessionIds), so the session that was listed while active disappears.
Why: a client reconciling a notebook's session history (audit, cleanup, "which sessions ran on this notebook") loses every terminated session and concludes the notebook was never used.
Fix: the notebook-session state/listing for ListNotebookSessions — retain terminated sessions on the notebook's roster instead of filtering by live state.
Traces: athena.list-notebook-sessions-reflects-active-sessions

Cause 8 of 9: the account's built-in 'primary' workgroup does not exist.
AWS: recorded behavior only (the model has no per-resource bootstrap). Every account has a service-created workgroup named "primary" in State ENABLED without any caller CreateWorkGroup, so GetWorkGroup(WorkGroup="primary") succeeds (200) with Name="primary", State="ENABLED"; DeleteWorkGroup on it is refused with InvalidRequestException (400, the operation's only modeled client error) and the fixed message "primary workGroup could not be deleted", and the refused delete leaves it present and ENABLED. By contrast a deleted caller-created workgroup reads back as InvalidRequestException "WorkGroup is not found."
Emulator: GetWorkGroup(WorkGroup="primary") raises InvalidRequestException "WorkGroup is not found." — the built-in workgroup was never seeded, so the managed-resource protection on DeleteWorkGroup cannot be observed either.
Why: "primary" is the default every client falls back to when no WorkGroup is supplied; with it absent, default-workgroup queries and console-equivalent flows fail not-found, and a client can also never learn that the managed workgroup is undeletable.
Fix: seed the account's "primary" workgroup in Athena's workgroup state at startup and add the managed-resource guard in DeleteWorkGroup's handler.
Traces: athena.primary-workgroup-cannot-be-deleted

Cause 9 of 9: a session left idle never auto-terminates on its SessionIdleTimeoutInMinutes.
AWS: Smithy: StartSession.SessionIdleTimeoutInMinutes is an integer with @range 1..480. A session started with that value and then left idle transitions on its own from IDLE through TERMINATING to TERMINATED — observable through GetSessionStatus — with no TerminateSession call, and not before its configured idle window has elapsed.
Emulator: the session reaches IDLE and stays IDLE indefinitely; one check observes state='IDLE' after polling past the configured window, the other is still polling when the harness gives up ("Check timed out after 600.0s"). No idle reaper exists, so the configured timeout has no effect.
Why: a client relying on Athena to reclaim idle Spark sessions sees them live forever: sessions accumulate against session/DPU limits, and any logic that waits for the automatic TERMINATED transition never completes.
Fix: session state must carry the configured idle deadline and a background transition must drive IDLE -> TERMINATING -> TERMINATED when it expires, reflected by GetSession/GetSessionStatus.
Traces: athena.session-idle-timeout-auto-terminates, athena.session-idle-ttl-transitions-to-terminated

## Evidence
Cause 1: expected ClientError InvalidRequestException "Exceeded the maximum number of active Data Processing Units (DPUs) allowed. If you require more DPUs for your use case, please reach out to athena-feedback@amazon.com."; actual HTTP 200 with an empty CreateCapacityReservation body ("expected a ClientError, but the call succeeded and returned {... 'HTTPStatusCode': 200 ...}").
Cause 2: expected InvalidRequestException "DataCatalog name <Name> is invalid."; actual InvalidRequestException "Name contains the character ' '; a data catalog name can use alphanumeric, underscore, at sign, or hyphen characters."
Cause 3: expected CreateStack to accept the template and return a StackId; actual HTTP 400 ValidationError "Template format error: Unrecognized resource types: [AWS::Athena::CapacityReservation]".
Cause 4: expected the UNLOAD execution State=SUCCEEDED with a data manifest; actual State='FAILED', StateChangeReason/AthenaError.ErrorMessage 'near "UNLOAD": syntax error in UNLOAD (SELECT 1 AS col) TO '<s3 uri>' WITH (format = 'JSON') at offset 0', ErrorCategory 2.
Cause 5: expected InvalidRequestException "Not authorized to make this request." (unknown session) and InvalidRequestException "Can not generate Session endpoint URL for Session in PROVISIONING state" (not ready), then a 200 carrying EndpointUrl/AuthToken/AuthTokenExpirationTime once IDLE; actual error code NotImplemented in both refusal cases ("GetSessionEndpoint: expected InvalidRequestException, got NotImplemented").
Cause 6: expected ListExecutors 200 with >=1 executor and exactly one COORDINATOR in REGISTERED for a live session (and a surviving roster after TerminateSession); actual InvalidRequestException "Not authorized to make this request." on the SessionId StartSession had just returned.
Cause 7: expected the SessionId to remain in NotebookSessionsList after GetSessionStatus reports TERMINATED; actual an empty roster ("expected <session id> to remain on the notebook's session roster after termination, got set()").
Cause 8: expected GetWorkGroup("primary") 200 with Name="primary"/State="ENABLED" and DeleteWorkGroup("primary") refused with InvalidRequestException "primary workGroup could not be deleted"; actual GetWorkGroup("primary") raises InvalidRequestException "WorkGroup is not found."
Cause 9: expected GetSessionStatus to report TERMINATED on its own within the configured idle window plus slack; actual last observed state='IDLE' ("session did not auto-terminate after its SessionIdleTimeoutInMinutes elapsed"), and the sibling check hit the harness ceiling ("Check timed out after 600.0s") still IDLE.

## Steps to Reproduce
Cause 1: athena.create_capacity_reservation(Name=<a>, TargetDpus=4) and wait for ACTIVE (control succeeds), then athena.create_capacity_reservation(Name=<b>, TargetDpus=1000000) — observe 200 instead of InvalidRequestException, then athena.get_capacity_reservation(Name=<b>) and see the reservation exists.
Cause 2: athena.create_data_catalog(Name="sim2real cat <x> cafe" (a name holding a space and non-ASCII letters), Type="GLUE", Parameters={"catalog-id": <account>}) and read Error.Message; also try a Name ending in "\x01" and compare with the modeled-pattern validation message.
Cause 3: cloudformation.create_stack with a template whose Resources contain a Type "AWS::Athena::CapacityReservation" — CreateStack is refused with ValidationError before any resource is created.
Cause 4: athena.start_query_execution(QueryString="UNLOAD (SELECT 1 AS col) TO 's3://<bucket>/unloaded/' WITH (format = 'JSON')", ResultConfiguration={"OutputLocation": "s3://<bucket>/results/"}), poll get_query_execution to a terminal state, then call get_query_results for the UNLOAD id.
Cause 5: athena.get_session_endpoint(SessionId=str(uuid4())) for the unknown-id path; and start a Spark session, call get_session_endpoint before it reaches IDLE and again once IDLE.
Cause 6: create a Spark-enabled workgroup and execution role, StartSession, then athena.list_executors(SessionId=<the returned id>); repeat after terminate_session.
Cause 7: create_notebook on a Spark workgroup, StartSession with EngineConfiguration.AdditionalConfigs={"NotebookId": <id>}, confirm list_notebook_sessions lists it, terminate_session, poll get_session_status to TERMINATED, then list_notebook_sessions again.
Cause 8: athena.get_work_group(WorkGroup="primary") on a fresh account; then athena.delete_work_group(WorkGroup="primary").
Cause 9: StartSession with SessionIdleTimeoutInMinutes=1 on a Spark workgroup, do nothing, and poll get_session_status past the one-minute window.
