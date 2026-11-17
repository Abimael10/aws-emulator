Issue-Source: trace-sweep-divergence
Issue-Trace: iot-managed-integrations-notification-configuration-error-paths
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-managed-integrations-notification-configuration-error-paths
Issue-Shape-Id: com.amazonaws.iotmanagedintegrations#IotManagedIntegrations
Issue-Key: iot-managed-integrations|DeleteNotificationConfiguration|2xx->4xx|$
Issue-Key: iot-managed-integrations|DeleteNotificationConfiguration|2xx->4xx|status
Issue-Commit: 589d3a541192eb0665b868107715ac8cb1979bc9

# DeleteNotificationConfiguration refuses an unconfigured EventType with 404 instead of AWS's idempotent 200 empty-body success

> _Note: this issue was identified against the code base at commit `589d3a541192eb0665b868107715ac8cb1979bc9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteNotificationConfiguration

## Description
This bundle has 1 cause; it must be fixed before the issue is closed.

Cause 1 of 1: DELETE /notification-configurations/{EventType} for an event type the region holds no notification configuration under is answered as a modelled refusal (404 ResourceNotFoundException), where AWS answers it as a successful no-op (200, empty body).

AWS: Smithy (services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/smithy-model/com-amazonaws-iotmanagedintegrations-iotmanagedintegrations.json), smithy_query(query="operation", operation="DeleteNotificationConfiguration"): http {method: DELETE, uri: /notification-configurations/{EventType}, code: 200}, output_shape Unit (an empty body), idempotent: true; the single input member EventType is an @required @httpLabel of the EventType enum. ResourceNotFoundException (httpError 404) IS declared on the operation, so the model alone does not settle which path an unconfigured event type takes; for that, recorded behavior only: the corpus records this DELETE for an event type that no CreateNotificationConfiguration had configured in that region (a valid EventType enum value) and AWS answers HTTP 200 with an EMPTY body and no error payload at all. That is the @idempotent delete contract — the post-condition 'no notification configuration is held for this event type' is already satisfied, so the call succeeds, and the declared 404 is reserved for other conditions. Because the recorded response is a success there is NO error code and NO error message owed on this path: the requirement is the status (200) and the empty body, not any sentence.

Emulator: HTTP 404 with the JSON error body {"__type": "ResourceNotFoundException", "Message": null} — i.e. error code ResourceNotFoundException and a bare exception with no message member at all. ('ResourceNotFoundException' is fixed text for that error class; the body carries no variable part because the handler emits the BARE shape via crate::state::bare_resource_not_found().) Raising the error class on an already-absent event type is the defect; the bare shape is this surface's correct rendering for a genuine not-found and should stay as-is for reads and updates.

Why: a client relying on AWS's behavior treats notification-configuration deletion as idempotent — teardown and cleanup code, a retried delete whose first attempt already landed, and reconcilers that delete unconditionally all expect 200 with no error. Against the emulator each of those raises a ResourceNotFoundException from the SDK, so retries of an interrupted delete fail where on AWS they succeed, and teardown paths abort part-way, leaving the remaining cleanup unrun. It also makes the emulator's own @idempotent contract unobservable: the second of two identical deletes behaves differently from the first.

Fix: DeleteNotificationConfiguration's handler — services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/src/ops/delete_notification_configuration.rs, the `if removed.is_none()` branch (lines 208-220) that returns DeleteNotificationConfigurationErrorModeled::ResourceNotFoundException(crate::state::bare_resource_not_found()). The already-absent case belongs on the success path (return DeleteNotificationConfigurationOutput: status 200, empty body); the module header's reasoning (lines 7-9) that an unconfigured event type is 'the modelled ResourceNotFoundException' must be corrected to match the recording. Scope this to the notification-configuration delete only — sibling deletes have their own recordings and must keep whatever those record. crate::state::bare_resource_not_found stays: it is still what a genuine Get/Update 404 on this surface renders.

Related but NOT the same filing: open issue 20261109T005742Z-iot-managed-integrations-deletecloudconnector-refuses-an-absent-identifier-with-404-instead.md records the identical fault class for DeleteCloudConnector (a different operation, resource store and fixture step), and 20261029T224743Z-iot-managed-integrations-deleteotataskconfiguration-404-reuses-the-get-not-found-message.md covers DeleteOtaTaskConfiguration's 404 wording. Neither mentions DeleteNotificationConfiguration or this step, and the replay reports this step as owned by no issue, so closing either would leave it diverging.

Traces: iot-managed-integrations-notification-configuration-error-paths#task#3 (the representative and only member of this cluster, affected_count 1).

## Evidence
Replay of corpus simparity-wire-aws-v1 (trace_set_id simparity-wire-aws-v1), fixture iot-managed-integrations-notification-configuration-error-paths, step task#3, operation DeleteNotificationConfiguration, service iot-managed-integrations, verdict mismatch, affected_count 1, stub_path empty, owned_by empty.

Expected status 200, actual status 404.
Detail, verbatim from the replay:
  status: expected 200, got 404
  $: value_mismatch (expected {}, got {'__type': 'ResourceNotFoundException', 'Message': None})

Expected body: {} — the modelled Unit output, no members.
Actual body: a ResourceNotFoundException payload. '__type' value 'ResourceNotFoundException' is FIXED text for that error class; there is no variable part in it at all (Message is absent/null because the handler emits the bare shape). On AWS this path emits none of it: no error code and no message, only 200 with an empty body.

Model grounding: smithy_query(query="operation", operation="DeleteNotificationConfiguration") on services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/smithy-model/com-amazonaws-iotmanagedintegrations-iotmanagedintegrations.json reports http {method: DELETE, uri: /notification-configurations/{EventType}, code: 200}, output_shape Unit, idempotent: true, and lists ResourceNotFoundException (httpError 404) among the declared errors. Operation documentation: "Deletes a notification configuration."

Code path: src/ops/delete_notification_configuration.rs:203-205 removes crate::state::RegionalName::new(&region, &input.event_type) from store.notification_configurations; lines 208-220 convert a None removal into the 404 via crate::state::bare_resource_not_found(). Lines 7-9 of the module header state the (recording-contradicted) assumption that this path is the modelled ResourceNotFoundException.

## Steps to Reproduce
1. Boot the emulator from this checkout and point an iot-managed-integrations client at its data listener, signing for the fixture's region (eu-west-1).
2. Replay iot-managed-integrations-notification-configuration-error-paths#task#3: send DELETE /notification-configurations/{EventType} with a valid EventType enum value (e.g. one of ACCOUNT_ASSOCIATION | DEVICE_EVENT | DEVICE_OTA ...) for which no CreateNotificationConfiguration was ever made in that region.
3. Observe HTTP 404 with x-amzn-errortype/__type ResourceNotFoundException and a body carrying only a null/absent Message.
4. Compare with the recorded AWS response for that step: HTTP 200 with an empty body and no error payload.
5. Equivalent second-delete check of the same contract: create a notification configuration for an event type, delete it (200), then delete the same EventType again — AWS answers 200 with an empty body both times (@idempotent); the emulator answers 200 then 404.
