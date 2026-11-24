Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: personalize-events.put-items-success personalize-events.put-items-max-batch-ten-items-success personalize-events.put-items-property-update-reflected-in-export personalize-events.put-users-success personalize-events.put-users-max-batch-ten-users-success
Issue-Shape-Id: com.amazonaws.personalizeevents#AmazonPersonalizeEvents
Issue-Key: personalize-events.put-items-success|_export_until_item_appears#11|KeyError|matching = [r for r in last_rows if r["ITEM_ID"] == item_id]
Issue-Key: personalize-events.put-items-max-batch-ten-items-success|run#15|KeyError|by_id = {r["ITEM_ID"]: r for r in rows}
Issue-Key: personalize-events.put-items-property-update-reflected-in-export|run#16|KeyError|matching = [r for r in rows if r["ITEM_ID"] == item_id]
Issue-Key: personalize-events.put-users-success|_await_exported_row#11|KeyError|matching = [r for r in rows if r["USER_ID"] == user_id]
Issue-Key: personalize-events.put-users-max-batch-ten-users-success|run#18|KeyError|exported = {r["USER_ID"]: r["AGE"] for r in rows if r["USER_ID"] in user_ids}
Issue-Commit: 9353c1bb256472de141f0c55c99b9667dc6fcfc6

# Dataset export of PUT-ingested records names its CSV columns after the emulator's internal canonical names, not the dataset schema's declared fields

> _Note: this issue was identified against the code base at commit `9353c1bb256472de141f0c55c99b9667dc6fcfc6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutItems

## Description
Cause 1 of 1: records ingested through the Amazon Personalize Events data plane (PutItems / PutUsers) are exported by a Personalize dataset export job (ingestionMode=PUT) under column names the emulator invents internally instead of the field names the dataset's schema declares, so no client can address the exported rows by the column names it wrote in its schema. Failing checks: personalize-events.put-items-success, personalize-events.put-items-max-batch-ten-items-success, personalize-events.put-items-property-update-reflected-in-export, personalize-events.put-users-success, personalize-events.put-users-max-batch-ten-users-success.

AWS: recorded behavior only — neither the personalize-events model nor the personalize model states the on-disk layout of an export object; the behavioral recordings do. The recordings create a dataset whose schema declares its fields in upper case (an Items schema with fields ITEM_ID and PRICE; a Users schema with USER_ID and AGE), PutItems/PutUsers the entities with properties written in the JSON the client chooses ({"price": <number>} / {"age": <number>}), then run CreateDatasetExportJob(ingestionMode="PUT") and read the written object as CSV with a header row. On AWS the header row names the SCHEMA's declared fields — the exported rows are keyed ITEM_ID / PRICE and USER_ID / AGE — and each row carries the entity id in the schema's id field and the property values the put supplied. That is, AWS exports the PUT half of a dataset under the schema's declared spelling, matching a put property to a schema field case-insensitively, the same naming the BULK half uses. The fixed part of the requirement is "every exported column is named as the dataset's schema declares that field"; the particular field names (ITEM_ID, PRICE, USER_ID, AGE) and values come from the schema and the request in each run, not from one recording.

Emulator: the export object is written (the checks' `assert keys` on the S3 listing passes and the job reaches ACTIVE), but its header row does not contain the schema's field names. csv.DictReader yields rows with no 'ITEM_ID' key, so the checks raise KeyError: 'ITEM_ID' (and KeyError: 'USER_ID' for the Users checks) on the first row lookup. The emulator writes the PUT half under its own canonical id column names (`item_id`, `user_id`, `action_id`) and the raw `properties` keys exactly as the caller sent them (`price`, `age`), deliberately: state/ingested.rs says "under the column names that record itself carries — a real-time record is exported as it was ingested, never renamed to the dataset schema's spellings", and export_rows applies the schema-name mapping (`schema_named_row`/`exported_column`) only to the BULK rows. So an export of PUT data is keyed `item_id,price` where AWS keys it `ITEM_ID,PRICE`.

Why: a client that declares a schema and then reads its own export back cannot find any column it declared. Every downstream consumer — a CSV reader addressing columns by schema field name, a re-import of an export back into a dataset, any check that a put landed — reads empty or raises KeyError, even though the data itself is present. The divergence is silent: the job reports ACTIVE and the object exists, so nothing signals that the column names are wrong.

Fix: the export's row/column naming for the PUT half — state::export::export_rows in the personalize crate (which maps only `dataset.imported` through `schema_named_row`) together with the rows state::ingested::put_rows builds. The schema-field mapping that already exists for imported rows (`exported_column`, case-insensitive match against `schema_fields`) must also be applied to every column of a PUT row: the id column and each merged `properties` key. The header-row rendering in render_csv/export_columns then needs no change.

Traces: personalize-events.put-items-success, personalize-events.put-items-max-batch-ten-items-success, personalize-events.put-items-property-update-reflected-in-export, personalize-events.put-users-success, personalize-events.put-users-max-batch-ten-users-success. Code: services/service-com-amazonaws-personalize-amazonpersonalize/src/state/export.rs lines 219-273 (exported_column / schema_named_row / export_rows) and services/service-com-amazonaws-personalize-amazonpersonalize/src/state/ingested.rs lines 14-22, 180-195 (entity_rows inserting ITEM_ID_COLUMN="item_id" and merging raw property names); services/service-com-amazonaws-personalize-amazonpersonalize/src/state/import.rs lines 330-335 (the canonical lower-case column constants).

## Evidence
Expected (AWS recording): after PutItems(datasetArn=<Items dataset whose schema declares fields ITEM_ID, PRICE>, items=[{itemId: "item-<i>", properties: "{\"price\": <i>.0}"}]) and CreateDatasetExportJob(ingestionMode="PUT"), the object under the job output prefix is CSV whose header row names the schema's fields, so csv.DictReader rows have r["ITEM_ID"] == "item-<i>" and float(r["PRICE"]) == <i>.0. Same for Users: r["USER_ID"], r["AGE"].

Actual (emulator): the export job reaches status ACTIVE and an object is written, but its header row carries no schema field names; reading it yields rows without those keys and the checks fail with:
  put-items-success: KeyError: 'ITEM_ID' at `matching = [r for r in last_rows if r["ITEM_ID"] == item_id]`
  put-items-max-batch-ten-items-success: KeyError: 'ITEM_ID' at `by_id = {r["ITEM_ID"]: r for r in rows}`
  put-items-property-update-reflected-in-export: KeyError: 'ITEM_ID' at `matching = [r for r in rows if r["ITEM_ID"] == item_id]`
  put-users-success: KeyError: 'USER_ID' at `matching = [r for r in rows if r["USER_ID"] == user_id]`
  put-users-max-batch-ten-users-success: KeyError: 'USER_ID' at `exported = {r["USER_ID"]: r["AGE"] for r in rows if r["USER_ID"] in user_ids}`
The emulator's own source states the divergence: state/ingested.rs documents PUT rows as exported "never renamed to the dataset schema's spellings", and the id columns are the lower-case constants item_id / user_id / action_id while properties keep the caller's raw JSON key (price / age). No HTTP status or error code diverges here — both PutItems/PutUsers and CreateDatasetExportJob answer 200; the divergence is in the exported object's column naming.

## Steps to Reproduce
1. personalize: CreateDatasetGroup, then CreateSchema for an Items schema whose fields are declared as ITEM_ID (string) and PRICE (float), then CreateDataset(datasetType="Items") with that schema; wait for ACTIVE.
2. personalize-events: PutItems(datasetArn=<that dataset>, items=[{itemId: "item-0", properties: "{\"price\": 0.0}"}]) -> 200.
3. personalize: CreateDatasetExportJob(datasetArn=<that dataset>, ingestionMode="PUT", jobOutput.s3DataDestination.path="s3://<bucket>/export/", roleArn=<role>); poll DescribeDatasetExportJob until ACTIVE.
4. s3: ListObjectsV2(Bucket=<bucket>, Prefix="export/") and GetObject the written key; parse the body with csv.DictReader.
5. Observe the header row: on AWS it names the schema's fields (ITEM_ID,PRICE) so r["ITEM_ID"] resolves; on the emulator it names item_id and price, so r["ITEM_ID"] raises KeyError.
6. Repeat with a Users schema declaring USER_ID, AGE and PutUsers to see KeyError: 'USER_ID'.
