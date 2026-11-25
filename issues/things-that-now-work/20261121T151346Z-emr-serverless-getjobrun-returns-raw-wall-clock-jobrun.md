Issue-Source: trace-sweep-divergence
Issue-Trace: emr-serverless-spark-job-run-writes-s3-output
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: emr-serverless-spark-job-run-writes-s3-output
Issue-Shape-Id: com.amazonaws.emrserverless#AwsToledoWebService
Issue-Key: emr-serverless|GetJobRun|2xx->2xx|$.jobRun.attemptCreatedAt
Issue-Key: emr-serverless|GetJobRun|2xx->2xx|$.jobRun.attemptUpdatedAt
Issue-Key: emr-serverless|GetJobRun|2xx->2xx|$.jobRun.createdAt
Issue-Key: emr-serverless|GetJobRun|2xx->2xx|$.jobRun.endedAt
Issue-Key: emr-serverless|GetJobRun|2xx->2xx|$.jobRun.startedAt
Issue-Key: emr-serverless|GetJobRun|2xx->2xx|$.jobRun.updatedAt
Issue-Commit: 2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92

# EMR Serverless GetJobRun returns raw wall-clock JobRun timestamps whose shape diverges from AWS

> _Note: this issue was identified against the code base at commit `2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetJobRun

## Description
One cause.

Cause 1 of 1: every timestamp member of the JobRun that GetJobRun returns diverges from the recorded AWS response at once (createdAt, updatedAt, startedAt, endedAt, attemptCreatedAt, attemptUpdatedAt). Fixture emr-serverless-spark-job-run-writes-s3-output, step task#8.

AWS: GetJobRun (GET /applications/{applicationId}/jobruns/{jobRunId}, 200) returns the required `jobRun` of shape JobRun. Smithy (services/service-com-amazonaws-emrserverless-awstoledowebservice/smithy-model): JobRun$createdAt and JobRun$updatedAt are @required, JobRun$startedAt ("The date and time when the job moved to the RUNNING state"), JobRun$endedAt ("The date and time when the job was terminated"), JobRun$attemptCreatedAt and JobRun$attemptUpdatedAt are optional; all six target com.amazonaws.emrserverless#Date, a bare `timestamp` with NO @timestampFormat, so this service's protocol default applies (service_info: protocols=[restJson1], sdkId "EMR Serverless", version 2021-07-13) — i.e. epoch-seconds as a JSON number. Beyond the type, the model states only the semantics quoted above; for the serialized granularity and for whether the attempt-scoped members are populated at all this is recorded behavior only: the fixture records each of these six as a shape-asserted value (volatile instants are deliberately not value-asserted) and the replay's shape check rejects all six of the emulator's values in one cluster. Hypothesis: AWS reports these at coarser-than-microsecond granularity and populates attemptCreatedAt/attemptUpdatedAt only for an attempt-bearing (retry-enabled) run, whereas the emulator always emits full sub-second precision and always populates the attempt pair.

Emulator: it serializes each member as the process wall clock verbatim, at microsecond precision, and derives three of them by aliasing/offsetting the others. Replay detail for task#8 (expected status 200, actual 200): `$.jobRun.createdAt: value_mismatch (expected '<shaped-not-asserted>', got 1795273815.978825)`, `$.jobRun.attemptCreatedAt: ... got 1795273815.978825`, `$.jobRun.startedAt: ... got 1795273818.978825`, `$.jobRun.endedAt: ... got 1795273826.763918`, `$.jobRun.updatedAt: ... got 1795273826.763918`, `$.jobRun.attemptUpdatedAt: ... got 1795273826.763918`. Note the structural signature, which is independent of the run: attemptCreatedAt is byte-identical to createdAt, attemptUpdatedAt and endedAt are byte-identical to updatedAt, and startedAt is createdAt plus exactly 3.000000s (the stage schedule's offset), all carrying six fractional digits. The absolute instants in the quoted values are per-run clock readings and are NOT a requirement; the requirement this cause is about is the serialized granularity of the number and the derivation of the attempt-scoped and lifecycle members.

Why: a client that reads these fields back gets values no real account produces. Code that parses an EMR Serverless timestamp into a type with second or millisecond resolution, or that round-trips it through a representation that drops digits, sees a value that does not compare equal to what it received; a client that treats a populated attemptCreatedAt/attemptUpdatedAt as evidence that the run has a retry attempt concludes the run was retried when it was not; and a client that measures queue or execution latency from endedAt-startedAt always gets the same synthetic 3s-anchored arithmetic rather than the run's real span, so timing-sensitive assertions pass against the emulator and fail against AWS (and vice versa).

Fix: in the job-run state that mints and settles these members, not the GetJobRun handler — GetJobRun is a pure report that clones the stored JobRun. The timestamps are taken straight from SystemTime at the granularity SystemTime offers, and the attempt-scoped pair is filled unconditionally from the run-level pair; the state should normalize the serialized granularity to what the service reports and populate attemptCreatedAt/attemptUpdatedAt only for a run that actually carries an attempt.

Traces: emr-serverless-spark-job-run-writes-s3-output#task#8 (GetJobRun, 200 vs 200, six diverging keys in one cluster). Related but NOT covering this: the open issues about EMR Serverless runs never executing (20261116T101705Z-emr-serverless-spark-job-runs-never-execute-the.md, 20261030T093552Z-emr-serverless-job-run-lifecycle-diverges-runs-never.md) are about the JobRunState/stateDetails a run settles in; this cause is about the shape and derivation of the JobRun timestamp members and the step's state field did not diverge.

## Evidence
Fixture emr-serverless-spark-job-run-writes-s3-output, step task#8, operation GetJobRun, verdict mismatch, expected_status 200, actual_status 200.

Expected (recorded AWS): each of $.jobRun.createdAt, $.jobRun.updatedAt, $.jobRun.startedAt, $.jobRun.endedAt, $.jobRun.attemptCreatedAt, $.jobRun.attemptUpdatedAt present as a shape-asserted epoch-seconds number whose shape the recording fixes (the instant itself is not value-asserted).

Actual (emulator): all six rejected in a single divergence cluster —
  $.jobRun.attemptCreatedAt: value_mismatch (expected '<shaped-not-asserted>', got 1795273815.978825)
  $.jobRun.attemptUpdatedAt: value_mismatch (expected '<shaped-not-asserted>', got 1795273826.763918)
  $.jobRun.createdAt:        value_mismatch (expected '<shaped-not-asserted>', got 1795273815.978825)
  $.jobRun.endedAt:          value_mismatch (expected '<shaped-not-asserted>', got 1795273826.763918)
  $.jobRun.startedAt:        value_mismatch (expected '<shaped-not-asserted>', got 1795273818.978825)
  $.jobRun.updatedAt:        value_mismatch (expected '<shaped-not-asserted>', got 1795273826.763918)
divergence_key: service=emr-serverless|GetJobRun|status=200->200|keys=$.jobRun.attemptCreatedAt,$.jobRun.attemptUpdatedAt,$.jobRun.createdAt,$.jobRun.endedAt,$.jobRun.startedAt,$.jobRun.updatedAt

Run-independent signature in the actual values: attemptCreatedAt == createdAt, attemptUpdatedAt == endedAt == updatedAt, startedAt == createdAt + 3.000000s, every value carrying six fractional digits.

Source of the values: services/service-com-amazonaws-emrserverless-awstoledowebservice/src/state.rs:2286-2288 (`now_timestamp(now) = DateTime::from(now)`), :854-859 (`started_at`/`ended_at` set to `DateTime::from(pending.due)` from the staged schedule), :964-966 (attempt_created_at/attempt_updated_at copied through); the handler itself only clones the stored run (src/ops/get_job_run.rs:181-183).

Model cited: smithy_query(query="shape", shape="JobRun") — createdAt/updatedAt @required, startedAt/endedAt/attemptCreatedAt/attemptUpdatedAt optional, all target `Date`; smithy_query(query="shape", shape="Date") — {"type": "timestamp"} with no timestampFormat trait; smithy_query(query="service_info") — protocols [restJson1].

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Replay fixture emr-serverless-spark-job-run-writes-s3-output up to step task#8: it creates an EMR Serverless application, submits a Spark job run, waits for the run to settle, then calls GetJobRun on it.
3. Inspect the 200 body of that GetJobRun call: $.jobRun.createdAt, updatedAt, startedAt, endedAt, attemptCreatedAt and attemptUpdatedAt are microsecond-precision epoch-seconds numbers (six fractional digits), attemptCreatedAt duplicates createdAt, attemptUpdatedAt duplicates endedAt/updatedAt, and startedAt is exactly createdAt + 3.000000s.
4. The replay reports step task#8 as a mismatch cluster over exactly those six keys while the status (200) matches.
