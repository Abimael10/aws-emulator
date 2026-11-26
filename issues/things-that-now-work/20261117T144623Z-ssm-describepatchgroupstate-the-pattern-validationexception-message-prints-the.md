Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.describe-patch-group-state-rejects-disallowed-characters
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.describe-patch-group-state-rejects-disallowed-characters|run#8|AssertionError|assert message == (
Issue-Commit: 6df92bd208b59385c9370bd54f69012d5d2b8998

# ssm DescribePatchGroupState: the @pattern ValidationException message prints the PatchGroup regex without its ^...$ anchors

> _Note: this issue was identified against the code base at commit `6df92bd208b59385c9370bd54f69012d5d2b8998`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribePatchGroupState

## Description
One net-new root cause in SSM's constraint-violation message rendering.

=== Cause 1 of 1: the pattern quoted in the ValidationException message drops the anchors AWS prints ===

AWS: Smithy com.amazonaws.ssm#DescribePatchGroupState input member PatchGroup (required, target ssm#PatchGroup, @length min 1 max 256) carries @pattern "^([\\p{L}\\p{Z}\\p{N}_.:/=+\\-@]*)$". A value containing a character outside that set is refused with HTTP 400, Error.Code "ValidationException", and AWS's standard constraint message, whose fixed text is "1 validation error detected: Value '<value>' at '<memberName>' failed to satisfy constraint: Member must satisfy regular expression pattern: <pattern>" — the variable parts being the rejected value from the request, the wire member name (lowerCamel 'patchGroup'), and the member's modeled pattern reproduced verbatim, i.e. including its leading ^ and trailing $. The emulator already gets the status, the code and the member name right; only the pattern text diverges.

Emulator: the message quotes the pattern with the anchors stripped — "... Member must satisfy regular expression pattern: ([\\p{L}\\p{Z}\\p{N}_.:/=+\\-@]*)" instead of "... Member must satisfy regular expression pattern: ^([\\p{L}\\p{Z}\\p{N}_.:/=+\\-@]*)$". Everything else in the sentence matches.

Why: the quoted pattern is the actionable part of the message — it is what a client, a CLI user or a log-scraping test uses to learn which values are acceptable. Unanchored, it reads as "the value must CONTAIN a run of allowed characters", which the rejected value satisfies, so the message contradicts the refusal it accompanies; message-equality assertions against AWS's text fail even though the error class is right. Since the emulator strips the anchors while rendering a modeled @pattern, every operation whose constraint message is produced the same way will diverge the same way.

Fix: the ssm service's constraint-violation message rendering for @pattern members (the validation layer that formats "Member must satisfy regular expression pattern" for DescribePatchGroupState's PatchGroup, shared by the service's other @pattern members) must emit the modeled pattern string verbatim, anchors included, rather than a stripped or re-derived form.

Traces: ssm.describe-patch-group-state-rejects-disallowed-characters.

## Evidence
Failing check ssm.describe-patch-group-state-rejects-disallowed-characters [mismatch]:

  File "check_ssm_describe_patch_group_state_rejects_disallowed_characters.py", line 38, in run
    assert message == (
  AssertionError: 1 validation error detected: Value 'sim2real-patchgroup-6b822bb72bfd#bad' at 'patchGroup' failed to satisfy constraint: Member must satisfy regular expression pattern: ([\p{L}\p{Z}\p{N}_.:/=+\-@]*)

The control leg passes first: ssm.describe_patch_group_state(PatchGroup=<clean name>) answers 200 with Instances == 0, and the bad leg is correctly refused with Error.Code "ValidationException". The assertion that fails is on Error.Message only.

Expected (AWS), with <bad> = the request's PatchGroup value:
  "1 validation error detected: Value '<bad>' at 'patchGroup' failed to satisfy constraint: Member must satisfy regular expression pattern: ^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$"
Actual (emulator):
  "1 validation error detected: Value '<bad>' at 'patchGroup' failed to satisfy constraint: Member must satisfy regular expression pattern: ([\p{L}\p{Z}\p{N}_.:/=+\-@]*)"

Fixed text: everything up to and including "Member must satisfy regular expression pattern: ". Variable parts: the quoted value (taken from the request), the member name 'patchGroup', and the pattern (taken from the model — ^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$). The suffixed "#bad" and the random name in the quoted value above are this run's values, not part of the requirement; the missing ^ and $ are.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite so the emulator is serving.
2. Control: ssm.describe_patch_group_state(PatchGroup="sim2real-patchgroup-<suffix>") -> HTTP 200 with Instances 0 (passes today).
3. Negative: ssm.describe_patch_group_state(PatchGroup="sim2real-patchgroup-<suffix>#bad") — '#' is outside the modeled @pattern.
4. Read Error.Code and Error.Message from the ClientError. Code is "ValidationException" (correct). Message ends "... regular expression pattern: ([\p{L}\p{Z}\p{N}_.:/=+\-@]*)" in the emulator, but "... regular expression pattern: ^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$" on AWS.
