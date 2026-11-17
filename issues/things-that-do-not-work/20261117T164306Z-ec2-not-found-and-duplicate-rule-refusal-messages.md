Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-create-tags-delete-tags-key-pair-existence
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-create-tags-delete-tags-key-pair-existence ec2-traffic-mirror-filter-lifecycle
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|CreateTags|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|CreateTrafficMirrorFilterRule|4xx->4xx|$.Response.Errors.Error.Message
Issue-Commit: 81e21ca007588719de802b4dd67d83498ccb7823

# ec2 not-found and duplicate-rule refusal messages are reworded by the emulator

> _Note: this issue was identified against the code base at commit `81e21ca007588719de802b4dd67d83498ccb7823`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTags

## Description
2 causes: two EC2 refusals return AWS's status and error code but an emulator-authored message, so a client that reads the message text sees wording AWS never produces.

Cause 1 of 2: CreateTags for a nonexistent key pair id
AWS: recorded behavior (the model declares no errors for CreateTags, so the refusal class comes from the recording). Recorded refusal: HTTP 400, error code InvalidKeyPair.NotFound-class ec2Query error whose Message reads `The key-pair ID 'key-0000000000000008d' does not exist`. Fixed text: `The key-pair ID '` ... `' does not exist` - note the hyphen in "key-pair". Variable part: the key-pair id taken verbatim from the request's ResourceId.
Emulator: HTTP 400 with the matching code but Message `The key pair ID 'key-0000000000000008d' does not exist` - "key pair" without the hyphen.
Why: a client (or test harness) that matches AWS's documented message text to distinguish a missing key pair from another tagging failure does not match, and the difference is invisible in the code alone.
Fix: the CreateTags handler's resource-existence validation in the EC2 crate - use AWS's message template for the key-pair resource type.
Traces: ec2-create-tags-delete-tags-key-pair-existence#task#5

Cause 2 of 2: CreateTrafficMirrorFilterRule with a rule number already in use
AWS: Smithy: CreateTrafficMirrorFilterRule requires TrafficMirrorFilterId, TrafficDirection, RuleNumber, RuleAction, DestinationCidrBlock, SourceCidrBlock; the operation models no errors, so the refusal class comes from the recording. Recorded refusal: HTTP 400 with Message `Rule number 100 is already in use`. Fixed text: `Rule number ` ... ` is already in use`. Variable part: the RuleNumber from the request.
Emulator: HTTP 400 with the matching code but Message `The traffic mirror filter rule identified by 100 already exists.` - entirely emulator-authored prose, including the trailing period.
Why: the duplicate-rule-number condition cannot be recognised from the message a client is given, and tooling that surfaces the message to an operator shows text AWS never emits.
Fix: the CreateTrafficMirrorFilterRule handler's duplicate-rule-number validation in the EC2 crate.
Traces: ec2-traffic-mirror-filter-lifecycle#task#7

Related open issues that do NOT cover these two steps (the replay reports no owner for them): 20261117T065015Z-ec2-refusal-error-codes-and-messages-for-malformed.md and 20261116T190237Z-ec2-error-responses-use-wrong-error-codes-and.md.

## Evidence
Replay of trace set simparity-wire-aws-v1 (service ec2), verdict=mismatch, expected_status 400 / actual_status 400:
- CreateTags, fixture ec2-create-tags-delete-tags-key-pair-existence step task#5: $.Response.Errors.Error.Message: value_mismatch (expected "The key-pair ID 'key-0000000000000008d' does not exist", got "The key pair ID 'key-0000000000000008d' does not exist")
- CreateTrafficMirrorFilterRule, fixture ec2-traffic-mirror-filter-lifecycle step task#7: $.Response.Errors.Error.Message: value_mismatch (expected 'Rule number 100 is already in use', got 'The traffic mirror filter rule identified by 100 already exists.')
(The identifier and rule number above are this recording's values filling the template, not part of the requirement.)

## Steps to Reproduce
Cause 1:
1. CreateTags with ResourceId set to a well-formed but nonexistent key pair id (key-...) and any tag.
2. Read Response.Errors.Error.Message -> emulator says "The key pair ID '<id>' does not exist"; AWS says "The key-pair ID '<id>' does not exist".
Cause 2:
1. CreateTrafficMirrorFilter, then CreateTrafficMirrorFilterRule with RuleNumber=100.
2. Repeat the same CreateTrafficMirrorFilterRule call with RuleNumber=100.
3. Read Response.Errors.Error.Message -> emulator says "The traffic mirror filter rule identified by 100 already exists."; AWS says "Rule number 100 is already in use".
Or replay the fixture steps listed under Traces.
