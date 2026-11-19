Issue-Source: trace-sweep-divergence
Issue-Trace: athena-data-catalog-create-conflict-and-delete-not-found-errors
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: athena-data-catalog-create-conflict-and-delete-not-found-errors
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena|CreateDataCatalog|2xx->2xx|$
Issue-Key: athena|DeleteDataCatalog|2xx->2xx|$
Issue-Commit: f97a2791479cf3e78acbdef1e7d516a9feec11d8

# Athena CreateDataCatalog and DeleteDataCatalog return a DataCatalog body where AWS returns an empty response

> _Note: this issue was identified against the code base at commit `f97a2791479cf3e78acbdef1e7d516a9feec11d8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDataCatalog

## Description
Two net-new root causes from the wire replay of simparity-wire-aws-v1: both data-catalog mutations serialize a full DataCatalog structure onto a response AWS returns empty. Both clusters carry an empty stub_path and an empty owned_by. Each cause must be closed separately; there are 2 of them.

Related open issues that do NOT cover these causes: 20261114T204410Z-athena-workgroup-and-data-catalog-reads-still-drop.md (GetDataCatalog/GetWorkGroup READ responses dropping members — the opposite defect, on the read path) and 20261117T053359Z-athena-7-net-new-causes-the-built-in.md (the reserved AwsDataCatalog's get/delete refusals, not the success-path body).

Cause 1 of 2: CreateDataCatalog's success response carries a DataCatalog member.
AWS: Smithy CreateDataCatalog(Name @required @length 1..256, Type @required enum, Description, Parameters, Tags) -> CreateDataCatalogOutput{DataCatalog} — the output member exists in the model but is OPTIONAL, and the recorded AWS response for a successful create is HTTP 200 with an empty JSON object body: no DataCatalog, no other member (recorded behavior only; the model states no requirement either way). The created catalog is read back through GetDataCatalog/ListDataCatalogs, not from the create response.
Emulator: HTTP 200 with a populated body — {'DataCatalog': {'Name': <name>, 'Description': <description>, 'Type': <type>, 'Parameters': {...}, 'Status': 'CREATE_COMPLETE'}} — i.e. the whole created resource echoed back, including a Status the create response never carries.
Why: a client written against AWS reads the catalog's state from GetDataCatalog after creating it; against the emulator the create response already looks authoritative, so code that asserts the member is absent (or that exercises the read-back path only when it is) diverges, and any client that trusts the echoed Status believes the catalog's provisioning state was reported by the create call when AWS has not reported it yet.
Fix: the CreateDataCatalog response serializer in the athena service crate — the created catalog must be committed to state but the response body must be empty.
Traces: athena-data-catalog-create-conflict-and-delete-not-found-errors#task#1.

Cause 2 of 2: DeleteDataCatalog's success response carries a DataCatalog member.
AWS: Smithy DeleteDataCatalog(Name @required, DeleteCatalogOnly default false) -> DeleteDataCatalogOutput{DataCatalog}, again an optional output member; the recorded AWS response for a successful delete is HTTP 200 with an empty JSON object body (recorded behavior only).
Emulator: HTTP 200 with {'DataCatalog': {'Name': <name>, 'Description': <description>, 'Type': <type>, 'Parameters': {...}, 'Status': 'DELETE_COMPLETE'}} — the deleted resource echoed back with a terminal Status AWS does not return here.
Why: same class of defect on the delete path — a client that distinguishes 'deleted' from 'still deleting' by the presence of a Status in the response gets a signal AWS never sends, and a client that asserts an empty delete body fails against the emulator.
Fix: the DeleteDataCatalog response serializer in the athena service crate — remove the catalog from state and answer with an empty body.
Traces: athena-data-catalog-create-conflict-and-delete-not-found-errors#task#5.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (42 fixtures, 433 steps, 372 passed, score 85.9%). Both clusters report stub_path '' and owned_by [], expected status 200 and actual status 200, diverging on the whole document root:

Cause 1 — CreateDataCatalog, athena-data-catalog-create-conflict-and-delete-not-found-errors#task#1:
  $: value_mismatch (expected {}, got {'DataCatalog': {'Name': <catalog name from the request>, 'Description': <description from the request>, 'Type': 'GLUE', 'Parameters': {'catalog-id': <account id>}, 'Status': 'CREATE_COMPLETE'}})

Cause 2 — DeleteDataCatalog, athena-data-catalog-create-conflict-and-delete-not-found-errors#task#5:
  $: value_mismatch (expected {}, got {'DataCatalog': {'Name': <catalog name from the request>, 'Description': <description from the request>, 'Type': 'GLUE', 'Parameters': {'catalog-id': <account id>}, 'Status': 'DELETE_COMPLETE'}})

(The catalog name, description, type, catalog-id parameter and account id above are values taken from the fixture's own request and the emulator's state; the requirement is only that the response body is empty.)

Smithy consulted (services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json): CreateDataCatalog -> CreateDataCatalogOutput{DataCatalog} and DeleteDataCatalog -> DeleteDataCatalogOutput{DataCatalog}, neither member @required; the recorded AWS responses are empty objects.

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from it; replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture athena-data-catalog-create-conflict-and-delete-not-found-errors to step task#1 — call CreateDataCatalog(Name=<new>, Type='GLUE', Description=<text>, Parameters={'catalog-id': <account>}) and inspect the response body: it carries a full DataCatalog with Status 'CREATE_COMPLETE' where AWS returns {}.
3. Cause 2: continue the same fixture to step task#5 — call DeleteDataCatalog(Name=<that catalog>) and inspect the response body: it carries a full DataCatalog with Status 'DELETE_COMPLETE' where AWS returns {}.
4. Re-replay both fixture#step bindings after fixing; both must pass before closing.
