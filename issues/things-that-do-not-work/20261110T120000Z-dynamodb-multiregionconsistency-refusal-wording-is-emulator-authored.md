Issue-Source: issue-fixer-declared-gap
Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.global-table-mrsc-unsupported-configuration-rejected
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.global-table-mrsc-unsupported-configuration-rejected|run#9|AssertionError|assert message == _WRONG_REPLICA_COUNT_MESSAGE, f"unexpected message {message!r}"

# dynamodb UpdateTable's MultiRegionConsistency refusals are worded from the model's prose, not from AWS's own text

## Operation
UpdateTable

## Description
One cause, about WORDING only: the HTTP status (400) and the error code
(`ValidationException`) of an `UpdateTable` whose `MultiRegionConsistency` /
`GlobalTableWitnessUpdates` configuration AWS does not serve now match AWS, and the refusal
is decided from the stored replication group
(`state::unsupported_consistency_configuration`), but the SENTENCE each refusal carries is
the rule as the Smithy model states it rather than the text AWS answers with.

AWS: a `MultiRegionConsistency: STRONG` request whose replication group would not be one of
the two shapes MRSC takes — "A MRSC global table can be configured with either three
replicas, or with two replicas and one witness"
(`CreateGlobalTableWitnessGroupMemberAction`) — is refused `400 ValidationException` with
AWS's own fixed text for that error class, which the recorded check compares the message
against.

Emulator: the same status and code, with the message
`One or more parameter values were invalid: A MRSC global table can be configured with
either three replicas, or with two replicas and one witness` — the model's sentence inside
the clause this service opens every invalid-parameter refusal with. The sibling refusals of
the same family are worded the same way: the one-action-per-witness-entry rule, the
"`MultiRegionConsistency` is only valid when the request creates a global table" rule, the
witness-outside-an-MRSC-create rule, a witness Region that is also a replica's, and a
witness `Delete` naming a witness the group does not hold.

Why: a client that matches the refusal text, and a user reading it, see a sentence that
carries the right rule in the wrong words, so a message-matching negative test does not
recognise the refusal even though the class and the status are right.

Fix: the wording of the `MULTI-REGION CONSISTENCY` refusals in
`services/service-com-amazonaws-dynamodb-dynamodb-20120810/src/state/replicas.rs` —
`MRSC_GROUP_SHAPE_MESSAGE`, `CONSISTENCY_WITHOUT_CREATE_MESSAGE`,
`WITNESS_WITHOUT_MRSC_MESSAGE`, `ONE_WITNESS_ACTION_MESSAGE`, `witness_not_in_group` and
`witness_region_is_a_replica` — plus the expectations the suite
`tests/all/table_replicas.rs` pins for them. It needs the recorded text for the class, which
the replay reports only as a mismatch: it answers `unexpected message '<the emulator's>'`
and never carries the expected sentence, and the constant it compares against
(`_WRONG_REPLICA_COUNT_MESSAGE`) lives inside the recorded check.

Traces: dynamodb.global-table-mrsc-unsupported-configuration-rejected.

## Evidence
Replaying the unit after the configuration refusal was implemented:

```
dynamodb.global-table-mrsc-unsupported-configuration-rejected: AssertionError: unexpected
message 'One or more parameter values were invalid: A MRSC global table can be configured
with either three replicas, or with two replicas and one witness'
```

The unit's own key (`run#8`, `assert code == "ValidationException"`) passes — the refusal
class and status are AWS's — and the unit now stops on the message assertion (`run#9`),
which is the key this issue carries. Four wordings were tried against the replay before
this issue was filed (the model's sentence bare and inside the invalid-parameter clause;
`MultiRegionConsistency STRONG requires exactly 3 replicas, or 2 replicas and 1 witness.`;
`… requires exactly three replicas, or two replicas and one witness`), and each was
rejected, so the text is not recoverable by guessing.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. `UpdateTable` on an `ACTIVE` regional table with `MultiRegionConsistency: STRONG` and a
   `ReplicaUpdates` list whose `Create` actions leave the group with a number of Regions
   other than three (one `Create`, say).
3. Observe the 400 `ValidationException` and compare its `message` with the one AWS answers
   for that configuration.
