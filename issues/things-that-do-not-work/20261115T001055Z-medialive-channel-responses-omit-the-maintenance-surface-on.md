Issue-Source: trace-sweep-divergence
Issue-Trace: medialive-cmc-traj-attached-input-conflicts-and-delete-order
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: medialive-cmc-traj-attached-input-conflicts-and-delete-order
Issue-Shape-Id: com.amazonaws.medialive#MediaLive
Issue-Key: medialive|DescribeChannel|2xx->2xx|$.maintenanceStatus
Issue-Key: medialive|DescribeChannel|2xx->2xx|$.maintenanceWindow
Issue-Key: medialive|DeleteChannel|2xx->2xx|$.maintenanceStatus
Issue-Key: medialive|DeleteChannel|2xx->2xx|$.maintenanceWindow
Issue-Commit: a49cdab94fc41177458b6e53730040285f53d8d6

# medialive channel responses omit the maintenance surface on DescribeChannel and DeleteChannel

> _Note: this issue was identified against the code base at commit `a49cdab94fc41177458b6e53730040285f53d8d6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeChannel

## Description
A MediaLive channel in the emulator carries no maintenance settings at all, so every operation that returns a channel body serves that part of the shape as absent. Two causes, one per channel-returning operation exercised by the corpus; both are the same missing projection of channel state.

Cause 1 of 2 - DescribeChannel omits the channel's maintenance settings (fixture medialive-cmc-traj-attached-input-conflicts-and-delete-order, step task#7; same cluster also at task#12 and task#14).
AWS: Smithy: DescribeChannel (GET /prod/channels/{ChannelId}, 200) output member Maintenance, jsonName "maintenance", target MaintenanceStatus; MaintenanceStatus members MaintenanceDay (jsonName maintenanceDay, enum, "The currently selected maintenance day"), MaintenanceStartTime (jsonName maintenanceStartTime, "The currently selected maintenance start time. Time is in UTC"), MaintenanceDeadline (jsonName maintenanceDeadline, "Maintenance is required by the displayed date and time") and MaintenanceScheduledDate (jsonName maintenanceScheduledDate). CreateChannel accepts the companion MaintenanceCreateSettings (maintenanceDay + maintenanceStartTime), so a channel always has a maintenance day and start time, whether the client chose them or the service assigned them. The recorded response for this step carries the channel's maintenance window and its maintenance scheduling state as present values (the window is asserted as shaped-but-not-value-compared, the scheduling state as the empty string, i.e. present and not yet scheduled); recorded behavior only for the exact top-level projection names the fixture asserts ($.maintenanceWindow, $.maintenanceStatus), which are the trajectory's normalized view of that modeled maintenance sub-structure.
Emulator: the step answers 200, matching the recorded status, but both maintenance keys are absent from the body - the replay reports "$.maintenanceWindow: missing (expected '<shaped-not-asserted>', got None)" and "$.maintenanceStatus: missing (expected '', got None)". Nothing about the channel's maintenance day, start time, deadline or scheduled date is served, and the distinction between "no maintenance scheduled" (an empty scheduling state) and "this field does not exist" is lost.
Why: a client that reads a channel to show or reconcile its maintenance window gets nothing to read. An SDK deserializes Maintenance as absent, so code that renders the maintenance day/start time renders nothing, and code that round-trips a described channel back into UpdateChannel drops the maintenance settings the channel actually has. Infrastructure tools that diff desired against actual state see the maintenance block as permanently unset and will either report perpetual drift or keep re-applying it.
Fix: the DescribeChannel handler's channel projection, and the channel record in state that it reads from - the maintenance settings must be stored on the channel when it is created (defaulted when the request omits them, as AWS assigns one) and then projected on every read, rather than left unpopulated.
Traces: medialive-cmc-traj-attached-input-conflicts-and-delete-order#task#7 (representative), plus task#12 and task#14 in the same cluster.

Cause 2 of 2 - DeleteChannel's response body omits the same maintenance settings (fixture medialive-cmc-traj-attached-input-conflicts-and-delete-order, step task#11).
AWS: Smithy: DeleteChannel (DELETE /prod/channels/{ChannelId}, 200) returns DeleteChannelResponse, which carries the SAME channel body as DescribeChannel including output member Maintenance, jsonName "maintenance", target MaintenanceStatus. AWS returns the full description of the channel being deleted, so the maintenance settings are present in the delete response exactly as they are in a describe. The recorded response for this step likewise carries both the maintenance window and the maintenance scheduling state as present values.
Emulator: the step answers 200, matching the recorded status, but the delete response body is missing both maintenance keys - "$.maintenanceWindow: missing (expected '<shaped-not-asserted>', got None)" and "$.maintenanceStatus: missing (expected '', got None)".
Why: DeleteChannel is the last chance a client has to capture the channel's state before it is gone. A client that archives or logs the deleted channel's configuration from the delete response - the normal pattern, since a follow-up DescribeChannel would 404 - loses the maintenance settings permanently and cannot recreate the channel with the window it had.
Fix: the DeleteChannel handler's response serializer, which must emit the same channel projection as DescribeChannel instead of a reduced one; it shares the state defect in Cause 1, so populating and projecting the channel's maintenance settings fixes both paths.
Traces: medialive-cmc-traj-attached-input-conflicts-and-delete-order#task#11.

Not covered by an existing issue: the open medialive issues 20261103T014723Z-medialive-channel-tombstones-security-group-tags-and-pull.md (channel tombstones and security-group tags) and 20261110T165405Z-medialive-reads-omit-committed-state-account-kmskeyid-and.md (account kmsKeyId and alarm template groups) concern different members and different operations, and neither carries the replay key for these two steps.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture medialive-cmc-traj-attached-input-conflicts-and-delete-order.

Cause 1 - DescribeChannel, step task#7 (cluster also covers task#12, task#14), verdict mismatch, expected_status 200 / actual_status 200:
  $.maintenanceWindow: missing (expected '<shaped-not-asserted>', got None)
  $.maintenanceStatus: missing (expected '', got None)
  divergence_key: service=medialive|DescribeChannel|status=200->200|keys=$.maintenanceStatus,$.maintenanceWindow
Expected: the channel body carries its maintenance settings - a maintenance window (the modeled maintenance.maintenanceDay + maintenance.maintenanceStartTime) present and shaped, and a maintenance scheduling state present (empty when no maintenance is scheduled).
Actual: both keys absent from the response body entirely (None), so no maintenance information is served.

Cause 2 - DeleteChannel, step task#11, verdict mismatch, expected_status 200 / actual_status 200:
  $.maintenanceWindow: missing (expected '<shaped-not-asserted>', got None)
  $.maintenanceStatus: missing (expected '', got None)
  divergence_key: service=medialive|DeleteChannel|status=200->200|keys=$.maintenanceStatus,$.maintenanceWindow
Expected: the delete response returns the full channel body, including the maintenance settings, as DeleteChannelResponse models them (member Maintenance / jsonName "maintenance" / target MaintenanceStatus).
Actual: both keys absent (None).

Model evidence (services/service-com-amazonaws-medialive-medialive/smithy-model/com-amazonaws-medialive-medialive.json): DescribeChannelResponse and DeleteChannelResponse both bind Maintenance -> MaintenanceStatus with jsonName "maintenance"; MaintenanceStatus = {maintenanceDay (enum), maintenanceStartTime, maintenanceDeadline, maintenanceScheduledDate}; CreateChannel takes MaintenanceCreateSettings = {maintenanceDay, maintenanceStartTime}. No status code or error code diverges in either cause - this is purely a missing output member on a 200 response. The concrete window value, scheduled date and deadline in the recording are per-run/per-account values and are not the requirement; the requirement is that the maintenance members are present and populated from the channel's own state.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay fixture medialive-cmc-traj-attached-input-conflicts-and-delete-order from trace set simparity-wire-aws-v1.
3. Cause 1: at step task#7, GET /prod/channels/{ChannelId} (DescribeChannel) for the channel the trajectory created. The emulator answers 200 but the body has no maintenance window and no maintenance scheduling state; the recorded AWS response has both. Steps task#12 and task#14 repeat the same read with the same result.
4. Cause 2: at step task#11, DELETE /prod/channels/{ChannelId} (DeleteChannel) for that channel. The emulator answers 200 but again omits the maintenance window and maintenance scheduling state from the returned channel body.
5. Compare each response body against the recorded one: in both cases the two maintenance keys are present in the recording and absent (None) from the emulator.
