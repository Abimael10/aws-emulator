Issue-Source: trace-sweep-divergence
Issue-Trace: autoscaling-asgdx-traj-standby-park-and-return
Issue-Step: task#13
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscaling-asgdx-traj-standby-park-and-return autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|ExitStandby|2xx->2xx|$.ExitStandbyResponse.ExitStandbyResult.Activities.member.Cause
Issue-Key: autoscaling|ExitStandby|2xx->2xx|$.ExitStandbyResponse.ExitStandbyResult.Activities.member.Details
Issue-Key: autoscaling|ExitStandby|2xx->2xx|$.ExitStandbyResponse.ExitStandbyResult.Activities.member.Progress
Issue-Key: autoscaling|ExitStandby|2xx->2xx|$.ExitStandbyResponse.ExitStandbyResult.Activities.member.StatusCode
Issue-Key: autoscaling|DescribeScalingActivities|2xx->2xx|$.DescribeScalingActivitiesResponse.DescribeScalingActivitiesResult.Activities.member[*]
Issue-Commit: addb8a7148576d7d08097a9e3c6488eb5c13818f

# autoscaling scaling-activity records omit Details and report emulator status values instead of AWS's

> _Note: this issue was identified against the code base at commit `addb8a7148576d7d08097a9e3c6488eb5c13818f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExitStandby

## Description
2 causes. The emulator synthesizes Auto Scaling `Activity` records from its own vocabulary rather than Auto Scaling's: it never populates the `Details` member, and it picks `StatusCode`/`Progress`/`Cause` values that are not the ones Auto Scaling records for the transition in question. Both causes below are instances of that one fault in the activity writer.

Cause 1 of 2: ExitStandby returns an activity with the wrong StatusCode, Progress, Cause and no Details.
AWS: Smithy (com.amazonaws.autoscaling) models `Activity` with members `StatusCode` -> enum `ScalingActivityStatusCode` whose value set includes `PreInService`, `InProgress`, `Successful`, `Failed`, `Cancelled`; `Progress` -> integer `Progress` (0..100); `Cause` -> XmlStringMaxLen1023 ("The reason the activity began"); and `Details` -> XmlString ("The details about the activity"). For the move of an instance out of Standby, the recording shows AWS answering `StatusCode` = `PreInService` (the instance is being returned to service, it is not yet InService), `Progress` = `30`, a `Details` member carrying the JSON placement document whose keys are `Availability Zone ID`, `Subnet ID` and `Availability Zone` (their values are the instance's actual AZ id, subnet id and AZ), and a `Cause` whose fixed text is `At <timestamp> instance <instance-id> was moved out of standby in response to a user request, increasing the capacity from <old> to <new>.` — the timestamp, instance id and the two capacity numbers are variable parts taken from the request and the group's state.
Emulator: answers 200 but with `StatusCode` = `InProgress`, `Progress` = `50`, no `Details` member at all, and `Cause` = `a user request explicitly moved an instance of the group out of Standby: changing the desired capacity from 0 to 1.` — emulator-authored prose in the group-capacity-change vocabulary rather than the ExitStandby sentence.
Why: a client that polls the returned ActivityId and keys off `StatusCode` to decide whether the instance is pre-service versus mid-flight sees a state Auto Scaling never reports for this transition; a client that parses `Details` for the placement (AZ id / subnet) of the returning instance finds the member absent and cannot resolve placement at all.
Fix: the ExitStandby handler's activity construction (the activity-record writer in the autoscaling service crate's standby path) — it must emit the PreInService/30 pair, the placement `Details` document, and ExitStandby's own Cause sentence.
Traces: autoscaling-asgdx-traj-standby-park-and-return#task#13.

Cause 2 of 2: DescribeScalingActivities entries carry no Details member for launch and terminate activities.
AWS: Smithy models `Activity.Details` (XmlString) on every activity. The recording shows both the `Launching a new EC2 instance: <instance-id>` and `Terminating EC2 instance: <instance-id>` activities of a launch-template group carrying `Details` set to the JSON placement document with keys `Availability Zone ID`, `Subnet ID`, `Availability Zone` — values taken from where the instance was actually placed.
Emulator: returns both activities with `StatusCode` `Successful` and `Progress` `100` but with no `Details` member, so the recorded entry and the served entry do not compare equal; the served entry instead carries only AutoScalingGroupState/ARN and the emulator's own Cause prose.
Why: a client reconciling an activity against the subnet/AZ an instance landed in — the common way to attribute a scale event to a zone — gets nothing to parse, and per-AZ activity accounting silently reports zero.
Fix: the DescribeScalingActivities projection / the shared activity-record writer in the autoscaling service crate's state: populate `Details` when the activity is recorded, for launch and terminate alike.
Traces: autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted#task#12.

Related but NOT covering these: open issue 20261111T102954Z-autoscaling-scaling-activities-do-not-record-real-launch.md concerns failed launches being recorded Successful; neither it nor 20261115T223640Z-autoscaling-auto-scaling-standby-suspend-process-and-group-lifecycle.md carries the replay keys above, so these two causes are net-new.

## Evidence
Cause 1 — fixture autoscaling-asgdx-traj-standby-park-and-return, step task#13, ExitStandby, status 200 vs 200:
  $.ExitStandbyResponse.ExitStandbyResult.Activities.member.StatusCode: expected 'PreInService', got 'InProgress'
  $.ExitStandbyResponse.ExitStandbyResult.Activities.member.Progress: expected '30', got '50'
  $.ExitStandbyResponse.ExitStandbyResult.Activities.member.Details: expected the placement JSON document (keys 'Availability Zone ID', 'Subnet ID', 'Availability Zone'), got None (member absent)
  $.ExitStandbyResponse.ExitStandbyResult.Activities.member.Cause: expected 'At <ts> instance <instance-id> was moved out of standby in response to a user request, increasing the capacity from <old> to <new>.', got 'a user request explicitly moved an instance of the group out of Standby: changing the desired capacity from 0 to 1.'

Cause 2 — fixture autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted, step task#12, DescribeScalingActivities, status 200 vs 200:
  $.DescribeScalingActivitiesResponse.DescribeScalingActivitiesResult.Activities.member[0] and member[1] do not match: the recorded 'Terminating EC2 instance: <id>' and 'Launching a new EC2 instance: <id>' entries each carry Details = the placement JSON document; the served entries carry no Details member.

Smithy citations: Activity.StatusCode -> ScalingActivityStatusCode enum values [ ... PreInService, InProgress, ... Successful, Failed, Cancelled ... ]; Activity.Progress -> integer; Activity.Details -> XmlString; Activity.Cause -> XmlStringMaxLen1023 (required).

## Steps to Reproduce
1. Replay fixture autoscaling-asgdx-traj-standby-park-and-return and inspect step task#13 (ExitStandby): compare Activities.member StatusCode / Progress / Details / Cause against the recording.
2. Replay fixture autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted and inspect step task#12 (DescribeScalingActivities): check that each activity member carries a Details placement document.
3. Both steps are part of the simparity-wire-aws-v1 corpus; replay_wire_corpus reports them as mismatch clusters on autoscaling.
