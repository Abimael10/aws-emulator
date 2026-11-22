Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue.test-connection-rejects-incomplete-jdbc-properties
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue.test-connection-rejects-incomplete-jdbc-properties|run#4|AssertionError|assert msg1 == "TestConnection is only supported for the Unified Connections", (
Issue-Commit: f32b2b9b20ce4b4501c6c563e36b233b987e4db8

# Glue TestConnection words the inline-TestConnectionInput unified-only refusal with the stored-connection sentence (one shared constant for two differently-worded AWS messages)

> _Note: this issue was identified against the code base at commit `f32b2b9b20ce4b4501c6c563e36b233b987e4db8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TestConnection

## Description
One net-new root cause in the Glue control plane: a message-text divergence on a refusal whose HTTP status (400) and error code (ValidationException) the emulator already gets right. ONE cause must be closed before this issue is submitted.

Cause 1 of 1: TestConnection called with an INLINE TestConnectionInput whose ConnectionType is outside the Unified Connections surface is refused with the wrong message text, because the emulator renders ONE shared constant for two refusal branches AWS words differently.

AWS: Smithy (com.amazonaws.glue#TestConnection, services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json) models input {ConnectionName, CatalogId, TestConnectionInput} and declares the client errors AccessDeniedException, ConflictException, EntityNotFoundException, FederationSourceException, GlueEncryptionException, InvalidInputException, OperationTimeoutException, ResourceNumberLimitExceededException (all 400) plus InternalServiceException (500); ValidationException is a service-level shape the operation's own errors list omits, so the restriction and its wording are recorded behavior only. The recording shows the restriction is reported with TWO DIFFERENT sentences, one per request form, each entirely fixed text with no variable part taken from the request or from state:
  - inline form: TestConnection(TestConnectionInput={ConnectionType:'JDBC', ...}) -> HTTP 400, Error.Code=ValidationException, Error.Message="TestConnection is only supported for the Unified Connections" — the subject is the OPERATION'S NAME.
  - stored form: TestConnection(ConnectionName=<an existing non-unified connection>) -> HTTP 400, Error.Code=ValidationException, Error.Message="This operation is only supported for the Unified Connections" — the subject is the generic "This operation" (this branch is the one open issue 20261119T084143Z-glue-refusal-messages-diverge-startcrawlerschedule-s-entitynotfoundexception-and.md carries, via check glue.test-connection-unknown-name-rejected).
Neither sentence carries a trailing period.

Emulator: both branches render the SINGLE constant crate::state::messages::TEST_CONNECTION_UNIFIED_ONLY = "This operation is only supported for the Unified Connections", so the INLINE branch answers HTTP 400 / ValidationException with "This operation is only supported for the Unified Connections" where AWS says "TestConnection is only supported for the Unified Connections". Replay detail: AssertionError: unexpected message: 'This operation is only supported for the Unified Connections'. The check's preceding assertion on the code (ValidationException) passes, so only the message diverges. Context, so no one re-files this as the already-fixed defect: the resolved issue 20261117T183344Z-glue-6-net-new-causes-cancelmltaskrun-updatecatalog-startcrawlerschedule.md carried this same check and fixed the CODE (the call used to answer the unmodeled 'NotImplemented'); the residual divergence here is the inline branch's wording, which is NOT the stored-branch wording open issue 20261119T084143Z pins. The two branches cannot both be served by one string.

Why: the two refusals are the only signal a caller gets for 'this connection kind is not testable here', and clients that key on the message (CLI/SDK users, log scrapers, golden-output tests, and automation that distinguishes the type-level restriction reported for an inline input from the one reported for a stored connection) read text AWS never emits for an inline TestConnectionInput; the inline and stored branches also become indistinguishable by message, which they are not in AWS.

Fix: the TestConnection handler's inline-TestConnectionInput validation and the message layer it calls — the one shared message constant must become two, the operation-named sentence for the inline branch and the generic "This operation" sentence for the stored-connection branch, so neither branch's recorded wording is lost when the other is corrected.

Traces: glue.test-connection-rejects-incomplete-jdbc-properties. Handler: services/service-com-amazonaws-glue-awsglue/src/ops/test_connection.rs lines 229-238 (inline branch) and 324-333 (stored-connection branch), both rendering services/service-com-amazonaws-glue-awsglue/src/state/messages.rs lines 1249-1250. The check's second half (an inline SALESFORCE TestConnectionInput declaring AuthenticationType=OAUTH2 with no OAuth2Properties, expected 400 InvalidInputException with '[AuthenticationConfiguration.OAuth2Properties is required for OAUTH2 AuthenticationType.]') is never reached because the first assertion fails, so it is ungraded; it must also hold once the message above is corrected, and the check additionally asserts the two codes DIFFER.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 586/724 passed. The failing check is listed under service heading 'glue' with NO 'OWNED by' owner and NO 'ALREADY FILED' marker.

Cause 1 — glue.test-connection-rejects-incomplete-jdbc-properties [mismatch], check_glue_test_connection_rejects_incomplete_jdbc_properties.py line 40:
  Request: TestConnection(TestConnectionInput={ConnectionType:'JDBC', ConnectionProperties:{JDBC_CONNECTION_URL:'jdbc:mysql://<host>:<port>/<db>', USERNAME:<user>, PASSWORD:<password>}}) — no ConnectionName.
  Expected (AWS): HTTP 400, Error.Code=ValidationException, Error.Message="TestConnection is only supported for the Unified Connections" (the whole string is fixed text; nothing in it comes from the request or from state).
  Actual (emulator): HTTP 400, Error.Code=ValidationException, Error.Message="This operation is only supported for the Unified Connections".
  Replay line: `assert msg1 == "TestConnection is only supported for the Unified Connections"` -> AssertionError: unexpected message: 'This operation is only supported for the Unified Connections'.

Source evidence: services/service-com-amazonaws-glue-awsglue/src/state/messages.rs:1249-1250 defines a single constant TEST_CONNECTION_UNIFIED_ONLY = "This operation is only supported for the Unified Connections"; services/service-com-amazonaws-glue-awsglue/src/ops/test_connection.rs renders it at lines 233 (inline TestConnectionInput branch) and 328 (stored ConnectionName branch).

Model evidence (com-amazonaws-glue-awsglue.json): TestConnection errors = [AccessDeniedException, ConflictException, EntityNotFoundException, FederationSourceException, GlueEncryptionException, InternalServiceException, InvalidInputException, OperationTimeoutException, ResourceNumberLimitExceededException]; the operation's own list declares no ValidationException, so the refusal class is the service-level shape and its text is pinned only by the recordings.

## Steps to Reproduce
1. Clone this checkout and boot the emulator built from it.
2. Call glue.TestConnection with an inline input and no ConnectionName:
   TestConnection(TestConnectionInput={ConnectionType:'JDBC', ConnectionProperties:{JDBC_CONNECTION_URL:'jdbc:mysql://198.51.100.10:3306/exampledb', USERNAME:'example-user', PASSWORD:'example-password'}})
   Observed: 400 ValidationException with Message 'This operation is only supported for the Unified Connections'.
   Expected: 400 ValidationException with Message 'TestConnection is only supported for the Unified Connections'.
3. Confirm the OTHER branch still words it AWS's way (it must not regress while fixing step 2): CreateConnection(ConnectionInput={Name:<n>, ConnectionType:'JDBC', ConnectionProperties:{...}}), then TestConnection(ConnectionName=<n>).
   Expected: 400 ValidationException with Message 'This operation is only supported for the Unified Connections'.
4. Call TestConnection(TestConnectionInput={ConnectionType:'SALESFORCE', ConnectionProperties:{}, AuthenticationConfiguration:{AuthenticationType:'OAUTH2'}}).
   Expected: 400 InvalidInputException with Message '[AuthenticationConfiguration.OAuth2Properties is required for OAUTH2 AuthenticationType.]', i.e. a DIFFERENT code from step 2.
5. Re-replay glue.test-connection-rejects-incomplete-jdbc-properties (and, as a regression guard, glue.test-connection-unknown-name-rejected) to confirm the fix.
