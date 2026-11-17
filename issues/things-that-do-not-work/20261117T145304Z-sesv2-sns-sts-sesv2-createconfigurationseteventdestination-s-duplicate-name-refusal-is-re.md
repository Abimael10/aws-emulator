Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sesv2__sns__sts.event-destination-duplicate-create-and-unknown-update-rejected
Issue-Key: sesv2__sns__sts.event-destination-duplicate-create-and-unknown-update-rejected|expect_error#22|AssertionError|assert (got_code, got_message) == (code, message), (
Issue-Commit: 6df92bd208b59385c9370bd54f69012d5d2b8998

# sesv2 CreateConfigurationSetEventDestination's duplicate-name refusal is re-worded: "Event destination X already exists in configuration set Y." instead of AWS's "An event destination with name <X> already exists for configuration set <Y>."

> _Note: this issue was identified against the code base at commit `6df92bd208b59385c9370bd54f69012d5d2b8998`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateConfigurationSetEventDestination

## Description
ONE net-new root cause, grounded in the single failing check of the suite's `sesv2__sns__sts` heading; the replay report marks it neither ALREADY FILED nor OWNED.

Cause 1 of 1: the AlreadyExistsException message for a duplicate event-destination name.
Failing check id: sesv2__sns__sts.event-destination-duplicate-create-and-unknown-update-rejected

AWS: Smithy (services/service-com-amazonaws-sesv2-simpleemailservice-v2/smithy-model/com-amazonaws-sesv2-simpleemailservice-v2.json) models CreateConfigurationSetEventDestination (POST /v2/email/configuration-sets/{ConfigurationSetName}/event-destinations) with errors AlreadyExistsException (client, HTTP 400), BadRequestException, LimitExceededException, NotFoundException (404) and TooManyRequestsException. Creating a second event destination with a name that configuration set already uses is refused with AlreadyExistsException and the message `An event destination with name <<EventDestinationName>> already exists for configuration set <<ConfigurationSetName>>.` — the FIXED text is `An event destination with name <`, `> already exists for configuration set <`, `>.` (AWS wraps each name in literal angle brackets and ends with a period); the VARIABLE parts are the event-destination name from the request and the configuration-set name from the request path. Recorded behavior for the exact sentence; the code and status are the modelled ones.

Emulator: the error code AlreadyExistsException is right, but the message is an emulator-authored sentence with different word order, no angle brackets and "in" instead of "for": `Event destination <name> already exists in configuration set <set>.`

Why: the message is part of the refusal contract — a client that distinguishes "name taken" from other AlreadyExists cases by text, a CDK/Terraform-style provider that parses the sentence, or a golden-output test written against AWS all mismatch, even though the status and code are correct.

Fix: the CreateConfigurationSetEventDestination handler's duplicate-name check in the sesv2 service crate — emit AWS's fixed sentence with the request's event-destination and configuration-set names substituted into the angle-bracketed slots.

Traces: sesv2__sns__sts.event-destination-duplicate-create-and-unknown-update-rejected (the duplicate-create leg; the check's later unknown-destination-update leg was never reached because the assertion fired first, so a fixer should re-run the whole check).

## Evidence
Failing check (suite service heading `### sesv2__sns__sts`): sesv2__sns__sts.event-destination-duplicate-create-and-unknown-update-rejected

AssertionError: expected ('AlreadyExistsException', 'An event destination with name <<dest-name>> already exists for configuration set <<cs-name>>.'), got ('AlreadyExistsException', 'Event destination <dest-name> already exists in configuration set <cs-name>.')
(the names in the recorded run were per-run unique values, e.g. sim2real-behav-sesv2-dest-… / sim2real-behav-sesv2-cs-…; only their positions in the template matter)

Expected (AWS): Code AlreadyExistsException (matches) and Message "An event destination with name <" + <EventDestinationName> + "> already exists for configuration set <" + <ConfigurationSetName> + ">."
Actual (emulator): "Event destination " + <name> + " already exists in configuration set " + <set> + "." — re-ordered wording, no angle brackets, "in" for "for".
Model evidence: CreateConfigurationSetEventDestination errors include AlreadyExistsException (errorKind client, httpError 400).

## Steps to Reproduce
1. sns:CreateTopic -> <topic arn>. sesv2:CreateConfigurationSet(ConfigurationSetName=<cs>).
2. sesv2:CreateConfigurationSetEventDestination(ConfigurationSetName=<cs>, EventDestinationName=<dest>, EventDestination={MatchingEventTypes:[...], SnsDestination:{TopicArn:<topic arn>}, Enabled:true}) — succeeds.
3. Repeat the identical call. EXPECT HTTP 400 AlreadyExistsException with Message "An event destination with name <<dest>> already exists for configuration set <<cs>>."; OBSERVE "Event destination <dest> already exists in configuration set <cs>."
Replay: sesv2__sns__sts.event-destination-duplicate-create-and-unknown-update-rejected
