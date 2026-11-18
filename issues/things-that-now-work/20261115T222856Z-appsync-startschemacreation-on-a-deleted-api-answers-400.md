Issue-Source: trace-sweep-divergence
Issue-Trace: appsync-error-delete-and-operate-on-deleted-api
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appsync-error-delete-and-operate-on-deleted-api
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync|StartSchemaCreation|4xx->4xx|$.message
Issue-Key: appsync|StartSchemaCreation|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: appsync|StartSchemaCreation|4xx->4xx|status
Issue-Commit: c2a51bbc379e00e6c722f42efee8d77cde344066

# AppSync StartSchemaCreation on a deleted API answers 400 BadRequestException instead of 404 NotFoundException "API not found."

> _Note: this issue was identified against the code base at commit `c2a51bbc379e00e6c722f42efee8d77cde344066`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartSchemaCreation

## Description
1 net-new cause. StartSchemaCreation collapses every ground an `apiId` can fail on into one refusal class, so an apiId that WAS a real API of this account and has since been deleted is refused as a malformed request instead of a missing resource.

Cause 1 of 1: StartSchemaCreation against a well-formed AppSync apiId whose API has been deleted is refused 400 BadRequestException "Invalid API ID <apiId>" instead of 404 NotFoundException "API not found." (fixture appsync-error-delete-and-operate-on-deleted-api, step task#4).

AWS: Smithy (services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice/smithy-model/com-amazonaws-appsync-awsdeepdishcontrolplaneservice.json): StartSchemaCreation = POST /v1/apis/{apiId}/schemacreation, success 200; required members apiId (String, @httpLabel, NO pattern constraint) and definition (Blob); modeled errors BadRequestException (client, httpError 400, members message/reason/detail), ConcurrentModificationException (client, 409), InternalFailureException (server, 500), NotFoundException (client, httpError 404, member message), UnauthorizedException (client, 401). The model therefore binds BOTH refusal classes to this operation and does not say which input picks which; recorded behavior only for the split. The recording shows AWS distinguishes the two grounds: for an apiId that is a well-formed AppSync api id (the 26-character lowercase alphanumeric token AWS itself issued) but no longer resolves because the API was deleted, AWS refuses with HTTP 404, x-amzn-errortype: NotFoundException and the message "API not found." — that sentence is entirely FIXED text, it interpolates nothing from the request or from state. (Hypothesis: the discriminator is the SHAPE of the token, not its existence — a value that no AppSync api id could be at all is the 400 BadRequestException "Invalid API ID <apiId>" case, whose fixed text is "Invalid API ID " and whose single variable part is the apiId copied verbatim from the URI label; that 400 case is recorded separately and is what open issues 20261109T022338Z-appsync-schema-creation-plane-diverges-success-detail-text.md (Cause 2) and 20261111T012528Z-appsync-2-net-new-causes-startschemacreation-refuses-an.md (Cause 1) already demand. Neither of those issues covers THIS cluster — the replay reports it with an empty owned_by — and the fix must satisfy both recordings at once rather than inverting the mapping again.)

Emulator: answers HTTP 400 with x-amzn-errortype: BadRequestException and the message "Invalid API ID <apiId>" (the rejected id spliced into emulator-authored prose). This is deliberate in the handler, which treats the apiId as a value to VALIDATE rather than a resource to RESOLVE and comments that "Every ground an `apiId` fails on is that one refusal: a value no API id can be at all and a well-formed id this account does not hold both name no API to add a schema to." So after a successful DeleteGraphqlApi, the follow-up StartSchemaCreation diverges in all three requirements: status 404 -> 400, error code NotFoundException -> BadRequestException, message "API not found." -> "Invalid API ID <apiId>".

Why: a client relying on AWS's behavior branches on the status and x-amzn-errortype. Deletion flows and teardown/idempotency logic treat 404/NotFoundException as "the resource is already gone, carry on" and 400/BadRequestException as "my request is malformed, do not retry and do not proceed" — so a delete-then-verify or re-create-after-delete sequence that AWS lets complete aborts against the emulator, and typed-error dispatch that catches NotFoundException never fires. Assertions on the refusal sentence also fail, because the emulator leaks the requested apiId into a message AWS keeps constant for this class.

Fix: the apiId resolution step of the StartSchemaCreation handler must keep the two grounds distinct instead of folding them together — a well-formed AppSync apiId that resolves to no API (including one deleted earlier in the session) must raise the modeled NotFoundException (404) with the fixed message "API not found.", while a token that cannot be an api id at all keeps the modeled BadRequestException (400) with the "Invalid API ID <apiId>" template.

Traces: appsync-error-delete-and-operate-on-deleted-api#task#4. Handler: services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice/src/ops/start_schema_creation.rs lines 172-187 (the single BadRequestException branch taken when guard.graphql_apis.get(&input.api_id) misses, using crate::state::invalid_api_id_message).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false, /__admin/health answered 200; 733 steps, 634 passed, score 86.5%). One net-new divergence cluster for this cause — service=appsync, stub_path empty, owned_by empty:

appsync-error-delete-and-operate-on-deleted-api#task#4 — operation StartSchemaCreation, verdict mismatch, expected_status 404, actual_status 400. divergence_key: service=appsync|StartSchemaCreation|status=404->400|keys=$.message,$headers.x-amzn-errortype,status. affected_count 1. detail:
  status: expected 404, got 400
  $.message: value_mismatch (expected 'API not found.', got 'Invalid API ID i5pwecfc4mlt3etajsvjc7xwhu')
  $headers.x-amzn-errortype: value_mismatch (expected 'NotFoundException', got 'BadRequestException')

Fixed text vs values: the expected message 'API not found.' is wholly fixed text with no interpolated part. In the emulator's message, 'Invalid API ID ' is emulator-authored fixed text and 'i5pwecfc4mlt3etajsvjc7xwhu' is the apiId from the request URI label — a per-run generated id, filler for a template value and not itself a requirement.

Model evidence (smithy_query, appsync model): StartSchemaCreation POST /v1/apis/{apiId}/schemacreation, code 200; input apiId (String, httpLabel, required, no pattern) + definition (Blob, required); errors BadRequestException(client,400), ConcurrentModificationException(client,409), InternalFailureException(server,500), NotFoundException(client,404), UnauthorizedException(client,401).

Source evidence: services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice/src/ops/start_schema_creation.rs:179-187 returns BadRequestException { message: invalid_api_id_message(&input.api_id) } for every apiId miss.

## Steps to Reproduce
1. Boot the emulator built from this checkout and replay trace set simparity-wire-aws-v1.
2. Fixture appsync-error-delete-and-operate-on-deleted-api mismatches at step task#4.
3. By hand: CreateGraphqlApi(name=<unique>, authenticationType='API_KEY') and keep the returned apiId (a 26-character lowercase alphanumeric token AWS/the emulator issued). Then DeleteGraphqlApi(apiId=<that id>) and confirm it succeeds.
4. Call StartSchemaCreation(apiId=<that same, now-deleted id>, definition=b'type Query { ping: String }\nschema { query: Query }\n') — i.e. POST /v1/apis/<deleted-apiId>/schemacreation.
5. Expect HTTP 404 with header x-amzn-errortype: NotFoundException and body {"message": "API not found."}.
6. Observe HTTP 400 with x-amzn-errortype: BadRequestException and body {"message": "Invalid API ID <the deleted apiId>"}.
7. Regression guard for the already-filed sibling case: POST /v1/apis/nonexistent0000000000/schemacreation must still answer 400 BadRequestException 'Invalid API ID nonexistent0000000000'. Both this step and step 5 must hold before the issue is closed.
