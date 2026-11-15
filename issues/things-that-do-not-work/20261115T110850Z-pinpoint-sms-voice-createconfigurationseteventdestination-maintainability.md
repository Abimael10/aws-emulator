Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 2fdb0bca35dbf2816333afc14f0d0074e1e4571d

# The EventDestinationDefinition -> stored-record conversion of the three nested destinations is written out twice (create and update)

_Filed: 2026-11-15T11:08:50Z_
_Operation: CreateConfigurationSetEventDestination | Class: maintainability_

> _Note: this issue was identified against the code base at commit `2fdb0bca35dbf2816333afc14f0d0074e1e4571d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Converting the wire `CloudWatchLogsDestination` / `KinesisFirehoseDestination` / `SnsDestination` into their `crate::state` records is implemented once in the create handler and again, member for member, in the update handler. The two copies must agree for a destination written by `Create` and one rewritten by `Update` to read back the same way through `GetConfigurationSetEventDestinations`, and nothing ties them together: a member added to one of those three shapes, or a renamed state field, can be carried by one copy and dropped by the other. A single conversion (e.g. one `From` impl per destination shape, or one helper in `state.rs`) used by both handlers leaves every response and every existing test unchanged — the merge branching in `Update` and the unconditional build in `Create` stay where they are, only the per-member mapping is shared.

## Evidence

Copy 1 — services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/src/ops/create_configuration_set_event_destination.rs:281-295:
```
cloud_watch_logs: definition.cloud_watch_logs_destination.map(|dest| {
    crate::state::CloudWatchLogsDest {
        iam_role_arn: dest.iam_role_arn,
        log_group_arn: dest.log_group_arn,
    }
}),
kinesis_firehose: definition.kinesis_firehose_destination.map(|dest| {
    crate::state::KinesisFirehoseDest {
        delivery_stream_arn: dest.delivery_stream_arn,
        iam_role_arn: dest.iam_role_arn,
    }
}),
sns: definition.sns_destination.map(|dest| crate::state::SnsDest {
    topic_arn: dest.topic_arn,
}),
```
Copy 2 — src/ops/update_configuration_set_event_destination.rs:236-252:
```
if let Some(cloud_watch_logs) = definition.cloud_watch_logs_destination {
    destination.cloud_watch_logs = Some(crate::state::CloudWatchLogsDest {
        iam_role_arn: cloud_watch_logs.iam_role_arn,
        log_group_arn: cloud_watch_logs.log_group_arn,
    });
}
if let Some(kinesis_firehose) = definition.kinesis_firehose_destination {
    destination.kinesis_firehose = Some(crate::state::KinesisFirehoseDest {
        delivery_stream_arn: kinesis_firehose.delivery_stream_arn,
        iam_role_arn: kinesis_firehose.iam_role_arn,
    });
}
if let Some(sns) = definition.sns_destination {
    destination.sns = Some(crate::state::SnsDest {
        topic_arn: sns.topic_arn,
    });
}
```
The state records both copies build are declared once in src/state.rs:107-125 (`CloudWatchLogsDest`, `KinesisFirehoseDest`, `SnsDest`), and the reverse conversion lives once in src/ops/get_configuration_set_event_destinations.rs:203-217 — so only the inbound direction is duplicated.

Tests that pin the behaviour and must stay green unchanged: tests/all/event_destination_updates.rs:9-12 ("an update changes exactly the members of `EventDestinationDefinition` the request carries and leaves every other member of the stored destination ... as it was"), tests/all/event_destination_deletion.rs, and tests/all/configuration_set_lifecycle.rs.

## Steps to Reproduce

1. Open src/ops/create_configuration_set_event_destination.rs:281-295 and src/ops/update_configuration_set_event_destination.rs:236-252 side by side: the same five-member mapping of the three nested destination shapes onto the three `crate::state` records appears in both.
2. Add a member to `CloudWatchLogsDestination` in src/ops/types/shared_types.rs:54-59 and to `crate::state::CloudWatchLogsDest` in src/state.rs:108-112: the compiler forces both call sites to be edited independently, and populating only the create copy leaves `Update` silently dropping it, which no current test would catch.
3. Confirm the fix is response-preserving by re-running tests/all/event_destination_updates.rs and tests/all/configuration_set_lifecycle.rs after factoring the mapping into one place.
