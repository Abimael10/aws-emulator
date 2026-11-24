Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs.deep-logs-2-logcmk-malformed-entity-rejected-events-kept logs.deep-logs-3-logacct-transformer-policy-applies-to-new-group-and-group-transformer-overrides logs.deep-logs-3-logacct-transformer-policy-overlapping-prefix-rejected
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs.deep-logs-2-logcmk-malformed-entity-rejected-events-kept|run#18|AssertionError|assert got == want, (
Issue-Key: logs.deep-logs-3-logacct-transformer-policy-applies-to-new-group-and-group-transformer-overrides|run#26|Timeout|time.sleep(5.0)
Issue-Key: logs.deep-logs-3-logacct-transformer-policy-overlapping-prefix-rejected|run#22|Timeout|time.sleep(5.0)
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# logs: 2 net-new causes — PutLogEvents never reports an InvalidAttributes entity rejection, and the account TRANSFORMER_POLICY scenarios never settle

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutLogEvents

## Description
Cause 1 of 2: PutLogEvents accepts an entity whose `attributes` carry an unsupported value and reports no rejection at all.

AWS: Smithy com.amazonaws.cloudwatchlogs PutLogEvents takes `entity` (shape Entity) with `keyAttributes` (EntityKeyAttributes, @length min 2 max 4; allowed key names Type, ResourceType, Identifier, Name, Environment) and `attributes` (EntityAttributes, @length min 0 max 10) — "Additional attributes of the entity that are not used to specify the identity of the entity". When the entity is rejected the 200 response carries `rejectedEntityInfo`, whose member `errorType` is required and is EntityRejectionErrorType with values InvalidEntity, InvalidTypeValue, InvalidKeyAttributes, InvalidAttributes, EntitySizeTooLarge, UnsupportedLogGroupType, MissingRequiredFields (AWS puts the PascalCase value on the wire). An entity with valid keyAttributes but an `attributes` entry whose value is outside the set the service accepts (e.g. attributes.PlatformType set to a value that is not a supported platform) is answered HTTP 200 with rejectedEntityInfo.errorType = "InvalidAttributes" and NO rejectedLogEventsInfo: an entity rejection never rejects the batch, and every event of that batch is readable afterwards, in order.

Emulator: the same request is answered 200 with no rejectedEntityInfo member at all (`rejectedEntityInfo=None`), so errorType is absent instead of "InvalidAttributes". The other legs of the same check already behave: a bad keyAttributes.Type value is reported InvalidTypeValue and keyAttributes missing the required keys is reported MissingRequiredFields — only the `attributes` leg of entity validation is missing.

Why: a client that reads rejectedEntityInfo to detect malformed telemetry entities is told its entity was accepted. It keeps shipping entities AWS discards, and the entity association is silently lost on every event it ingests — a wrong answer with no signal, as opposed to AWS's explicit per-call rejection.

Fix: the entity validation in the logs crate's PutLogEvents handler — validate the `attributes` map's values (not only keyAttributes) and populate rejectedEntityInfo.errorType = InvalidAttributes, while still ingesting the batch and emitting no rejectedLogEventsInfo.

Traces: logs.deep-logs-2-logcmk-malformed-entity-rejected-events-kept.

Cause 2 of 2: the account-level TRANSFORMER_POLICY surface never reaches a verdict — both scenarios exhaust their budget.

AWS: Smithy cloudwatchlogs PutAccountPolicy with policyType TRANSFORMER_POLICY and selectionCriteria `LogGroupNamePrefix = "<prefix>"` echoes the criteria back in accountPolicy.selectionCriteria, and (per the CloudWatch Logs transformation documentation, which is where the rule is stated — the model does not state it) a prefix-scoped transformer policy transforms every in-prefix log group that has no transformer of its own; an in-prefix group's own PutTransformer chain REPLACES, not merges with, the account chain; an out-of-prefix group is untouched; and a second TRANSFORMER_POLICY whose prefix overlaps an existing one is refused 400 InvalidParameterException with the message "SelectionCriteria prefix is already supported by an existing policy" — that sentence is fixed text with no request-derived parts — and is not persisted, so the rejected policy name does not appear in DescribeAccountPolicies and a group in the rejected scope is transformed by the first policy alone.

Emulator: both checks end `Error: Check timed out after 60.0s`. Neither the transformed-record read (through the Kinesis sink the policy delivers to) nor the overlapping-prefix refusal is ever reached inside the budget, so the whole account transformer-policy behaviour — application on ingest, group override, out-of-prefix isolation, and the overlap guard — is unobservable.

Why: a client cannot distinguish an account-scoped transformer that is never applied from one whose delivery is merely late, and cannot rely on the overlap guard at all: a second overlapping policy may be accepted and silently double-transform, or may be refused with different wording, with no way to tell.

Fix: the account-policy (TRANSFORMER_POLICY) path in the logs crate — apply the prefix-scoped processor chain on ingest for in-prefix groups, let a group's own transformer override it, leave out-of-prefix groups untouched, enforce the overlapping-prefix InvalidParameterException with AWS's sentence, and make the ingest/delivery leg settle rather than hang.

Note: open issues 20261029T182816Z-logs-transformers-are-not-applied-on-ingest-data.md and 20261111T030649Z-logs-5-net-new-causes-ingest-transformers-are.md cover group-level ingest transformers; neither carries these two account-policy checks' replay keys.

Traces: logs.deep-logs-3-logacct-transformer-policy-applies-to-new-group-and-group-transformer-overrides, logs.deep-logs-3-logacct-transformer-policy-overlapping-prefix-rejected.

## Evidence
Cause 1 — logs.deep-logs-2-logcmk-malformed-entity-rejected-events-kept, check line 66:
`AssertionError: bad-attribute: rejectedEntityInfo.errorType=None, expected 'InvalidAttributes' (full rejectedEntityInfo=None)`
The request was PutLogEvents on a CMK-encrypted group with entity {keyAttributes: {Type: "Service", Name: <service>, Environment: <env>}, attributes: {PlatformType: <unsupported value>}}. Expected 200 + rejectedEntityInfo {errorType: "InvalidAttributes"} and no rejectedLogEventsInfo; actual 200 with rejectedEntityInfo absent. The preceding legs of the same loop passed: entity Type "Bogus" -> InvalidTypeValue, keyAttributes {Foo, Bar} -> MissingRequiredFields.

Cause 2 — logs.deep-logs-3-logacct-transformer-policy-applies-to-new-group-and-group-transformer-overrides: `Error: Check timed out after 60.0s`; logs.deep-logs-3-logacct-transformer-policy-overlapping-prefix-rejected: `Error: Check timed out after 60.0s`. Neither produced an assertion, so no step of the account TRANSFORMER_POLICY flow was graded: the expected refusal for the overlapping prefix is 400 InvalidParameterException / "SelectionCriteria prefix is already supported by an existing policy".

## Steps to Reproduce
Cause 1: CreateLogGroup + CreateLogStream, AssociateKmsKey with a CMK whose policy grants logs.<region>.amazonaws.com, then PutLogEvents with entity {keyAttributes: {Type: "Service", Name: <n>, Environment: <e>}, attributes: {PlatformType: <value outside the supported set>}} and a two-event batch. The emulator returns no rejectedEntityInfo; AWS returns rejectedEntityInfo.errorType "InvalidAttributes" with the events still ingested and readable in order.

Cause 2a: PutAccountPolicy policyType=TRANSFORMER_POLICY, selectionCriteria 'LogGroupNamePrefix = "<prefix>"' with a [parseJSON, addKeys] chain; create an in-prefix group with no transformer, an in-prefix group carrying its own PutTransformer chain, and an out-of-prefix group; ingest JSON events and read the delivered records through the subscription's Kinesis sink. The check never settles within 60 s.

Cause 2b: PutAccountPolicy TRANSFORMER_POLICY on prefix <base>, then PutAccountPolicy a second TRANSFORMER_POLICY on <base>-sub: AWS refuses 400 InvalidParameterException "SelectionCriteria prefix is already supported by an existing policy" and does not persist it; the check never settles within 60 s.
