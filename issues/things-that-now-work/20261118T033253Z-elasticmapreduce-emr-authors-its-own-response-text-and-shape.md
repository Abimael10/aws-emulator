Issue-Source: trace-sweep-divergence
Issue-Trace: elasticmapreduce-cluster-mutations-on-unknown-cluster
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticmapreduce-cluster-mutations-on-unknown-cluster elasticmapreduce-multi-master-topology-release-and-type-mistakes elasticmapreduce-perf-diagnosis-spark-cluster-recreate-refusals elasticmapreduce-run-job-flow-two-bootstrap-actions-ordered
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: elasticmapreduce|SetVisibleToAllUsers|4xx->4xx|$.message
Issue-Key: elasticmapreduce|RunJobFlow|4xx->4xx|$.message
Issue-Key: elasticmapreduce|ListBootstrapActions|2xx->2xx|$.BootstrapActions[*].Args
Issue-Commit: 323f4af0304d555aad3c797a5f8d6448948dc50c

# EMR authors its own response text and shape: reworded refusal messages and an omitted empty Args list

> _Note: this issue was identified against the code base at commit `323f4af0304d555aad3c797a5f8d6448948dc50c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetVisibleToAllUsers

## Description
This issue bundles 3 net-new root causes in the EMR (service token `elasticmapreduce`) crate that share ONE fault: the emulator serves a response body it authored itself instead of the body real EMR serves for that case — two refusal messages are reworded (wrong sentence template, or AWS's variable parts dropped) and one 200 read path omits a list member AWS always serializes. Status codes and exception types match in all three; only the authored text/shape diverges.

In each cause below the FIXED text of the message is the requirement and the variable parts come from the request or from emulator state, as marked; the concrete ids, instance types and release labels in the recordings are per-run request values, not part of the template.

Cause 1 of 3: SetVisibleToAllUsers refuses an unknown job flow with the wrong sentence template.
AWS: Smithy (com.amazonaws.emr#ElasticMapReduce): SetVisibleToAllUsers(JobFlowIds required, VisibleToAllUsers required); the model declares only InternalServerError, so the 400 refusal text is recorded behavior only. The recording shows a well-formed but nonexistent job flow id refused with HTTP 400 and message `Job flow id '<JobFlowId>' is not valid.` — fixed text `Job flow id '` and `' is not valid.`, variable part: the id taken from the request's JobFlowIds.
Emulator: HTTP 400 with the CONSTANT message `Specified job flow ID not valid.` — i.e. the template EMR reserves for AddJobFlowSteps / TerminateJobFlows, with no id at all.
Why: a client, CLI wrapper or runbook that classifies the refusal by message (the message is the only human-visible discriminator on this exception) does not recognise it, and loses the identifier AWS reports, so it cannot tell which of several submitted JobFlowIds was rejected.
Fix: the unknown-job-flow branch of the SetVisibleToAllUsers handler in the EMR crate's operation handlers — it must use the `Job flow id '<id>' is not valid.` template rather than the shared constant job-flow sentence. (The open issue 20261029T212242Z-elasticmapreduce-emr-unknown-id-refusal-messages-are-reworded-or.md describes the same family of per-operation message mix-ups and is the reason this is one shared helper, but it does NOT cover SetVisibleToAllUsers and carries no replay key for this step.)
Traces: elasticmapreduce-cluster-mutations-on-unknown-cluster#task#7.

Cause 2 of 3: RunJobFlow's unsupported-instance-type refusal drops the release label and quotes the type.
AWS: Smithy: RunJobFlow(Name required, Instances required, ReleaseLabel optional); the model declares only InternalServerError, so the refusal text is recorded behavior only. The recording shows an instance type that is not supported for the requested release refused with HTTP 400 and message `Instance type <InstanceType> not supported for release <ReleaseLabel>.` — fixed text `Instance type `, ` not supported for release ` and the trailing `.`; variable parts: the instance type from the request's Instances configuration and the ReleaseLabel from the request. The type is NOT quoted, and the release label is part of the sentence because the support decision depends on it.
Emulator: HTTP 400 with `Instance type '<InstanceType>' is not supported.` — it single-quotes the type, says `is not supported` instead of `not supported`, and omits the release label entirely.
Why: the refusal is stated as an absolute property of the instance type rather than as a type/release combination, so a caller is told the type can never be used instead of being told to pick a different release; message-matching clients also fail on the quoting and the altered verb phrase.
Fix: the instance-type/release compatibility validation for RunJobFlow in the EMR crate — the message must be rendered from both the instance type and the resolved release label.
Traces: elasticmapreduce-multi-master-topology-release-and-type-mistakes#task#9 (also reproduced at elasticmapreduce-perf-diagnosis-spark-cluster-recreate-refusals#task#13).

Cause 3 of 3: ListBootstrapActions omits Args when a bootstrap action has no arguments.
AWS: Smithy: ListBootstrapActions(ClusterId required) -> BootstrapActions: CommandList of com.amazonaws.emr#Command{Name, ScriptPath, Args: StringList}. The recording shows that for a bootstrap action created with no arguments AWS still serializes `Args` as an EMPTY LIST (`"Args": []`) on every element, rather than leaving the member out.
Emulator: the element for the argument-less bootstrap action has no `Args` member at all (null/absent), while the element that does have arguments carries it.
Why: the member's presence becomes conditional, so a client that reads `BootstrapActions[i].Args` positionally or iterates it without a null guard sees a missing key / null instead of an empty collection and either throws or treats the action as malformed; AWS's response shape is uniform across elements.
Fix: the ListBootstrapActions response serializer (the Command projection) in the EMR crate — emit `Args` as an empty list when the stored bootstrap action has no arguments instead of skipping the member.
Traces: elasticmapreduce-run-job-flow-two-bootstrap-actions-ordered#task#10.

## Evidence
Replay of wire corpus simparity-wire-aws-v1 against the booted emulator (summary: 390 steps, 354 passed, score 90.8%, emulator_up true). All three clusters below are net-new: no stub_path and no owning open issue.

Cause 1 — fixture elasticmapreduce-cluster-mutations-on-unknown-cluster, step task#7, SetVisibleToAllUsers, expected status 400 / actual 400:
  `$.message: value_mismatch (expected "Job flow id 'j-3SD91U2P1PANK' is not valid.", got 'Specified job flow ID not valid.')`
  The `j-3SD91U2P1PANK` id is a per-run request value; the requirement is the `Job flow id '<id>' is not valid.` template with the request's id interpolated.

Cause 2 — fixture elasticmapreduce-multi-master-topology-release-and-type-mistakes, step task#9, RunJobFlow, expected status 400 / actual 400:
  `$.message: value_mismatch (expected 'Instance type m7g.xlarge not supported for release emr-6.0.0.', got "Instance type 'm7g.xlarge' is not supported.")`
  Same divergence at fixture elasticmapreduce-perf-diagnosis-spark-cluster-recreate-refusals, step task#13. `m7g.xlarge` and `emr-6.0.0` are the request's values; the requirement is that both appear in AWS's sentence, unquoted.

Cause 3 — fixture elasticmapreduce-run-job-flow-two-bootstrap-actions-ordered, step task#10, ListBootstrapActions, expected status 200 / actual 200:
  `$.BootstrapActions[1].Args: missing (expected [], got None)`

## Steps to Reproduce
Boot the emulator and replay these steps from wire trace set simparity-wire-aws-v1 (service token `elasticmapreduce`):
1. Fixture `elasticmapreduce-cluster-mutations-on-unknown-cluster`, step task#7: call SetVisibleToAllUsers with a well-formed but nonexistent job flow id and VisibleToAllUsers set; assert HTTP 400 and that the message is `Job flow id '<the requested id>' is not valid.`
2. Fixture `elasticmapreduce-multi-master-topology-release-and-type-mistakes`, step task#9 (and `elasticmapreduce-perf-diagnosis-spark-cluster-recreate-refusals`, step task#13): call RunJobFlow with an instance type that is not supported for the requested ReleaseLabel; assert HTTP 400 and that the message is `Instance type <type> not supported for release <releaseLabel>.` naming both values from the request.
3. Fixture `elasticmapreduce-run-job-flow-two-bootstrap-actions-ordered`, step task#10: create a cluster with two bootstrap actions, one of which has no script arguments, then call ListBootstrapActions; assert every returned element carries `Args`, with `[]` for the argument-less action.
