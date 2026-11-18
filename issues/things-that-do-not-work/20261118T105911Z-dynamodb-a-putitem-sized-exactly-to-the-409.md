Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.number-byte-cost-trailing-zero-trim-depends-on-fraction-presence
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|PutItem|ValidationException
Issue-Commit: 2d1d2911b5095265487653cd392f1f2d36a8c5ca

# dynamodb: a PutItem sized exactly to the 409,600-byte ceiling under a Number's trailing-zero trim is refused — the N width now overcharges a bare integer

> _Note: this issue was identified against the code base at commit `2d1d2911b5095265487653cd392f1f2d36a8c5ca`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutItem

## Description
REOPENS 20261117T172826Z-dynamodb-sizing-and-capacity-7-net-new-causes.md: its fix left the call refused.

One cause. Cause 1 of 1: the byte width charged for a Number attribute at the 409,600-byte item ceiling overcharges the bare-integer form, so a PutItem AWS stores is refused.
AWS: Smithy: PutItem takes a required Item (PutItemInputAttributeMap) and the model carries no size rule, so this is the documented item-size rule plus the recorded response. An item may not exceed 409,600 bytes, measured over attribute names and values as the store holds them, and an N's contribution follows the digits the value is SPELLED with: a bare integer's trailing zeros trim away ('100' costs 2 bytes, '1010' costs 3, '12' and '15' cost 2), but once the literal also carries a fraction the integer part KEEPS its trailing zeros ('100.5' costs 4). An item built to exactly 409,600 bytes under that cost is stored and reads back whole, including the N round-tripping as the same literal; one byte over is refused HTTP 400 ValidationException "Item size has exceeded the maximum allowed size" (fixed text, no request values) and nothing is persisted.
Emulator: the at-the-limit PutItem for the first form ('100', cost 2) is refused — 'An error occurred (ValidationException) when calling the PutItem operation: Item size has exceeded the maximum allowed size' — so the emulator charges at least one byte more than the trimmed width for that form and the ceiling lands below 409,600 bytes.
Why: a client that sizes writes against the documented item limit has valid writes rejected with a 400 it cannot explain from its own request, and the failure depends on the decimal form of an unrelated attribute. It is the inverse of the sibling boundary check that still STORES an item AWS refuses, so no single digit-counting rule currently satisfies both forms.
Fix: the Number width function in the item-size accounting (the shared N-sizing used by PutItem's size gate, and by the 1 KB write unit, the 4 KB read unit and the index-entry sizes that read from it) — the trim must depend on whether the literal carries a fraction, not on the digit run alone.
Traces: dynamodb.number-byte-cost-trailing-zero-trim-depends-on-fraction-presence. The open issue 20261118T031500Z-dynamodb-a-secondary-index-table-never-reaches-active-and-updateitem-clause-overhead.md states this pairing as its cause 3, but the check's recorded replay key no longer matches this failure (it now raises inside the at-limit PutItem rather than at the over-limit assertion), so the failure is re-grounded here.

## Evidence
Expected vs actual, verbatim from this replay:
- Actual: 'botocore.exceptions.ClientError: An error occurred (ValidationException) when calling the PutItem operation: Item size has exceeded the maximum allowed size', raised on the FIRST at-the-limit PutItem — the item built to exactly 409,600 bytes with n={'N': '100'} (byte cost 2) plus a pk and a filler string.
- Expected: HTTP 200; the item reads back whole with ConsistentRead, n round-trips as {'N': '100'}, and only the item built to 409,601 bytes under the same cost is refused with ValidationException 'Item size has exceeded the maximum allowed size', leaving nothing stored.
- The same expectation holds for '1010' (cost 3), '12' (2), '15' (2) and '100.5' (4). The literals and the 409,600/409,601 pair are the rule, not one run's figures; the filler length is derived from the cost.

## Steps to Reproduce
1. Build and boot the emulator, then replay the behavioral suite (run_behavioral_suite).
2. Re-run the check id in trace_ids.
3. CreateTable with a single S hash key, PAY_PER_REQUEST, and wait for ACTIVE.
4. PutItem an item of {'pk': {'S': 'at-0'}, 'n': {'N': '100'}, 'f': {'S': 'x' * filler}} where filler makes len('pk') + len('at-0') + len('n') + 2 + len('f') + filler exactly 409,600 — AWS stores it; the emulator refuses it with ValidationException 'Item size has exceeded the maximum allowed size'.
5. Repeat with the same total at 409,601 bytes — both AWS and the emulator must refuse, and GetItem must show nothing persisted.
6. Repeat steps 4-5 for '1010' (cost 3), '12' (2), '15' (2) and '100.5' (4) to pin the trim's dependence on the presence of a fraction.
