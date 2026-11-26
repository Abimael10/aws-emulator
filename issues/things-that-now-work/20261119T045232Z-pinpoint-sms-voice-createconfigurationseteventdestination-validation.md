Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 687878a7b2387a0d7f4027758ceff70f56698de8

# The cross-account pass-role refusal is keyed to member NAMES, so an IAM role of another account sent in KinesisFirehoseDestination$DeliveryStreamArn is accepted and stored

_Filed: 2026-11-19T04:52:32Z_
_Operation: CreateConfigurationSetEventDestination | Class: validation_

> _Note: this issue was identified against the code base at commit `687878a7b2387a0d7f4027758ceff70f56698de8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The crate implements ONE rule for a role another account holds: a destination definition that hands this service such a role is refused with HTTP 403 `AccessDeniedException` / "Cross-account pass role is not allowed.", "whatever else is true of the request". The implementation (`types::authorization::pass_role_denial`, called by `CreateConfigurationSetEventDestination` and, through the same helper, by `UpdateConfigurationSetEventDestination`) does not look at what the values ARE — it looks at two member names only: `CloudWatchLogsDestination$IamRoleArn` and `KinesisFirehoseDestination$IamRoleArn`. The model, however, documents `KinesisFirehoseDestination$DeliveryStreamArn` as carrying "The Amazon Resource Name (ARN) of an IAM role that can write data to an Amazon Kinesis Data Firehose stream" — a member the helper never reads. So the identical ARN that is refused 403 in `IamRoleArn` is accepted with HTTP 200 and persisted in the configuration set when it arrives in `DeliveryStreamArn`, and `GetConfigurationSetEventDestinations` reports it back. The helper's own predicate already decides from the VALUE (`names_another_accounts_role` parses the ARN and requires vendor `iam`, resource type `role`, and a foreign non-empty account), so the member-name keying is the whole of the gap: a genuine Firehose stream ARN (`arn:aws:firehose:...`) cannot match that predicate, which is why the two destinations' other ARN-bearing members are not the issue — `SnsDestination$TopicArn` is explicitly excluded with a stated reason ("an SNS topic ... passes no role at all") and `CloudWatchLogsDestination$LogGroupArn` is documented as a log group, not a role. `DeliveryStreamArn` is the one member the model documents as a role ARN and the helper neither reads nor excuses.

## Evidence

The rule, stated in the crate's single authorization module — services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/src/ops/types/authorization.rs:70-80: "The refusal an action gets when the event destination it defines hands this service an IAM role ARN naming an account OTHER than the one this emulator serves, or `None` when it hands over no such role. ... a role of another account is one no principal of this account can hand to a service, whatever else is true of the request — which is why this is decided for every account, entitled or not"; and authorization.rs:26-31: "a request that hands this service a role of another account is told so ([`pass_role_denial`]), whatever the account's own entitlement."

The code, which reads exactly two members by name — authorization.rs:84-97:
```
    [
        definition
            .cloud_watch_logs_destination
            .as_ref()
            .and_then(|destination| destination.iam_role_arn.as_deref()),
        definition
            .kinesis_firehose_destination
            .as_ref()
            .and_then(|destination| destination.iam_role_arn.as_deref()),
    ]
    .into_iter()
    .flatten()
    .any(names_another_accounts_role)
    .then(cross_account_pass_role)
```
and the value-based predicate it already owns — authorization.rs:107-114: `service_common::arn::parse(role_arn).is_some_and(|arn| arn.vendor == IAM_VENDOR && arn.resource_type() == ROLE_RESOURCE_TYPE && !arn.account.is_empty() && arn.account != ::engine::emulator_account_id())`.

The model member the helper never reads — smithy_query(query="doc", shape="KinesisFirehoseDestination", member="DeliveryStreamArn"): "The Amazon Resource Name (ARN) of an IAM role that can write data to an Amazon Kinesis Data Firehose stream." (the sibling member is documented the other way round: smithy_query(query="doc", shape="KinesisFirehoseDestination", member="IamRoleArn") = "The Amazon Resource Name (ARN) of the Amazon Kinesis Data Firehose destination that you want to use in the event destination.").

The excluded members that ARE justified, so this one stands out — authorization.rs:80: "A destination that passes no role at all (an SNS topic) reaches the other decision."; smithy_query(query="shape", shape="CloudWatchLogsDestination") gives `LogGroupArn` the doc "The name of the Amazon CloudWatch Log Group that you want to record events in." No comment anywhere in the crate mentions `DeliveryStreamArn`.

The call sites — src/ops/create_configuration_set_event_destination.rs:200-202 (`if let Some(denied) = super::types::authorization::pass_role_denial(&definition) { return Err(denied); }`) and src/ops/update_configuration_set_event_destination.rs:176-178 (identical), after which create_configuration_set_event_destination.rs:287-292 stores the member verbatim: `kinesis_firehose: definition.kinesis_firehose_destination.map(|dest| crate::state::KinesisFirehoseDest { delivery_stream_arn: dest.delivery_stream_arn, iam_role_arn: dest.iam_role_arn })`.

Test coverage: tests/all/legacy_surface_authorization.rs:697-770 is the suite for this rule, and EVERY row puts the foreign role in an `IamRoleArn` — :716-723 "a Firehose role another account holds" sends `{"KinesisFirehoseDestination": {"DeliveryStreamArn": "arn:aws:firehose:us-east-1:123456789012:deliverystream/voice", "IamRoleArn": "arn:aws:iam::123456789012:role/voice-firehose"}}`, :731-737 sends the role in `KinesisFirehoseDestination$IamRoleArn` alone. No row puts an `arn:aws:iam::<other account>:role/...` value in `DeliveryStreamArn`, so nothing pins or justifies the current scan set.

## Steps to Reproduce

1. Boot the emulator and grant the account the entitlement the resource-scoped actions need (as tests/all/legacy_surface_authorization.rs:99-104 does: set `legacy_surface_entitled = true`).
2. POST /v1/sms-voice/configuration-sets with {"ConfigurationSetName": "firehose-set"} — HTTP 200.
3. Control: POST /v1/sms-voice/configuration-sets/firehose-set/event-destinations with {"EventDestinationName": "fh-role-member", "EventDestination": {"KinesisFirehoseDestination": {"IamRoleArn": "arn:aws:iam::123456789012:role/voice-firehose"}}} — observe HTTP 403, `AccessDeniedException`, message "Cross-account pass role is not allowed."
4. Now POST /v1/sms-voice/configuration-sets/firehose-set/event-destinations with {"EventDestinationName": "fh-stream-member", "EventDestination": {"KinesisFirehoseDestination": {"DeliveryStreamArn": "arn:aws:iam::123456789012:role/voice-firehose"}}} — the SAME foreign role ARN, in the member the model documents as "the ARN of an IAM role" — observe HTTP 200.
5. GET /v1/sms-voice/configuration-sets/firehose-set/event-destinations — the destination is reported back with `DeliveryStreamArn` set to the other account's role ARN, so the refusal the crate states for "whatever else is true of the request" was never applied.
6. Repeat step 4 as PUT /v1/sms-voice/configuration-sets/firehose-set/event-destinations/fh-role-member — the same acceptance, since UpdateConfigurationSetEventDestination calls the same helper (update_configuration_set_event_destination.rs:176-178).
