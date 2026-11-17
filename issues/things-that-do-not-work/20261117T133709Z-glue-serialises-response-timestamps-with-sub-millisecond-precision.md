Issue-Source: trace-sweep-divergence
Issue-Trace: glue-blueprint-run-illegal-state-rejected
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-blueprint-run-illegal-state-rejected glue-job-etl-unset-version-defaults-tags-provider-update-max-retries-and-not-found glue-job-python-shell-explicit-version-and-timeout-update-run-succeeds-and-not-found
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|GetBlueprint|2xx->2xx|$.Blueprint.CreatedOn
Issue-Key: glue|GetBlueprint|2xx->2xx|$.Blueprint.LastModifiedOn
Issue-Key: glue|GetJobRun|2xx->2xx|$.JobRun.CompletedOn
Issue-Key: glue|GetJobRun|2xx->2xx|$.JobRun.LastModifiedOn
Issue-Key: glue|GetJobRun|2xx->2xx|$.JobRun.StartedOn
Issue-Key: glue|GetJobRuns|2xx->2xx|$.JobRuns[*].CompletedOn
Issue-Key: glue|GetJobRuns|2xx->2xx|$.JobRuns[*].LastModifiedOn
Issue-Key: glue|GetJobRuns|2xx->2xx|$.JobRuns[*].StartedOn
Issue-Related: 20261117T133709Z-glue-read-paths-omit-or-misreport-members-aws.md
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# Glue serialises response timestamps with sub-millisecond precision

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBlueprint

## Description
Glue's awsJson timestamps are serialised as epoch-seconds with full floating-point precision (7 fractional digits) instead of AWS's millisecond-resolution epoch-seconds, so every timestamp member fails the recorded wire-shape assertion. This is a serialisation-format divergence, not a clock-value difference: the recorded fixtures assert the SHAPE of these members (they are '<shaped-not-asserted>'), and the shape AWS emits is epoch-seconds with at most 3 fractional digits (the corpus's recorded Glue timestamps are of the form 1791555590.752). The emulator emits values such as 1794921864.7132862. The same defect appears on four read paths below; it is one serializer.

Cause 1 of 4: GetBlueprint — Blueprint.CreatedOn and Blueprint.LastModifiedOn.
AWS: Smithy: Glue's protocol is awsJson, timestamps are epoch-seconds; the recorded responses carry millisecond resolution (3 fractional digits).
Emulator: emits 7 fractional digits, so both members fail the shape assertion.
Why: a client that formats, hashes or compares the serialised value (or that round-trips it into another API) sees a value of a form AWS never emits; strict SDK/date parsers and recorded-shape assertions reject it.
Fix: the shared timestamp serializer for Glue responses (truncate/round epoch-seconds to millisecond resolution).
Traces: glue-blueprint-run-illegal-state-rejected#task#4.

Cause 2 of 4: GetJob — Job.CreatedOn and Job.LastModifiedOn.
AWS: as above, millisecond-resolution epoch-seconds.
Emulator: 7 fractional digits on both members.
Fix: the same timestamp serializer, reached through the GetJob output path.
Traces: glue-job-etl-unset-version-defaults-tags-provider-update-max-retries-and-not-found#task#12 (cluster also covers the python-shell job fixture's GetJob reads).

Cause 3 of 4: GetJobRun — JobRun.StartedOn, JobRun.CompletedOn and JobRun.LastModifiedOn.
AWS: as above.
Emulator: 7 fractional digits on all three.
Fix: the same serializer on the job-run output path.
Traces: glue-job-python-shell-explicit-version-and-timeout-update-run-succeeds-and-not-found#task#10.

Cause 4 of 4: GetJobRuns — JobRuns[].StartedOn, .CompletedOn and .LastModifiedOn.
AWS: as above.
Emulator: 7 fractional digits on every element.
Fix: the same serializer on the job-runs list output path.
Traces: glue-job-python-shell-explicit-version-and-timeout-update-run-succeeds-and-not-found#task#11.

## Evidence
All four replayed at status 200; each member's recorded expectation is the wire SHAPE ('<shaped-not-asserted>'), which the emitted value fails:
1. glue-blueprint-run-illegal-state-rejected#task#4 GetBlueprint — $.Blueprint.CreatedOn and $.Blueprint.LastModifiedOn got 1794921802.1095562.
2. glue-job-etl-unset-version-defaults-tags-provider-update-max-retries-and-not-found#task#12 GetJob — $.Job.CreatedOn got 1794921864.7132862, $.Job.LastModifiedOn got 1794921864.7730494.
3. glue-job-python-shell-explicit-version-and-timeout-update-run-succeeds-and-not-found#task#10 GetJobRun — $.JobRun.StartedOn got 1794921785.3616254, $.JobRun.CompletedOn and $.JobRun.LastModifiedOn got 1794921791.3616254.
4. glue-job-python-shell-explicit-version-and-timeout-update-run-succeeds-and-not-found#task#11 GetJobRuns — $.JobRuns[0].StartedOn/.CompletedOn/.LastModifiedOn, same values.
For contrast, a recorded AWS Glue timestamp in the same corpus (glue-gjwr-traj-start-the-copied-job-fails-before-provisioning, JobRun.CompletedOn) is 1791555590.752 — three fractional digits. The specific instants above are per-run values and are not the requirement; the number of fractional digits is.

## Steps to Reproduce
1. Replay the wire corpus (replay_wire_corpus).
2. CreateBlueprint then GetBlueprint; CreateJob then GetJob; StartJobRun then GetJobRun and GetJobRuns.
3. Inspect the raw JSON body (not the SDK-parsed datetime): every timestamp member is an epoch-seconds double with 7 fractional digits, where AWS serialises at most 3.
