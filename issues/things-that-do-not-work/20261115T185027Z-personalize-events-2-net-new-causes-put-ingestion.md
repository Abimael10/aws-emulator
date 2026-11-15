Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: personalize-events.put-events-minimal-fields-success personalize-events.put-events-reserved-property-keyword-rejected
Issue-Shape-Id: com.amazonaws.personalizeevents#AmazonPersonalizeEvents
Issue-Key: personalize-events.put-events-minimal-fields-success|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Key: personalize-events.put-events-reserved-property-keyword-rejected|run#7|AssertionError|assert err["Message"] == (
Issue-Commit: 302c678b7901f99ac739cc877697e5b939709ef8

# personalize-events: 2 net-new causes — PUT-ingestion-mode dataset export renames the real-time record's columns to the schema's declared field names, and PutEvents' reserved-properties-keyword refusal message is emulator-authored wording

> _Note: this issue was identified against the code base at commit `302c678b7901f99ac739cc877697e5b939709ef8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
Two net-new root causes behind the two failing personalize-events behavioral checks. Both are defects of the PutEvents ingestion round trip as a client observes it; neither is covered by an open issue (see the notes under each cause).

Cause 1 of 2: a PUT-ingestion-mode dataset export renames the columns of the real-time event records to the Interactions schema's declared field names, so a client reading the export back by the column names AWS writes for PUT data finds no rows at all. Failing check: personalize-events.put-events-minimal-fields-success.

AWS: a client makes a PutEvents event durable, then reads it back at the only terminal reader AWS offers for it — personalize.CreateDatasetExportJob with ingestionMode="PUT" (Smithy com.amazonaws.personalize, CreateDatasetExportJob$ingestionMode: "only PUT data that you imported incrementally (using the console, PutEvents, PutUsers and PutItems operations)"; "The default value is PUT"), waits for DescribeDatasetExportJob to report status ACTIVE ("CREATE PENDING > CREATE IN_PROGRESS > ACTIVE -or- CREATE FAILED"), and reads the CSV objects under the jobOutput.s3DataDestination.path folder. Recorded behavior only for the column spelling: AWS writes a PUT-mode export with the raw real-time event record's own lowercase snake_case columns (event_type, item_id, user_id, timestamp, session_id, ...), NOT the uppercase field names the Interactions schema declares (EVENT_TYPE, ITEM_ID, USER_ID, TIMESTAMP) — those govern a BULK-ingestion-mode export of rows a dataset import job read in. The recorded run of this check therefore selects the exported row by the header name "event_type". The event itself carries only eventType, sentAt and itemId (no userId), and AWS still exports it as exactly one row.

Emulator: PutEvents stores the event with snake_case columns, but the export renames every column to the schema's spelling before rendering, for every ingestion mode. The stored PUT row is built with the columns user_id / item_id / timestamp / event_type, and the export then maps each column through a case-insensitive lookup against the dataset schema's declared fields and emits the SCHEMA's spelling, so the written header row reads USER_ID,ITEM_ID,TIMESTAMP,EVENT_TYPE for a PUT-mode export. A client selecting rows by "event_type" (the name AWS writes) sees zero matching rows, re-runs the export on its retry cadence, and the check is killed by its budget with "Check timed out after 60.0s" instead of asserting on the single expected row. Secondary, contributing: the create-status clock holds every stage transient for 500ms (dataset group, dataset, event tracker, export job), so the first Describe* after each create always reports a transient status and the client pays one full poll interval per stage before it can even attempt the first export — roughly 35s of client sleep here, which leaves room for only one export attempt inside the budget. Open issue 20261110T163937Z-personalize-runtime-personalize-provisioning-never-reaches-an-active-campaign-inside describes that clock, but it is keyed to the two personalize-runtime checks and does not carry this check's replay key, and the column naming above is the reason this check cannot pass even with one attempt.

Why: PutEvents' 200 becomes unverifiable over the wire. Code written against AWS that ingests real-time events and then exports them with ingestionMode=PUT to confirm ingestion (a pipeline validation step, a test, an analytics hand-off) parses the export by AWS's PUT column names and reads an empty result set, so it concludes the events were lost even though the emulator holds them — and any consumer keyed on event_type/item_id silently drops every row.

Fix: the export's column naming in the dataset-export path of the personalize control plane — the column renaming applied to exported rows, and the header-row construction that follows it — must keep a PUT-sourced record's own column names and apply the schema's declared spelling only to BULK rows (an ALL export carries both halves as each half names them). The status progression of the export job and of the stages above it belongs to the same component's create-status clock.

Traces: personalize-events.put-events-minimal-fields-success (PutEvents 200, then an ingestionMode=PUT export whose rows carry no "event_type" column, so the terminal read finds zero rows and the check times out at 60s).

Cause 2 of 2: PutEvents refuses a reserved keyword in Event.properties with the right error code and status but with emulator-authored wording instead of the sentence AWS words that refusal with. Failing check: personalize-events.put-events-reserved-property-keyword-rejected.

AWS: Event$properties documents "The following can't be included as a keyword for properties (case insensitive)": userId, sessionId, eventType, timestamp, recommendationId, impression (Smithy: com.amazonaws.personalizeevents#Event$properties). A PutEvents call whose properties blob uses one of them is refused with InvalidInputException (HTTP 400, the only error PutEvents models). Its message is, as recorded: "eventtype is not allowed in properties. Please refer to documentation to include eventtype" — fixed text "<key> is not allowed in properties. Please refer to documentation to include <key>", whose one variable part is the offending reserved keyword, LOWER-CASED, taken from the request's properties object and interpolated twice; "eventtype" here is that template filled by this request's "eventType" key, not a literal.

Emulator: the refusal is raised with code InvalidInputException and HTTP 400 (correct), but the message is "Event.properties must not use the reserved key eventType; Personalize reserves it case insensitively" — a different sentence, naming the input member ("Event.properties"), echoing the canonical camelCase spelling of the reserved key rather than the lower-cased one, and appending a clause AWS does not emit. The positive-control sibling call (same tracker/session, non-reserved key numberOfRatings) is accepted with 200, so only the wording diverges.

Why: a client that classifies or surfaces this refusal by its message — an error mapper, a log assertion, a retry/repair path that strips the offending key by parsing it out of the message — does not recognize the emulator's sentence, and an operator reading the two side by side cannot match them. Message text of a modeled error is part of the contract a client codes against.

Fix: the shared properties validation of the personalize-events ingestion service (the reserved-key branch of the properties parser that both PutEvents and PutActionInteractions call, and the reserved-key table it matches against) must emit AWS's sentence, interpolating the lower-cased offending key twice; the handler's error class and status are already right.

Traces: personalize-events.put-events-reserved-property-keyword-rejected (asserts the exact InvalidInputException message for a properties blob keyed "eventType").

## Evidence
Cause 1 — personalize-events.put-events-minimal-fields-success reports, with no botocore error at all:
  Check timed out after 60.0s

Expected (AWS): PutEvents(trackingId=<id>, sessionId=<session>, eventList=[{eventType=<type>, sentAt=<now>, itemId=<item>}]) -> HTTP 200 with an EMPTY body (the response carries only ResponseMetadata). A subsequent personalize.CreateDatasetExportJob(datasetArn=<interactions dataset>, ingestionMode="PUT", jobOutput={s3DataDestination:{path:"s3://<bucket>/<prefix>/"}}, roleArn=<role>) reaches status ACTIVE, and the CSV objects written under that prefix have a header row naming the real-time record's own columns, including "event_type" and "item_id", with exactly one row whose event_type equals the eventType just sent (and an empty user_id, since the call sent none).
Actual: the export job is created and reaches ACTIVE, and an object IS written, but its header row carries the Interactions schema's declared spellings (USER_ID,ITEM_ID,TIMESTAMP,EVENT_TYPE) for PUT-sourced rows, so selecting rows by "event_type" yields none; the client retries the export on its cadence and the check is killed at the 60s budget.

Code: services/service-com-amazonaws-personalize-amazonpersonalize/src/state/ingested.rs lines 33-35 and 191-219 (a PUT row is built with the columns "timestamp", "event_type", plus user_id/item_id); services/service-com-amazonaws-personalize-amazonpersonalize/src/state/export.rs lines 197-255 (schema_fields + exported_column rename EVERY exported column to the schema's spelling via eq_ignore_ascii_case, for every ingestion mode) and lines 264-310 (export_columns/render_csv emit that renamed header); services/service-com-amazonaws-personalize-amazonpersonalize/src/ops/create_dataset_export_job.rs lines 223-265 (calls export_rows/render_csv/write_export) and line 294 (active_at = due_after(now, dataset_active_at)); services/service-com-amazonaws-personalize-amazonpersonalize/src/state.rs lines 358-402 (CREATE_SPAN = 500ms, creation_status, terminal_status — the contributing per-stage poll cost).

Cause 2 — personalize-events.put-events-reserved-property-keyword-rejected fails on the message assertion:
  AssertionError: unexpected reserved-keyword message 'Event.properties must not use the reserved key eventType; Personalize reserves it case insensitively'

Expected: Error.Code == "InvalidInputException", HTTP 400, Error.Message == "eventtype is not allowed in properties. Please refer to documentation to include eventtype" (fixed text with the lower-cased offending key interpolated twice).
Actual: Error.Code == "InvalidInputException", HTTP 400, Error.Message == "Event.properties must not use the reserved key eventType; Personalize reserves it case insensitively".
The sibling control call with properties {"numberOfRatings": "12"} is accepted with 200, as AWS does.

Code: services/service-com-amazonaws-personalizeevents-amazonpersonalizeevents/src/state.rs lines 94-101 (RESERVED_PROPERTY_KEYS) and lines 111-135 (parse_properties' reserved-key branch formats the diverging sentence); services/service-com-amazonaws-personalizeevents-amazonpersonalizeevents/src/ops/put_events.rs lines 217-225 (calls parse_properties("Event.properties", raw, true)) and lines 266-271 (lifts it into InvalidInputException).

## Steps to Reproduce
Cause 1 (PUT-mode export column names):
1. Boot the emulator.
2. personalize.CreateSchema(name=<name>, schema=<Interactions record with fields USER_ID string, ITEM_ID string, TIMESTAMP long, EVENT_TYPE string>) -> 200.
3. personalize.CreateDatasetGroup(name=<name>) -> 200; poll DescribeDatasetGroup until status "ACTIVE".
4. personalize.CreateDataset(name=<name>, datasetGroupArn=<arn>, datasetType="Interactions", schemaArn=<arn>) -> 200; poll DescribeDataset until "ACTIVE". personalize.CreateEventTracker(name=<name>, datasetGroupArn=<arn>) -> 200; poll DescribeEventTracker until "ACTIVE" and take its trackingId.
5. personalize-events.PutEvents(trackingId=<trackingId>, sessionId=<session>, eventList=[{eventType="<type>", sentAt=<now>, itemId="<item>"}]) -> expect HTTP 200 with an empty body (no userId is sent).
6. s3.CreateBucket(<bucket>); iam.CreateRole + PutRolePolicy granting s3:PutObject/s3:ListBucket on that bucket.
7. personalize.CreateDatasetExportJob(jobName=<name>, datasetArn=<interactions dataset arn>, ingestionMode="PUT", jobOutput={"s3DataDestination":{"path":"s3://<bucket>/<prefix>/"}}, roleArn=<role arn>) -> 200; poll DescribeDatasetExportJob until status "ACTIVE".
8. s3.ListObjectsV2(Bucket=<bucket>, Prefix="<prefix>/") then GetObject each key and parse the CSV. Expected: a header row containing "event_type" and "item_id" and exactly one row with event_type == the eventType from step 5. Actual: the header row reads USER_ID,ITEM_ID,TIMESTAMP,EVENT_TYPE, so a reader keyed on "event_type" finds zero rows.
9. Also note the per-stage cost: each Describe* in steps 3, 4 and 7 issued immediately after its create reports a transient status ("CREATE PENDING"), so a client polling at AWS cadences sleeps a full interval per stage before the first export can even be read.

Cause 2 (reserved properties keyword message):
1. Boot the emulator and obtain an ACTIVE event tracker's trackingId as in steps 2-4 above.
2. personalize-events.PutEvents(trackingId=<trackingId>, sessionId=<session>, eventList=[{eventType="click", sentAt=<now>, itemId="item-1", properties=json({"eventType": "reserved-collision"})}]).
   Expected: HTTP 400, Code "InvalidInputException", Message "eventtype is not allowed in properties. Please refer to documentation to include eventtype".
   Actual: HTTP 400, Code "InvalidInputException", Message "Event.properties must not use the reserved key eventType; Personalize reserves it case insensitively".
3. Repeat with properties=json({"numberOfRatings": "12"}) -> HTTP 200 (unchanged, correct). Repeat step 2 with each of userId, sessionId, timestamp, recommendationId, impression as the properties key: each must be refused with the same template filled by the lower-cased key.
