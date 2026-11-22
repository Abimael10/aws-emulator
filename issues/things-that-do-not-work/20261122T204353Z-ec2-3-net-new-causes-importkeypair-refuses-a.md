Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.import-key-pair-accepts-an-rsa-3072-key-and-reports-its-fingerprint ec2.create-placement-group-partition-count-requires-partition-strategy-rejected ec2.describe-prefix-lists-unknown-id-rejected
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|ImportKeyPair|InvalidKey.Format
Issue-Key: ec2.create-placement-group-partition-count-requires-partition-strategy-rejected|run#6|AssertionError|assert message == expected_message, f"unexpected message: {message!r}"
Issue-Key: ec2.describe-prefix-lists-unknown-id-rejected|run#9|AssertionError|assert message == expected_message, f"unexpected message: {message!r}"
Issue-Commit: 02c6380a70f664dba771786c55045481408bc4cc

# ec2: 3 net-new causes — ImportKeyPair refuses a valid RSA-3072 OpenSSH key, and the CreatePlacementGroup / DescribePrefixLists refusal messages are the emulator's own wording

> _Note: this issue was identified against the code base at commit `02c6380a70f664dba771786c55045481408bc4cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ImportKeyPair

## Description
REOPENS 20261117T185234Z-ec2-10-net-new-refusal-classification-causes-createcapacityreservationfleet.md: its fix left the call refused. That issue reclassified three of these very paths (its cause 6 ImportKeyPair, cause 3 CreatePlacementGroup, cause 8 DescribePrefixLists); the error CODES it names are now correct, but ImportKeyPair still refuses a key AWS imports, and the two refusal paths still answer the emulator's own sentence instead of AWS's. Three causes, all under the `ec2` service heading. All three must be closed before submitting.

Cause 1 of 3: ImportKeyPair refuses a well-formed 3072-bit RSA public key in OpenSSH format.
AWS: recorded behavior only — Smithy (services/service-com-amazonaws-ec2-amazonec2) declares ImportKeyPair with input PublicKeyMaterial: Blob (required) and errors: [], so EC2 carries any refusal on the ec2Query envelope and the recording is the requirement. The recording imports an `ssh-rsa` OpenSSH public key whose modulus is 3072 bits and gets a 200: ImportKeyPair answers the requested KeyName, a minted KeyPairId and a KeyFingerprint that is the MD5 digest of the key's DER SubjectPublicKeyInfo rendered as colon-separated lowercase hex pairs, and DescribeKeyPairs on that name then reports the same KeyPairId and KeyFingerprint with KeyType `rsa`. The key length is the only thing that varies between this check and the sibling 2048-bit import the emulator accepts.
Emulator: the import is refused outright — `An error occurred (InvalidKey.Format) when calling the ImportKeyPair operation: Key is not in valid OpenSSH public key format`. The whole check dies on that call, so neither the fingerprint nor the KeyType projection is ever reached. The message's text is wholly fixed (it names no value from the request), and it is factually wrong: the material IS OpenSSH-encoded, it merely carries a 3072-bit modulus. Hypothesis: the key-format validator accepts only an allow-list of modulus sizes (1024/2048/4096) or hard-codes an expected blob length.
Why: a client that generates its own RSA key and imports it cannot get a 3072-bit key into the emulator at all, so every launch that must use that key pair is blocked; a caller branching on InvalidKey.Format is told its well-formed key is malformed and will never recover by re-encoding.
Fix: the ImportKeyPair handler's public-key validation (and the fingerprint derivation behind it) — accept any valid ssh-rsa OpenSSH public key regardless of modulus size, and report the MD5-of-SubjectPublicKeyInfo fingerprint with KeyType `rsa`. Named but NOT covering this failure: the open issue 20261111T133150Z-ec2-4-net-new-causes-resources-leak-across.md lists "ImportKeyPair rejects an RSA-3072 OpenSSH key" as one of its four causes, but the replay report binds this check id to no issue, so the failure is unowned.
Traces: ec2.import-key-pair-accepts-an-rsa-3072-key-and-reports-its-fingerprint.

Cause 2 of 3: CreatePlacementGroup's PartitionCount-without-partition-strategy refusal carries emulator prose.
AWS: recorded behavior only (CreatePlacementGroup declares errors: [] in Smithy; PartitionCount is an Integer member and Strategy a PlacementStrategy enum). A CreatePlacementGroup with Strategy=cluster and PartitionCount set is refused with code `InvalidParameterValue` and the message `partition-count is only available with the 'partition' strategy.` — that sentence is entirely fixed text: it names neither the submitted count nor the submitted strategy. The identical PartitionCount on Strategy=partition is accepted, and the refused call creates nothing (DescribePrefixLists-style effect check: no group under that name survives).
Emulator: the code is now right but the message is its own composition — `Value (3) for parameter partitionCount is invalid. PartitionCount is valid only when Strategy is set to partition.` It interpolates the submitted count (3 here is a per-run request value, not part of any template) into a sentence AWS does not use, in the generic "Value (x) for parameter y is invalid" shape.
Why: the message is the only human-readable statement of the rule a caller gets; operators and tests match on it, and a paraphrase that also leaks the submitted value cannot be matched against AWS's fixed sentence.
Fix: the CreatePlacementGroup handler's PartitionCount/Strategy cross-validation — emit AWS's fixed sentence with no request values interpolated.
Traces: ec2.create-placement-group-partition-count-requires-partition-strategy-rejected.

Cause 3 of 3: DescribePrefixLists' not-found refusal message is re-worded.
AWS: recorded behavior only (DescribePrefixLists declares errors: []; PrefixListIds targets PrefixListResourceIdStringList). A well-formed but nonexistent prefix-list id is refused with code `InvalidPrefixListId.NotFound` and the message `The Prefix List Id '<id>' does not exist` — fixed text "The Prefix List Id '" … "' does not exist" (no trailing period), with the single variable part being the id taken verbatim from the request. On the same wiring a real AWS-managed prefix-list id is still returned by its own id.
Emulator: code is now correct, message is `The prefix list (pl-00000000) does not exist.` — different fixed wording ("The prefix list" instead of "The Prefix List Id"), parentheses instead of single quotes around the id, and a trailing period AWS does not emit. The id itself is the request's value and is correctly echoed; only the template diverges.
Why: callers that surface or match the service's not-found sentence (CLI users, assertion libraries, log scrapers) see text no AWS region produces, and a reconciliation that keys on the quoted-id form cannot extract the id.
Fix: the DescribePrefixLists not-found error construction — use AWS's template and leave the request's id as the only substitution.
Traces: ec2.describe-prefix-lists-unknown-id-rejected.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout; service heading `### ec2` (957/1368 passing). All three failures are listed with no OWNED/ALREADY-FILED marker.

Cause 1 — ec2.import-key-pair-accepts-an-rsa-3072-key-and-reports-its-fingerprint (verdict: error):
  expected: ImportKeyPair(KeyName=<name>, PublicKeyMaterial=<3072-bit ssh-rsa OpenSSH key>, TagSpecifications=[key-pair]) returns 200 with KeyFingerprint == MD5(DER SubjectPublicKeyInfo) as colon-separated hex, then DescribeKeyPairs reports (KeyPairId, 'rsa', same fingerprint).
  actual:   File "client.py", line 1094, in _make_api_call -> botocore.exceptions.ClientError: An error occurred (InvalidKey.Format) when calling the ImportKeyPair operation: Key is not in valid OpenSSH public key format

Cause 2 — ec2.create-placement-group-partition-count-requires-partition-strategy-rejected (verdict: mismatch):
  the code assertion (InvalidParameterValue) now passes; the message assertion fails.
  expected: 'partition-count is only available with the \'partition\' strategy.'
  actual:   AssertionError: unexpected message: 'Value (3) for parameter partitionCount is invalid. PartitionCount is valid only when Strategy is set to partition.'
  (the 3 is the request's PartitionCount, a per-run value; the divergence is the template.)

Cause 3 — ec2.describe-prefix-lists-unknown-id-rejected (verdict: mismatch):
  the code assertion (InvalidPrefixListId.NotFound) now passes; the message assertion fails.
  expected: "The Prefix List Id 'pl-00000000' does not exist" (template: The Prefix List Id '<requested id>' does not exist)
  actual:   AssertionError: unexpected message: 'The prefix list (pl-00000000) does not exist.'

Smithy cited: services/service-com-amazonaws-ec2-amazonec2 — ImportKeyPair(input KeyName:String required, PublicKeyMaterial:Blob required; output KeyName/KeyPairId/KeyFingerprint/Tags; errors: []), CreatePlacementGroup(input GroupName:String, Strategy:PlacementStrategy, PartitionCount:Integer; errors: []), DescribePrefixLists(input PrefixListIds:PrefixListResourceIdStringList; errors: []).

## Steps to Reproduce
Boot the emulator from this checkout, then:

Cause 1: generate an RSA key with a 3072-bit modulus, encode the public half in OpenSSH format, and call ec2 ImportKeyPair(KeyName=<unique>, PublicKeyMaterial=<that blob>). Expect 200 with KeyFingerprint = MD5 of the key's DER SubjectPublicKeyInfo as colon-separated lowercase hex; then DescribeKeyPairs(KeyNames=[<name>]) must report KeyType 'rsa' and the same fingerprint. Observe InvalidKey.Format / 'Key is not in valid OpenSSH public key format'. Control: the same flow with a 2048-bit key is accepted.

Cause 2: ec2 CreatePlacementGroup(GroupName=<unique>, Strategy='cluster', PartitionCount=3). Expect Error.Code InvalidParameterValue and Error.Message exactly "partition-count is only available with the 'partition' strategy."; then DescribePlacementGroups filtered on that group-name must be empty, and the identical PartitionCount on Strategy='partition' must be accepted.

Cause 3: ec2 DescribePrefixLists() to get a real AWS-managed id, then DescribePrefixLists(PrefixListIds=['pl-00000000']). Expect Error.Code InvalidPrefixListId.NotFound and Error.Message exactly "The Prefix List Id 'pl-00000000' does not exist" (no trailing period); the real id must still be returned by its own id.

Re-replay the three check ids in trace_ids to confirm.
