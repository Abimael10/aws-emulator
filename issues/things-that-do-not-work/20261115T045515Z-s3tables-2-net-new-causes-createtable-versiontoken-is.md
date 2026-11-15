Issue-Source: trace-sweep-divergence
Issue-Trace: s3tables-rename-table-across-namespaces
Issue-Step: setup#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: s3tables-rename-table-across-namespaces s3tables-s3t-uppercase-and-hyphenated-table-names-rejected s3tables-sct-create-table-input-mistakes s3tables-sct-namespace-string-and-delete-while-in-use s3tables-sct-sort-order-at-create-via-write-order s3tables-table-replication s3tables-table-bucket-config-error-paths s3tables-table-bucket-configuration-error-paths s3tables-table-bucket-policy-malformed
Issue-Shape-Id: com.amazonaws.s3tables#S3TableBuckets
Issue-Key: s3tables|CreateTable|2xx->2xx|$.versionToken
Issue-Key: s3tables|PutTableBucketPolicy|4xx->4xx|$.message
Issue-Commit: 07acbfe4a3d7fac5eb5dd49030154c6544330d6e

# s3tables: 2 net-new causes — CreateTable versionToken is minted in the wrong token form and PutTableBucketPolicy's malformed-policy refusal text is rewritten

> _Note: this issue was identified against the code base at commit `07acbfe4a3d7fac5eb5dd49030154c6544330d6e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTable

## Description
Two unowned s3tables wire divergences, both in values the service mints or words itself: the opaque versionToken CreateTable hands back does not have the recorded token form, and the refusal for a malformed table-bucket resource policy carries emulator-authored text instead of AWS's message. 2 causes, in s3tables version-token minting/serialization and in PutTableBucketPolicy's policy validation.

Related open issues (neither owns these clusters; the replay reports them unowned): 20261103T033559Z-s3tables-list-and-get-responses-emit-epoch-timestamps.md carries the versionToken fault for the GetTable cluster and asserts the recorded id form is a hyphenated UUID — the replay still rejects the emulator's hyphenated-UUID versionToken, so that characterization does not hold for versionToken and the CreateTable cluster is net-new here. 20261106T132944Z-s3tables-puttablebucketpolicy-stub.md is a stale stub issue: PutTableBucketPolicy now answers on the wire (the replay reports no stub for it), it answers with the wrong message.

Cause 1 of 2: CreateTable.versionToken is not minted in the token form AWS returns.
AWS: Smithy: CreateTable output member versionToken targets com.amazonaws.s3tables#VersionToken, a required string with @length min 1 max 2048 — the model constrains only length, so the form is recorded behavior only. The recorded CreateTable response is 200 and carries a server-minted opaque token whose shape the corpus asserts as a class (the recorded literal is withheld as '<shaped-not-asserted>' because the value itself is per-run). The requirement is the form/class of the minted token, not any one recorded string: the same token must then be echoed by GetTable and accepted as the precondition token by UpdateTableMetadataLocation/DeleteTable.
Emulator: CreateTable returns 200 with versionToken set to a hyphenated 8-4-4-4-12 UUID (e.g. 'e4276912-458a-445c-8834-03b86a20f04d'), which does not conform to the recorded token's asserted shape; the same generator is used for the GetTable read-back ('76d065b5-b91f-44eb-ab4e-d8cf74c9d9f7').
Why: versionToken is an opaque handle a client stores and re-submits as an optimistic-concurrency precondition, and clients/fixtures that recognise or validate the service's token form reject the emulator's value; a token of the wrong class also stops correlating with tokens recorded elsewhere, so concurrency tests built against AWS do not line up.
Fix: the versionToken generator in the s3tables state layer (the single place every operation mints a table version token), so CreateTable and every read-back/precondition path emit the service's token form; the CreateTable response serializer needs no change beyond emitting the minted value unchanged.
Traces: s3tables-rename-table-across-namespaces#setup#4 (representative, CreateTable 200, $.versionToken), plus s3tables-s3t-uppercase-and-hyphenated-table-names-rejected#task#6, s3tables-sct-create-table-input-mistakes#task#7 and #task#8, s3tables-sct-namespace-string-and-delete-while-in-use#task#4, s3tables-sct-sort-order-at-create-via-write-order#task#5 and #task#8, s3tables-table-replication#setup#5 — 8 CreateTable steps in the same cluster.

Cause 2 of 2: PutTableBucketPolicy refuses a malformed policy document with its own wording.
AWS: Smithy: PutTableBucketPolicy input resourcePolicy targets ResourcePolicy (string, @length min 1 max 20480); the operation models BadRequestException (httpError 400, doc 'The request is invalid or malformed.'). The recorded refusal of a policy document that is not a well-formed/valid IAM policy is HTTP 400 with the message 'The specified policy is malformed' — that message is entirely fixed text, with no parts interpolated from the request or from state.
Emulator: answers 400 for the same request but with the message 'The specified resource policy is not valid.'
Why: the status matches, so clients that branch or assert on the refusal message — error-mapping layers, retry/classification logic, and tests that match AWS's malformed-policy wording — treat the emulator's refusal as a different failure, and a user-facing surface relaying the message shows text AWS never emits.
Fix: the message constructed in PutTableBucketPolicy's resource-policy validation (where the malformed-document refusal is raised) must be AWS's fixed string for this error class; the status and BadRequestException classification are already correct.
Traces: s3tables-table-bucket-config-error-paths#task#9 (representative), s3tables-table-bucket-configuration-error-paths#task#4, s3tables-table-bucket-policy-malformed#task#2 — 3 steps in the same cluster.

## Evidence
Cause 1 — CreateTable, expected status 200, actual 200; divergence key service=s3tables|CreateTable|status=200->200|keys=$.versionToken. Replay detail: "$.versionToken: value_mismatch (expected '<shaped-not-asserted>', got 'e4276912-458a-445c-8834-03b86a20f04d')". 8 affected steps (s3tables-rename-table-across-namespaces#setup#4, s3tables-s3t-uppercase-and-hyphenated-table-names-rejected#task#6, s3tables-sct-create-table-input-mistakes#task#7, #task#8, s3tables-sct-namespace-string-and-delete-while-in-use#task#4, s3tables-sct-sort-order-at-create-via-write-order#task#5, #task#8, s3tables-table-replication#setup#5). The same generator shows up on GetTable: "$.versionToken: value_mismatch (expected '<shaped-not-asserted>', got '76d065b5-b91f-44eb-ab4e-d8cf74c9d9f7')" (s3tables-s3t-get-table-with-both-the-arn-and-the-triple-rejected#task#6, owned by 20261103T033559Z-s3tables-list-and-get-responses-emit-epoch-timestamps.md). The literal UUIDs are per-run values; the asserted token form is the requirement.

Cause 2 — PutTableBucketPolicy, expected status 400, actual 400; divergence key service=s3tables|PutTableBucketPolicy|status=400->400|keys=$.message. Replay detail: "$.message: value_mismatch (expected 'The specified policy is malformed', got 'The specified resource policy is not valid.')". Expected message text (all fixed, no interpolated values): The specified policy is malformed. Actual: The specified resource policy is not valid. 3 affected steps: s3tables-table-bucket-config-error-paths#task#9, s3tables-table-bucket-configuration-error-paths#task#4, s3tables-table-bucket-policy-malformed#task#2.

## Steps to Reproduce
Cause 1: replay fixture s3tables-rename-table-across-namespaces step setup#4 (CreateTable in an existing namespace) and read $.versionToken off the 200 response — it is a hyphenated UUID rather than a token of the recorded form. Repeat on any of the other 7 CreateTable steps listed in Traces (e.g. s3tables-sct-create-table-input-mistakes#task#7), and on s3tables-s3t-get-table-with-both-the-arn-and-the-triple-rejected#task#6 to see the same generator in the GetTable read-back.

Cause 2: replay fixture s3tables-table-bucket-config-error-paths step task#9 — PutTableBucketPolicy on an existing table bucket with a resourcePolicy document that is not a well-formed policy. The response is 400, but $.message reads 'The specified resource policy is not valid.' instead of 'The specified policy is malformed'. Same on s3tables-table-bucket-configuration-error-paths#task#4 and s3tables-table-bucket-policy-malformed#task#2.
