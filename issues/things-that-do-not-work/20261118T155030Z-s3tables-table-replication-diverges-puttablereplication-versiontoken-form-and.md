Issue-Source: trace-sweep-divergence
Issue-Trace: s3tables-table-replication
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: s3tables-table-replication
Issue-Shape-Id: com.amazonaws.s3tables#S3TableBuckets
Issue-Key: s3tables|PutTableReplication|2xx->2xx|$.versionToken
Issue-Key: s3tables|GetTableReplication|4xx->2xx|$.message
Issue-Key: s3tables|GetTableReplication|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: s3tables|GetTableReplication|4xx->2xx|status
Issue-Commit: 269634c07d88de01ddfa9deb1de7fc2f19cd46b4

# s3tables table-replication diverges: PutTableReplication versionToken form and GetTableReplication missing-configuration 404

> _Note: this issue was identified against the code base at commit `269634c07d88de01ddfa9deb1de7fc2f19cd46b4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutTableReplication

## Description
Two unowned s3tables wire divergences on the table-replication route, both in the replication configuration's own state handling: the opaque versionToken PutTableReplication mints is not of the form the service returns, and GetTableReplication keeps answering 200 with a configuration after the recording has the replication configuration removed, so its modeled NotFoundException is unreachable. 2 causes, in s3tables version-token minting/serialization and in the replication state's absence modelling behind GetTableReplication.

Related open issues (the replay reports both clusters unowned, so neither covers them): 20261102T065133Z-s3tables-encryption-metrics-replication-put-operations-answer-501.md says the replication Put operations answer 501 NotImplemented — PutTableReplication now answers 200 on the wire, it answers with the wrong token form, so that characterization no longer holds; 20261115T045515Z-s3tables-2-net-new-causes-createtable-versiontoken-is.md carries the same token-form fault for the CreateTable cluster only; 20261118T151519Z-s3tables-2-net-new-causes-deletetablereplication-s-stale.md and 20261118T151519Z-s3tables-deletetablereplication-stub.md are behavioral/stub issues about DeleteTableReplication's refusal paths and carry no wire replay key for the GetTableReplication read below.

Cause 1 of 2: PutTableReplication.versionToken is not minted in the token form AWS returns.
AWS: Smithy — PutTableReplication (PUT /table-replication, success code 200) returns a required output member versionToken of type String; the model constrains only the type, so the token's form is recorded behavior only. The recorded 200 response carries a server-minted opaque token that is a single run of 20 lowercase hexadecimal characters with no hyphens or other separators (the recording's literal is a per-run value and is not itself the requirement). The requirement is the class of token: the same opaque handle must then be echoed by GetTableReplication and accepted as the precondition versionToken by a following PutTableReplication/DeleteTableReplication.
Emulator: PutTableReplication answers 200 but sets versionToken to a hyphenated 8-4-4-4-12 UUID (replay detail: "$.versionToken: value_mismatch (expected 'f7a86fd2eb82f2ad41a9', got '1f5058b6-e964-456a-ac49-bad2c6ec1b54')"), which is a different token class from the recorded one — the same generator the CreateTable cluster was filed for.
Why: versionToken is an opaque optimistic-concurrency handle a client stores and re-submits. A client or fixture that recognises or validates the service's token form rejects the emulator's value, and a token of the wrong class stops correlating with tokens captured against AWS, so replication concurrency-control code built on AWS tokens is never exercised correctly.
Fix: the version-token generator in the s3tables replication state (the single place a replication version token is minted) so PutTableReplication and every read-back/precondition path emit the service's token form; the PutTableReplication response serializer needs no change beyond emitting the minted value unchanged.
Traces: s3tables-table-replication#task#6 (PutTableReplication 200, $.versionToken).

Cause 2 of 2: GetTableReplication answers 200 with a configuration for a table that has no replication configuration, instead of refusing 404 NotFoundException.
AWS: Smithy — GetTableReplication (GET /table-replication, success code 200) requires the output members versionToken and configuration, and models NotFoundException @httpError 404 among its errors (NotFoundException is the service's not-found error class, serialized with the x-amzn-errortype header). The recording drives the table's replication configuration to removal and then calls GetTableReplication, which answers HTTP 404 with error type NotFoundException and the message 'The specified replication configuration does not exist.' — that message is entirely fixed text for this error class: it interpolates nothing from the request or from state.
Emulator: the same read answers HTTP 200 and serializes a replication configuration, with no message and no x-amzn-errortype header (replay detail: "status: expected 404, got 200; $.message: missing (expected 'The specified replication configuration does not exist.', got None); $headers.x-amzn-errortype: missing (expected 'NotFoundException:...', got None)"). Either the removal of the configuration leaves the stored state in place or the read path has no absent-configuration branch at all, so the modeled 404 is unreachable on this route.
Why: a client cannot tell 'this table has no replication configuration' from 'this table replicates'. Against AWS the 404 is the signal to create a configuration or to stop treating the table as replicated; against the emulator the read always succeeds, so teardown/verification logic concludes a configuration still exists, idempotent create-if-absent flows never fire, and code that branches on NotFoundException is dead.
Fix: the GetTableReplication handler and the s3tables replication state it reads — the state must record the absence of a replication configuration once it is removed, and the handler must refuse with 404 NotFoundException carrying the fixed message above (and the NotFoundException error-type header) when the table has none, instead of serializing a configuration.
Traces: s3tables-table-replication#task#10 (GetTableReplication, expected 404, got 200).

## Evidence
Cause 1 — PutTableReplication, expected status 200, actual 200; divergence key service=s3tables|PutTableReplication|status=200->200|keys=$.versionToken. Replay detail: "$.versionToken: value_mismatch (expected 'f7a86fd2eb82f2ad41a9', got '1f5058b6-e964-456a-ac49-bad2c6ec1b54')". Affected: s3tables-table-replication#task#6 (1 step). Expected token class: an opaque server-minted handle of 20 lowercase hex characters, no separators; actual: hyphenated 8-4-4-4-12 UUID. Both literals are per-run values; the token's form is the requirement.

Cause 2 — GetTableReplication, expected status 404, actual 200; divergence key service=s3tables|GetTableReplication|status=404->200|keys=$.message,$headers.x-amzn-errortype,status. Replay detail: "status: expected 404, got 200\n$.message: missing (expected 'The specified replication configuration does not exist.', got None)\n$headers.x-amzn-errortype: missing (expected 'NotFoundException:http://internal.amazon.com/coral/com.amazon.s3tablebuckets/', got None)". Affected: s3tables-table-replication#task#10 (1 step). Expected: HTTP 404, error type NotFoundException, message exactly 'The specified replication configuration does not exist.' (all fixed text, nothing interpolated). Actual: HTTP 200 with a serialized replication configuration, no message, no x-amzn-errortype header.

## Steps to Reproduce
Cause 1: replay fixture s3tables-table-replication step task#6 — PutTableReplication(tableArn=<ICEBERG table in the source table bucket>, configuration={role, rules:[{destinations:[{destinationTableBucketARN:<destination bucket>}]}]}). The response is 200, but $.versionToken is a hyphenated UUID rather than the opaque 20-hex-character token form the recording shows. Read the same token back with GetTableReplication to see the same generator.

Cause 2: replay fixture s3tables-table-replication through step task#10 — after the recorded sequence removes the table's replication configuration, call GetTableReplication(tableArn=<same table>). AWS answers 404 with x-amzn-errortype NotFoundException and message 'The specified replication configuration does not exist.'; the emulator answers 200 and serializes a replication configuration with no error fields.
