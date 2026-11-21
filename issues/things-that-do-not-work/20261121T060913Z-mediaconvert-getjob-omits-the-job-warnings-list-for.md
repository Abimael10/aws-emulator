Issue-Source: trace-sweep-divergence
Issue-Trace: mediaconvert-job-transcodes-to-complete-with-output
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mediaconvert-job-transcodes-to-complete-with-output
Issue-Shape-Id: com.amazonaws.mediaconvert#MediaConvert
Issue-Key: mediaconvert|GetJob|2xx->2xx|$.job.warnings[*]
Issue-Commit: 9089d6afeee8a152e22effcbb2088e0285f20b22

# mediaconvert GetJob omits the job warnings list for a completed transcode

> _Note: this issue was identified against the code base at commit `9089d6afeee8a152e22effcbb2088e0285f20b22`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetJob

## Description
One cause.

Cause 1 of 1: GetJob returns a Job object with no `warnings` member for a job the emulator ran to COMPLETE, where AWS returns the warning groups the transcode accumulated.

AWS: Smithy (services/service-com-amazonaws-mediaconvert-mediaconvert/smithy-model/com-amazonaws-mediaconvert-mediaconvert.json) binds GetJob to GET /2017-08-29/jobs/{Id} with success code 200 and output member Job (jsonName "job") targeting mediaconvert#Job. Job.Warnings (jsonName "warnings") targets __listOfWarningGroup, documented "Contains any warning messages for the job. Use to help identify potential issues with your input, output, or job." Each element is mediaconvert#WarningGroup, a structure whose two required members are Code (jsonName "code", __integer, "Warning code that identifies a specific warning in the job") and Count (jsonName "count", __integer, "The number of times this warning occurred in the job"). So for a job whose transcode raised warnings, the 200 body carries job.warnings as a JSON array of {"code": <integer warning code>, "count": <integer occurrences>} objects — the codes and counts are values derived from what the job's inputs/outputs actually triggered, not fixed text. The recording confirms the shape: job.warnings[0] = {"code": 230001, "count": 1} and job.warnings[1] = {"code": 230005, "count": 1} on the COMPLETE job. Status is 200 on both sides; only the body member diverges.

Emulator: the same request answers 200 but the serialized Job has no `warnings` key at all — replay reports `$.job.warnings[0]: missing (expected {'code': 230001, 'count': 1}, got None)` and `$.job.warnings[1]: missing (expected {'code': 230005, 'count': 1}, got None)`. The emulator's job state never records warning groups while it advances a job through PROBING/TRANSCODING to COMPLETE, so GetJob has nothing to project and emits the Job without the member.

Why: a client that polls GetJob until status COMPLETE and then reads job.warnings to surface or gate on transcode warnings (the documented way to identify potential problems with an input, output or job setting) sees an unconditionally warning-free job. Pipelines that fail, alert, or re-encode on specific MediaConvert warning codes never fire against the emulator, and code that iterates job.warnings must tolerate an absent key where AWS supplies the list — so the absence is indistinguishable from a genuinely clean job and the warning signal is lost entirely.

Fix: the MediaConvert job state (the job progression that should accumulate WarningGroup entries, keyed by warning code with an occurrence count, as it probes the input and writes outputs) plus the GetJob response serializer that must project Job.Warnings as job.warnings with each element's `code` and `count`. A job that genuinely raised no warnings may continue to omit the member; a job whose simulated transcode did raise them must report them, and ListJobs/CreateJob's Job projection must use the same accumulated state so the three views agree.

Traces: mediaconvert-job-transcodes-to-complete-with-output#task#6 (GetJob), affected_count 1.

## Evidence
Fixture mediaconvert-job-transcodes-to-complete-with-output, step task#6, service mediaconvert, operation GetJob, verdict mismatch, expected_status 200 / actual_status 200.

Diverging body members (replay detail, verbatim):
  $.job.warnings[0]: missing (expected {'code': 230001, 'count': 1}, got None)
  $.job.warnings[1]: missing (expected {'code': 230005, 'count': 1}, got None)

divergence_key: service=mediaconvert|GetJob|status=200->200|keys=$.job.warnings[0],$.job.warnings[1]

Expected: a 200 GetJob body whose job.warnings is a non-empty array of WarningGroup objects, each with integer `code` and integer `count` (the recorded run shows codes 230001 and 230005, each with count 1 — those particular codes/counts are values from that run's inputs, the requirement is that the member is present and populated with the warnings the job raised).
Actual: a 200 GetJob body with no job.warnings key (both indices resolve to None).

Model citations: GetJob -> GET /2017-08-29/jobs/{Id}, 200, output {Job: jsonName "job"}; Job.Warnings -> __listOfWarningGroup, jsonName "warnings"; WarningGroup.Code (required, jsonName "code", __integer) and WarningGroup.Count (required, jsonName "count", __integer).

## Steps to Reproduce
1. Replay the wire corpus: replay_wire_corpus() against the emulator built from this checkout.
2. Fixture mediaconvert-job-transcodes-to-complete-with-output, step task#6: the fixture creates a MediaConvert job, lets it progress to status COMPLETE, then issues GetJob on the job id.
3. Read the 200 JSON body's job.warnings and compare against the recording: the recorded job carries two WarningGroup entries ({"code":230001,"count":1}, {"code":230005,"count":1}); the emulator's body has no warnings member at all.
4. Minimal repro: CreateJob, poll GetJob until "status":"COMPLETE", and inspect job.warnings — it is absent rather than a populated list of {code,count} objects.
