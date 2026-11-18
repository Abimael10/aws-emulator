Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot-managed-integrations.event-log-configuration-lifecycle-and-duplicate-conflict
Issue-Shape-Id: com.amazonaws.iotmanagedintegrations#IotManagedIntegrations
Issue-Key: iot-managed-integrations.event-log-configuration-lifecycle-and-duplicate-conflict|run#23|AssertionError|assert dead_update_code_msg is None, dead_update_code_msg
Issue-Commit: 53152f24252eb4c6b050f7ba33da4a3e7b513fd2

# UpdateEventLogConfiguration's 404 body carries the read-path not-found sentence where AWS sends a ResourceNotFoundException with no Message at all

> _Note: this issue was identified against the code base at commit `53152f24252eb4c6b050f7ba33da4a3e7b513fd2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateEventLogConfiguration

## Description
Cause 1 of 1: UpdateEventLogConfiguration serves a Message on its ResourceNotFoundException, re-using the read path's fixed sentence, where AWS answers the mutation path with a ResourceNotFoundException that carries NO Message member. Failing check: iot-managed-integrations.event-log-configuration-lifecycle-and-duplicate-conflict.

AWS: Smithy (com.amazonaws.iotmanagedintegrations) models UpdateEventLogConfiguration as PATCH /event-log-configurations/{Id} -> Unit, with errors AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), ValidationException(400). ResourceNotFoundException's members Message, ResourceId and ResourceType are ALL optional (no @required), so the model permits a 404 with an empty body and does not require any message. Recorded behavior only for which paths populate Message: the recording shows the two paths wording the same condition differently on this resource. GetEventLogConfiguration on a deleted id answers 404 ResourceNotFoundException with Message exactly "Event log configuration doesn't exist" (fixed text in full; no variable part - the id is not interpolated), and the emulator already matches that leg, which passes in the same check. UpdateEventLogConfiguration on the SAME deleted id answers 404 ResourceNotFoundException with the Message member ABSENT, so botocore's e.response["Error"].get("Message") is None. The requirement is therefore: on the update path the error code is ResourceNotFoundException at HTTP 404 and the serialized body carries no Message (nor ResourceId/ResourceType).

Emulator: the update path emits the read path's sentence. The check deletes the configuration, confirms Get 404s with "Event log configuration doesn't exist" (passes), then calls UpdateEventLogConfiguration(Id=<deleted id>, EventLogLevel="INFO"). The emulator returns code ResourceNotFoundException (correct) but with Message "Event log configuration doesn't exist", so the assertion that the update's Message is absent fails with `AssertionError: Event log configuration doesn't exist`. The handler deliberately shares one constructor for both paths - services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/src/ops/update_event_log_configuration.rs lines 204-214 return `crate::state::event_log_configuration_not_found()` under the comment "The one fixed sentence this resource's 404s carry, read and mutation alike" - which is exactly the assumption that diverges: read and mutation do NOT carry the same body here.

Why: a client that distinguishes AWS's not-found paths by the presence or text of Message gets a message AWS never sends. Error-handling code that branches on the message (or that logs/compares the full error body, or asserts message absence to identify the mutation path) sees a different shape under the emulator than in production, and a recorded-vs-replayed body comparison of the update 404 diverges even though code and status agree. It also masks the real distinction between this service's read and mutation 404 bodies, so an emulator-passing test can fail against AWS.

Fix: the UpdateEventLogConfiguration handler's not-found arm (its error construction, not the shared state helper, which the Get path legitimately needs): construct ResourceNotFoundException with message None on the update path so the serializer emits a 404 body with no Message, leaving GetEventLogConfiguration's wording untouched. Note the related RESOLVED issue 20261029T171839Z-iot-managed-integrations-resourcenotfoundexception-bodies-carry-resourceid-resourcetype-and-message-that.md, which removed emulator-added members from other ResourceNotFoundException bodies; it does not cover this operation, whose update path still carries the read sentence. This is not a refusal regression: the call is correctly refused with 404 ResourceNotFoundException, only the body diverges.

Traces: iot-managed-integrations.event-log-configuration-lifecycle-and-duplicate-conflict (line 394 of check_iot_managed_integrations_event_log_configuration_lifecycle_and_duplicate_conflict.py: `assert dead_update_code_msg is None`). Handler citation: services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/src/ops/update_event_log_configuration.rs:204-214.

## Evidence
Check: iot-managed-integrations.event-log-configuration-lifecycle-and-duplicate-conflict [mismatch]

Sequence: CreateEventLogConfiguration(ResourceType="*", EventLogLevel="INFO") -> Get/Update/Get (all pass) -> duplicate create ConflictException "Log configuration already exists." (passes) -> DeleteEventLogConfiguration(Id) -> GetEventLogConfiguration(Id) -> 404 ResourceNotFoundException, Message "Event log configuration doesn't exist" (passes) -> UpdateEventLogConfiguration(Id=<deleted id>, EventLogLevel="INFO").

Expected (AWS): HTTP 404, Error.Code = "ResourceNotFoundException", Error.Message ABSENT (botocore: e.response["Error"].get("Message") is None); no ResourceId/ResourceType members.

Actual (emulator): HTTP 404, Error.Code = "ResourceNotFoundException" (correct), Error.Message = "Event log configuration doesn't exist" — the read path's fixed sentence, emitted on the mutation path too.

Failure output:
  File "check_iot_managed_integrations_event_log_configuration_lifecycle_and_duplicate_conflict.py", line 52, in run
    assert dead_update_code_msg is None, dead_update_code_msg
  AssertionError: Event log configuration doesn't exist

Model support: ResourceNotFoundException = {Message: ErrorMessage (optional), ResourceId (optional), ResourceType (optional)}, httpError 404 — nothing requires a message, and AWS sends none on this operation.

Message-text breakdown: "Event log configuration doesn't exist" is entirely fixed text with no request- or state-derived variable part; it belongs to GetEventLogConfiguration's 404 only. UpdateEventLogConfiguration's 404 has no message template at all.

## Steps to Reproduce
1. Point a botocore client at the emulator: client = session.client("iot-managed-integrations", region_name="eu-west-1", endpoint_url=<emulator>).
2. created = client.create_event_log_configuration(ResourceType="*", EventLogLevel="INFO"); cfg = created["Id"].
3. client.delete_event_log_configuration(Id=cfg).
4. Confirm the read leg: client.get_event_log_configuration(Id=cfg) raises ClientError with Code "ResourceNotFoundException" and Message "Event log configuration doesn't exist" (this is correct and must not change).
5. try: client.update_event_log_configuration(Id=cfg, EventLogLevel="INFO") / except ClientError as e: print(e.response["Error"]["Code"], repr(e.response["Error"].get("Message")), e.response["ResponseMetadata"]["HTTPStatusCode"]).
6. Observed: ResourceNotFoundException 404 with Message "Event log configuration doesn't exist". Expected against AWS: ResourceNotFoundException 404 with no Message member (get("Message") returns None).

Or replay the suite check directly: iot-managed-integrations.event-log-configuration-lifecycle-and-duplicate-conflict.
