Issue-Source: trace-sweep-divergence
Issue-Trace: medialive-cmc-traj-attached-input-conflicts-and-delete-order
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: medialive-cmc-traj-attached-input-conflicts-and-delete-order medialive-cmc-traj-role-mistakes-and-idempotent-retry
Issue-Shape-Id: com.amazonaws.medialive#MediaLive
Issue-Key: medialive|DescribeChannel|2xx->2xx|$.maintenanceWindow
Issue-Key: medialive|DeleteChannel|2xx->2xx|$.maintenanceWindow
Issue-Key: medialive|CreateChannel|2xx->2xx|$.channel.maintenanceWindow
Issue-Commit: 75f577a9b4ae922afffed072560bc3cac4bdc4f1

# medialive channel responses carry a hardcoded maintenanceWindow instead of the channel's own maintenance settings

> _Note: this issue was identified against the code base at commit `75f577a9b4ae922afffed072560bc3cac4bdc4f1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeChannel

## Description
Three divergence clusters, one shared fault: every channel-shaped response the emulator serves carries the same literal maintenance window string, independent of the channel and of the maintenance settings the create request supplied. 3 causes must be closed.

Cause 1 of 3: DescribeChannel ($.maintenanceWindow)
AWS: Smithy: DescribeChannel (GET /prod/channels/{ChannelId}, 200) returns Maintenance -> MaintenanceStatus {maintenanceDay (MaintenanceDay enum), maintenanceStartTime, maintenanceDeadline, maintenanceScheduledDate}; com.amazonaws.medialive#Channel models the same member, and NO channel shape in the model carries a scalar 'maintenanceWindow' string. Recorded behavior only for the extra field: the recorded DescribeChannel response carries $.maintenanceWindow with a per-channel value the corpus masks as '<shaped-not-asserted>' (shape asserted, value not asserted), and the emulator's value fails that shape assertion.
Emulator: answers 200 with $.maintenanceWindow = 'TUESDAY 16:00' - a day name and an hour joined by a space - the identical literal in every affected step, in two unrelated fixtures, for channels created with different requests.
Why: a client reading a channel's maintenance window gets a constant that reflects neither what it configured on CreateChannel (MaintenanceCreateSettings.maintenanceDay / maintenanceStartTime) nor the recorded shape, so maintenance-window scheduling logic and any round-trip assertion on the value it just set are wrong.
Fix: the channel response projection shared by the channel read/write handlers in services/service-com-amazonaws-medialive-medialive/src/ops plus the channel record in src/state.rs - store the channel's maintenance settings at create time and derive this field from them, in the recorded shape, instead of emitting a fixed literal.
Traces: medialive-cmc-traj-attached-input-conflicts-and-delete-order#task#7 (cluster also covers #task#12, #task#14, medialive-cmc-traj-role-mistakes-and-idempotent-retry#task#10 and #teardown#14).

Cause 2 of 3: DeleteChannel ($.maintenanceWindow)
AWS: Smithy: DeleteChannel (DELETE /prod/channels/{ChannelId}, 200) returns the same channel projection, Maintenance -> MaintenanceStatus, with no scalar maintenanceWindow member. Recorded behavior only: the recorded delete response carries $.maintenanceWindow with a value masked as '<shaped-not-asserted>'.
Emulator: 200 with $.maintenanceWindow = 'TUESDAY 16:00', the same constant as on describe.
Why: the terminal state a client records for a deleted channel keeps the wrong maintenance window, so a client that archives the delete response stores a value that was never the channel's.
Fix: same channel response projection - the delete path must serialize the stored maintenance settings, not a constant.
Traces: medialive-cmc-traj-attached-input-conflicts-and-delete-order#task#11 (cluster also covers medialive-cmc-traj-role-mistakes-and-idempotent-retry#teardown#13).

Cause 3 of 3: CreateChannel ($.channel.maintenanceWindow)
AWS: Smithy: CreateChannel (POST /prod/channels, 201) takes Maintenance -> MaintenanceCreateSettings {maintenanceDay (MaintenanceDay enum), maintenanceStartTime (pattern __stringPattern010920300)} and returns Channel -> com.amazonaws.medialive#Channel, whose maintenance members echo what was requested (or the service-assigned window when none was requested). No scalar maintenanceWindow is modeled. Recorded behavior only: the recorded 201 body carries $.channel.maintenanceWindow masked as '<shaped-not-asserted>'.
Emulator: 201 with $.channel.maintenanceWindow = 'TUESDAY 16:00' on every create, so the value is not a function of the request.
Why: the create response is the first thing a client reads back; it cannot confirm the maintenance day/start time it asked for, and an idempotent re-create comparison sees a value it never sent.
Fix: CreateChannel handler in src/ops - persist the requested maintenance settings onto the channel record in src/state.rs and project this field from them.
Traces: medialive-cmc-traj-role-mistakes-and-idempotent-retry#task#9 (cluster also covers #task#11).

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout, 3 mismatch clusters, 9 steps:
- DescribeChannel, expected 200 / actual 200: '$.maintenanceWindow: value_mismatch (expected \'<shaped-not-asserted>\', got \'TUESDAY 16:00\')' - medialive-cmc-traj-attached-input-conflicts-and-delete-order#task#7, #task#12, #task#14; medialive-cmc-traj-role-mistakes-and-idempotent-retry#task#10, #teardown#14.
- DeleteChannel, expected 200 / actual 200: '$.maintenanceWindow: value_mismatch (expected \'<shaped-not-asserted>\', got \'TUESDAY 16:00\')' - medialive-cmc-traj-attached-input-conflicts-and-delete-order#task#11; medialive-cmc-traj-role-mistakes-and-idempotent-retry#teardown#13.
- CreateChannel, expected 201 / actual 201: '$.channel.maintenanceWindow: value_mismatch (expected \'<shaped-not-asserted>\', got \'TUESDAY 16:00\')' - medialive-cmc-traj-role-mistakes-and-idempotent-retry#task#9, #task#11.
The actual value is byte-identical in all nine steps and across two fixtures whose channels were created by different requests, which is what identifies it as a hardcoded constant rather than stored state. Smithy (services/service-com-amazonaws-medialive-medialive/smithy-model): Channel.Maintenance -> MaintenanceStatus{maintenanceDay, maintenanceStartTime, maintenanceDeadline, maintenanceScheduledDate}; CreateChannelRequest.Maintenance -> MaintenanceCreateSettings{maintenanceDay, maintenanceStartTime}; no 'maintenanceWindow' member exists on Channel, DescribeChannelResponse or DeleteChannelResponse.

## Steps to Reproduce
1. Replay fixture medialive-cmc-traj-role-mistakes-and-idempotent-retry: CreateChannel (step task#9) with maintenance settings, and inspect $.channel.maintenanceWindow in the 201 body.
2. Replay fixture medialive-cmc-traj-attached-input-conflicts-and-delete-order: GET the channel (step task#7) and inspect $.maintenanceWindow, then DELETE it (step task#11) and inspect $.maintenanceWindow in the 200 body.
3. Observe the same literal 'TUESDAY 16:00' in all three responses regardless of the create request, where the recordings carry a per-channel value of a different shape.
