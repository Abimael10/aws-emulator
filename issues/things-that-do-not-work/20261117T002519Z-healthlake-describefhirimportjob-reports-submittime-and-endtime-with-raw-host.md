Issue-Source: trace-sweep-divergence
Issue-Trace: healthlake-fhir-import-job-loads-ndjson-patient
Issue-Step: task#9
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: healthlake-fhir-import-job-loads-ndjson-patient
Issue-Shape-Id: com.amazonaws.healthlake#HealthLake
Issue-Key: healthlake|DescribeFHIRImportJob|2xx->2xx|$.ImportJobProperties.EndTime
Issue-Key: healthlake|DescribeFHIRImportJob|2xx->2xx|$.ImportJobProperties.SubmitTime
Issue-Commit: e014230fb9b53a8f1acc538e4bd0b4dbf598fc56

# DescribeFHIRImportJob reports SubmitTime and EndTime with raw host-clock sub-millisecond precision, diverging from the HealthLakeTimestamp wire shape

> _Note: this issue was identified against the code base at commit `e014230fb9b53a8f1acc538e4bd0b4dbf598fc56`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeFHIRImportJob

## Description
One cause.

Cause 1 of 1: the two timestamp members of ImportJobProperties (SubmitTime, EndTime) are serialized in a shape HealthLake never emits, so a shaped (value-insensitive) assertion on both members fails even though the response status and the rest of the body are correct. Fixture healthlake-fhir-import-job-loads-ndjson-patient, step task#9.

AWS: HealthLake is awsJson1_0 (Smithy: service HealthLake, version 2017-07-01, protocols [awsJson1_0]; smithy_query service_info). DescribeFHIRImportJob returns 200 with the required member ImportJobProperties (Smithy: DescribeFHIRImportJob.output ImportJobProperties required). Inside it, SubmitTime (required) and EndTime target com.amazonaws.healthlake#HealthLakeTimestamp, a plain timestamp shape that carries NO @timestampFormat trait, so the awsJson1_0 default applies: epoch-seconds as a JSON number. HealthLake emits those epoch-seconds at the precision the service records a job transition at - whole or millisecond-resolution seconds (at most three fractional digits) - which is the shape the recorded response carries and the shape the replay asserted against. The recorded value itself was deliberately NOT asserted (the expectation is the placeholder '<shaped-not-asserted>'), so this is a divergence in the REPRESENTATION of the timestamp, not clock drift between the recording and the replay. Docs: ImportJobProperties.SubmitTime "The time the import job was submitted for processing", ImportJobProperties.EndTime "The time the import job was completed".

Emulator: the replay reports, at 200/200, `$.ImportJobProperties.SubmitTime: value_mismatch (expected '<shaped-not-asserted>', got 1794874948.8008265)` and `$.ImportJobProperties.EndTime: value_mismatch (expected '<shaped-not-asserted>', got 1794874949.3009233)`. Both are raw f64 epoch seconds carrying the host wall clock's full sub-microsecond fraction (seven fractional digits), because the record stores `::aws_smithy_types::DateTime::from(SystemTime)` unrounded and the response serializer renders every fractional digit it holds. The pair is also only ~0.5s apart, i.e. the whole import is stamped within one emulated run span - consistent with the job's CHOSEN transition span - but the divergence the replay flags is the fractional precision, which no AWS HealthLake response carries.

Why: a client that relies on AWS's timestamp shape gets the wrong thing from this field. Consumers that re-serialize the value, compare it against a timestamp obtained elsewhere, hash or snapshot the response body, or feed it to a decoder that accepts only epoch seconds at second/millisecond resolution see a value AWS would never return; anything that round-trips SubmitTime/EndTime through a millisecond-precision representation and compares for equality with the served value diverges. The affected step also stops the recorded scenario: the remaining steps of this fixture are reported blocked.

Fix: in the DescribeFHIRImportJob response path - the state's ImportJob timestamp fields and the single place they are stamped - normalize HealthLakeTimestamp values to the precision AWS reports (do not propagate the host clock's nanosecond fraction into the epoch-seconds number). The same stamping pattern is shared by ExportJob, TransformationJob and Datastore.created_at in this service's state, so the same normalization is owed to ListFHIRImportJobs, DescribeFHIRExportJob/ListFHIRExportJobs, DescribeDataTransformationJob/ListDataTransformationJobs and DescribeFHIRDatastore, which report the same values off the same records.

Traces: healthlake-fhir-import-job-loads-ndjson-patient#task#9 (DescribeFHIRImportJob, verdict mismatch, expected 200 / actual 200, keys $.ImportJobProperties.SubmitTime and $.ImportJobProperties.EndTime).

No open issue covers this: the open healthlake issues concern TransformData having no handler, DatastoreEndpoint publishing an unserved host / POST /transform-data, the connecthealth behavioral surface and CloudFormation HealthLake resource types - none of them names this fixture step or the ImportJobProperties timestamp shape.

## Evidence
Replay of fixture healthlake-fhir-import-job-loads-ndjson-patient, step task#9, operation DescribeFHIRImportJob, service healthlake (verdict: mismatch, expected_status 200, actual_status 200).

Expected (shape-only assertion, value deliberately not asserted):
  $.ImportJobProperties.SubmitTime -> an awsJson1_0 epoch-seconds number at the resolution HealthLake reports (whole or millisecond seconds, at most 3 fractional digits)
  $.ImportJobProperties.EndTime   -> same

Actual:
  $.ImportJobProperties.SubmitTime = 1794874948.8008265
  $.ImportJobProperties.EndTime    = 1794874949.3009233

Replay detail, verbatim:
  $.ImportJobProperties.EndTime: value_mismatch (expected '<shaped-not-asserted>', got 1794874949.3009233)
  $.ImportJobProperties.SubmitTime: value_mismatch (expected '<shaped-not-asserted>', got 1794874948.8008265)

Model evidence: ImportJobProperties.SubmitTime (required) and ImportJobProperties.EndTime target com.amazonaws.healthlake#HealthLakeTimestamp, a timestamp shape with no @timestampFormat; service protocol awsJson1_0, so epoch-seconds is the default serialization. The specific epoch values above are per-run clock values and are NOT the requirement - the fractional precision of the serialized number is.

Code evidence: services/service-com-amazonaws-healthlake-healthlake/src/state.rs:1445-1459 (ImportJob::properties maps submit_time: self.submitted_at, end_time: self.ended_at), :1404 and :1409 (submitted_at / ended_at held as ::aws_smithy_types::DateTime), :1482 (ended_at = DateTime::from(due_at), the unrounded wall-clock instant); handler services/service-com-amazonaws-healthlake-healthlake/src/ops/describe_fhir_import_job.rs:199-201.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Fixture healthlake-fhir-import-job-loads-ndjson-patient: create a HealthLake data store, start a FHIR import job against it with StartFHIRImportJob, then call DescribeFHIRImportJob with that DatastoreId/JobId once the job has settled to a terminal status (step task#9).
3. Inspect the 200 response body's ImportJobProperties.SubmitTime and ImportJobProperties.EndTime.
4. Observe both are epoch-seconds numbers carrying the host clock's full fractional precision (e.g. 1794874948.8008265 / 1794874949.3009233, seven fractional digits), where HealthLake reports epoch seconds at second/millisecond resolution; the shaped assertion on both members fails and the fixture's remaining steps are blocked.
