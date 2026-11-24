Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 864dd26a02416cc990250070e65c7b703d06755e

# StartSnapshot accepts Encrypted together with ParentSnapshotId whenever Encrypted is false

_Filed: 2026-11-19T10:21:54Z_
_Operation: StartSnapshot | Class: validation_

> _Note: this issue was identified against the code base at commit `864dd26a02416cc990250070e65c7b703d06755e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`StartSnapshotRequest$Encrypted` documents that the two members cannot be specified together and that specifying both fails with `ValidationException`. The handler refuses the combination only when `Encrypted` carries the value `true`: it tests `input.encrypted == Some(true)`, so a request that specifies `"Encrypted": false` ALONGSIDE `ParentSnapshotId` — both parameters specified — is accepted and a snapshot is minted. `Option<bool>` makes the distinction available to the handler (`Some(false)` is a specified `false`, `None` is an absent member), so the handler has the information the documented rule needs and discards it.

The same conflation is carried into the idempotency fingerprint, so the two cases are indistinguishable there as well.

## Evidence

Smithy, `StartSnapshotRequest$Encrypted` (smithy_query doc, verbatim): "Indicates whether to encrypt the snapshot. You can't specify **Encrypted** and **ParentSnapshotId** in the same request. If you specify both parameters, the request fails with `ValidationException`."

services/service-com-amazonaws-ebs-ebs/src/ops/start_snapshot.rs:263-272 quotes that very sentence and then implements a narrower rule:
"        // `Encrypted`: \"You can't specify Encrypted and ParentSnapshotId in the same\n        // request. If you specify both parameters, the request fails with\n        // ValidationException.\" ...\n        if input.encrypted == Some(true) && input.parent_snapshot_id.is_some() {\n            return Err(validation(\n                \"INVALID_PARAMETER_VALUE\",\n                PARENT_SNAPSHOT_ID_AND_ENCRYPT_MESSAGE.to_string(),\n            ));\n        }"

services/service-com-amazonaws-ebs-ebs/src/ops/start_snapshot.rs:513-516 collapses the two cases in the idempotency fingerprint too: "canonical.push_str(&format!(\n        \"Encrypted={}\\n\",\n        input.encrypted.unwrap_or(false)\n    ));"

Test coverage: tests/all/sealed_status_and_fixed_refusal_text.rs:331-448 (`the_parent_and_encryption_refusal_is_the_same_fixed_sentence_for_every_request`) exercises only `"Encrypted": true` with a parent (lines 354-392) and the two members alone (lines 426-447); no case specifies `Encrypted: false` with `ParentSnapshotId`, so no test pins the accepting behaviour.

## Steps to Reproduce

1. POST /snapshots with body {"VolumeSize": 1} and CompleteSnapshot it so a sealed parent exists.
2. POST /snapshots with body {"VolumeSize": 1, "Encrypted": false, "ParentSnapshotId": "<the sealed parent>"} — both documented parameters are specified.
3. Observe HTTP 201 with a new SnapshotId instead of the documented HTTP 400 `ValidationException` (`Reason` INVALID_PARAMETER_VALUE, `Message` "ParentSnapshotId and Encrypt cannot be specified together"), because start_snapshot.rs:267 only refuses `Some(true)`.
4. Repeat with {"Encrypted": true, "ParentSnapshotId": ...} to see the refusal the same rule produces for the other value.
