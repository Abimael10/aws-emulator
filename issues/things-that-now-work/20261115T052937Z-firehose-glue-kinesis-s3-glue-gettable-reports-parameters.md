Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: firehose__glue__kinesis__s3.kds-create-delivery-stream-accepts-a-glue-table-with-no-iceberg-marker
Issue-Key: firehose__glue__kinesis__s3.kds-create-delivery-stream-accepts-a-glue-table-with-no-iceberg-marker|run#8|AssertionError|assert "Parameters" not in described, (
Issue-Commit: 07acbfe4a3d7fac5eb5dd49030154c6544330d6e

# firehose__glue__kinesis__s3: Glue GetTable reports Parameters as an empty map for a table created with no Parameters, so "no table properties" is indistinguishable from "an empty property set"

> _Note: this issue was identified against the code base at commit `07acbfe4a3d7fac5eb5dd49030154c6544330d6e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTable

## Description
Cause 1 of 1: Glue GetTable serializes Table.Parameters as a present-but-empty map for a table whose CreateTable TableInput carried no Parameters at all. Failing check: firehose__glue__kinesis__s3.kds-create-delivery-stream-accepts-a-glue-table-with-no-iceberg-marker.

AWS: Smithy com.amazonaws.glue#Table$Parameters targets ParametersMap and is OPTIONAL — it carries no @required and no @default, so it is a nullable member. Documented as "These key-value pairs define properties associated with the table." A table created from a TableInput that set no Parameters therefore comes back from GetTable with the Parameters member ABSENT from the Table structure; the member appears only once at least one table property exists. This matters semantically for the Iceberg destination rules, where the PRESENCE of a table property such as table_type=ICEBERG (or its absence) is what marks a Glue table as an Iceberg table.
Emulator: GetTable on a table created with no Parameters answers 200 with Table.Parameters present and equal to {} (an empty map). Replay evidence: `AssertionError: the unmarked table already carries Parameters {}; this check needs a table with no Iceberg marker of any kind` — the check asserts `"Parameters" not in described` on the GetTable Table payload and the member is there.
Why: a client cannot distinguish "this table declares no properties" from "this table declares an empty property set". Code that branches on `if "Parameters" in table` (the idiomatic boto3/SDK optional-member test, and exactly how an Iceberg-marker probe is written) takes the wrong branch for every unmarked table, and round-tripping a GetTable result back through UpdateTable writes an empty Parameters map where AWS wrote nothing. It also makes the "unmarked Glue table" precondition of Firehose's Iceberg destination validation impossible to set up against the emulator.
Fix: the GetTable handler's projection of the stored table into the Table output shape, and the stored-table representation it reads: an unset Parameters map must serialize as an absent member, not as an empty map. The same projection is shared by GetTables/SearchTables and by CreateTable's echo, so the omission rule belongs in the table-to-output conversion rather than in one operation.
Traces: firehose__glue__kinesis__s3.kds-create-delivery-stream-accepts-a-glue-table-with-no-iceberg-marker.

Related open issues do not cover this cause: 20261109T135435Z-firehose-glue-kinesis-s3-the-iceberg-destination-differential.md is about the Iceberg destination never materialising a table, and 20261108T175930Z-athena-cloudformation-glue-glue-gettable-over-a-database.md is about GetTable's not-found refusal wording. Neither carries this check's replay key or this optional-member serialization defect.

## Evidence
Failing check (mismatch): firehose__glue__kinesis__s3.kds-create-delivery-stream-accepts-a-glue-table-with-no-iceberg-marker

Request sequence: glue.create_table(DatabaseName=<db>, TableInput={Name: <table>, StorageDescriptor: {...}}) with NO Parameters member, then glue.get_table(DatabaseName=<db>, Name=<table>).

Expected per AWS (Smithy glue#Table$Parameters is an optional ParametersMap, neither @required nor @default): the returned Table structure has NO "Parameters" key.
Actual from the emulator: the returned Table structure has "Parameters": {}.

Replay output:
  File "check_firehose__glue__kinesis__s3_kds_create_delivery_stream_accepts_a_glue_table_with_no_iceberg_marker.py", line 247, in run
    assert "Parameters" not in described, (
  AssertionError: the unmarked table already carries Parameters {}; this check needs a table with no Iceberg marker of any kind

The empty map is the emulator's own default-construction of an unset optional member; no value in the recording is account-, clock- or run-specific.

## Steps to Reproduce
1. glue.create_database(DatabaseInput={"Name": "<db>"}).
2. glue.create_table(DatabaseName="<db>", TableInput={"Name": "unmarked_table", "TableType": "EXTERNAL_TABLE", "StorageDescriptor": {"Location": "s3://<bucket>/<prefix>/", "Columns": [...]}}) — deliberately pass NO "Parameters" member.
3. glue.get_table(DatabaseName="<db>", Name="unmarked_table").
4. Inspect the response's ["Table"] dict: AWS has no "Parameters" key; the emulator returns "Parameters": {}.
5. Control: repeat with TableInput["Parameters"] = {"table_type": "ICEBERG"} — both AWS and the emulator return that map, so the divergence is confined to the unset case.
