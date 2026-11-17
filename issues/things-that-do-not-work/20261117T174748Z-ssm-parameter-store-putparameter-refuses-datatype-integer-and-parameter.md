Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-put-parameter-typed-data-type-validation
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-put-parameter-typed-data-type-validation ssm-parameter-empty-allowed-pattern-explicit-text-versions-by-selector-path-and-not-found
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|PutParameter|2xx->4xx|$.Tier
Issue-Key: ssm|PutParameter|2xx->4xx|$.Version
Issue-Key: ssm|PutParameter|2xx->4xx|status
Issue-Key: ssm|GetParameter|2xx->4xx|$.Parameter
Issue-Key: ssm|GetParameter|2xx->4xx|status
Issue-Key: ssm|PutParameter|4xx->4xx|$.message
Issue-Key: ssm|DeleteParameter|2xx->4xx|status
Issue-Key: ssm|GetParameter|2xx->2xx|$.Parameter.LastModifiedDate
Issue-Key: ssm|DescribeParameters|2xx->2xx|$.Parameters[*].AllowedPattern
Issue-Key: ssm|DescribeParameters|2xx->2xx|$.Parameters[*].LastModifiedDate
Issue-Key: ssm|GetParameterHistory|2xx->2xx|$.Parameters[*].LastModifiedDate
Issue-Key: ssm|GetParametersByPath|2xx->2xx|$.Parameters[*].LastModifiedDate
Issue-Key: ssm|GetParameters|2xx->2xx|$.Parameters[*].LastModifiedDate
Issue-Commit: 0d074a71eaca712c4eb2c65274b3985903118640

# Parameter Store: PutParameter refuses DataType integer and parameter reads diverge (4 causes)

> _Note: this issue was identified against the code base at commit `0d074a71eaca712c4eb2c65274b3985903118640`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutParameter

## Description
Four net-new root causes in the SSM Parameter Store plane, all on ssm, all found by replaying the wire corpus. A fixer must close ALL FOUR before submitting.

Cause 1 of 4 - PutParameter rejects a String parameter whose DataType is `integer`.
AWS: Smithy - PutParameter.DataType targets `ParameterDataType` with @length min 0 max 128 and NO enum/pattern constraint, so the model admits any data-type string of that length; the member's documentation lists text / aws:ec2:image / aws:ssm:integration but states no closed set on the wire. Recorded behavior: AWS accepts DataType `integer` and answers HTTP 200 with the PutParameterResult body {"Version": 1, "Tier": "Standard"} - it validates the VALUE against the data type (see cause 2), it does not reject the data type itself.
Emulator: answers HTTP 400 with the message "DataType 'integer' isn't supported; the supported data type values are text aws:ec2:image aws:ssm:integration" and no Version/Tier, so the parameter is never created. Every later step on that parameter then fails as a consequence: GetParameter answers 400 with no $.Parameter where AWS answers 200 with {ARN, DataType 'integer', LastModifiedDate, Name, Type 'String', Value '443', Version 1} (step task#2, also task#4 and task#7), and DeleteParameter answers 400 instead of 200 at teardown (teardown#8, teardown#9).
Why: a client that stores typed parameters (an `integer` DataType is accepted by the real service) cannot write them at all, and the whole read/update/delete lifecycle built on such a parameter collapses to 400s against an emulator that AWS answers 200 for.
Fix: PutParameter's input validation in the ssm service handler - the DataType allow-list must not be narrower than what the service accepts; keep the value-level validation and drop the data-type-level refusal.
Traces: ssm-put-parameter-typed-data-type-validation#task#1 (representative), #task#2, #teardown#8.

Cause 2 of 4 - the wrong error message when the VALUE does not match DataType integer.
AWS: recorded behavior only (the model carries no message text) - the refusal message is "The parameter value is not a valid integer. A value must be an optional minus sign followed by digits, with no leading zeros, no plus sign, no decimal point and no whitespace." That whole sentence is FIXED text; it carries no value from the request or from state. HTTP 400, and $.__type agreed with the recording.
Emulator: HTTP 400 with the data-type refusal from cause 1 instead - "DataType 'integer' isn't supported; the supported data type values are text aws:ec2:image aws:ssm:integration".
Why: the message is the only thing telling a caller WHICH input was wrong; a caller that sent a well-formed request with a malformed value is told its data type is unsupported and will stop sending the data type rather than fix the value.
Fix: PutParameter's validation in the ssm service handler - emit the value-level message for a value that does not parse as the declared data type.
Traces: ssm-put-parameter-typed-data-type-validation#task#3, #task#5.

Cause 3 of 4 - parameter-read timestamps are serialized out of shape.
AWS: Smithy - ParameterMetadata.LastModifiedDate and Parameter.LastModifiedDate target `DateTime` (@timestamp) and SSM is awsJson1_1, so they are serialized as epoch seconds. Every recorded AWS timestamp in this corpus carries at most millisecond resolution (for example 1789662568.258, 1503695717.821, 1706038480.989); the corpus asserts the SHAPE of this member rather than the instant, and the emulator's value does not satisfy it.
Emulator: emits a raw sub-millisecond float - $.Parameter.LastModifiedDate = 1794937142.8656085 (GetParameter), $.Parameters[0].LastModifiedDate = 1794937142.8656085 (DescribeParameters), $.Parameters[0..1].LastModifiedDate = 1794937142.8656085 / 1794937142.9391203 (GetParameterHistory), 1794937142.9391203 (GetParametersByPath and GetParameters). Replay verdict on each: "value_mismatch (expected '<shaped-not-asserted>', got ...)". Hypothesis: the serializer writes the clock's f64 directly instead of truncating to the millisecond resolution AWS publishes.
Why: a client that round-trips or hashes the serialized metadata, or that compares a timestamp it read back with one AWS would have produced, sees a value of a shape the real service never emits; strict decoders and recorded-response comparisons reject it.
Fix: the timestamp serializer used by the Parameter Store read handlers (GetParameter, GetParameters, GetParameterHistory, GetParametersByPath, DescribeParameters) in the ssm service - one shared normalization point, not five.
Traces: ssm-parameter-empty-allowed-pattern-explicit-text-versions-by-selector-path-and-not-found#task#2 (GetParameter, also task#12/task#13), #task#3 (DescribeParameters), #task#14 (GetParameterHistory), #task#15 (GetParametersByPath), #task#16 (GetParameters).

Cause 4 of 4 - DescribeParameters returns an empty AllowedPattern instead of omitting the member.
AWS: Smithy - ParameterMetadata.AllowedPattern targets `AllowedPattern` with @length min 0 max 1024 and is OPTIONAL. For a parameter created with no effective allowed pattern, the recorded ParameterMetadata has no AllowedPattern key at all.
Emulator: serializes $.Parameters[0].AllowedPattern = "" (replay verdict "unexpected_present (expected None, got '')").
Why: a client distinguishing "no allowed pattern" from "a pattern that matches nothing" reads the emulator as having a pattern configured, and code that compiles AllowedPattern when the key is present now compiles an empty regex.
Fix: the DescribeParameters response projection/serializer in the ssm service - omit an empty optional string member rather than emitting "".
Traces: ssm-parameter-empty-allowed-pattern-explicit-text-versions-by-selector-path-and-not-found#task#3.

No open issue owns these replay keys: the replay report listed owned_by = [] for all nine clusters. The open issues 20261111T081352Z-ssm-describeparameters-page-contents-and-ordering-diverge-under.md (DescribeParameters paging/ordering) and 20261116T133835Z-ssm-parameter-history-name-echo-and-patch-filter.md (GetParameterHistory name echo) touch the same operations but cover different causes and do not carry these steps.

## Evidence
Replay of trace set simparity-wire-aws-v1 (180 fixtures, 907 steps, score 74.3%):
- ssm-put-parameter-typed-data-type-validation#task#1 PutParameter: expected 200 with $.Version = 1 and $.Tier = 'Standard'; got 400, both members missing. Cluster also covers #task#6.
- ssm-put-parameter-typed-data-type-validation#task#2 GetParameter: expected 200 with $.Parameter = {ARN arn:aws:ssm:us-west-2:000000000000:parameter/<name>, DataType 'integer', LastModifiedDate <shaped>, Name <name>, Type 'String', Value '443', Version 1}; got 400 with no $.Parameter. Cluster also covers #task#4 and #task#7.
- ssm-put-parameter-typed-data-type-validation#task#3 PutParameter: $.message expected 'The parameter value is not a valid integer. A value must be an optional minus sign followed by digits, with no leading zeros, no plus sign, no decimal point and no whitespace.'; got "DataType 'integer' isn't supported; the supported data type values are text aws:ec2:image aws:ssm:integration". Cluster also covers #task#5.
- ssm-put-parameter-typed-data-type-validation#teardown#8 DeleteParameter: expected 200; got 400. Cluster also covers #teardown#9.
- ssm-parameter-empty-allowed-pattern-explicit-text-versions-by-selector-path-and-not-found#task#2 GetParameter: $.Parameter.LastModifiedDate value_mismatch (expected '<shaped-not-asserted>', got 1794937142.8656085).
- same fixture #task#3 DescribeParameters: $.Parameters[0].LastModifiedDate value_mismatch (got 1794937142.8656085) and $.Parameters[0].AllowedPattern unexpected_present (expected None, got '').
- same fixture #task#14 GetParameterHistory: $.Parameters[0].LastModifiedDate got 1794937142.8656085, $.Parameters[1].LastModifiedDate got 1794937142.9391203.
- same fixture #task#15 GetParametersByPath and #task#16 GetParameters: $.Parameters[0].LastModifiedDate got 1794937142.9391203.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Cause 1/2: replay fixture ssm-put-parameter-typed-data-type-validation - PutParameter {Name: <name>, Type: String, Value: '443', DataType: 'integer'} at task#1 must answer 200 {Version: 1, Tier: 'Standard'}; the follow-up GetParameter (task#2) must answer 200 with DataType 'integer', and DeleteParameter (teardown#8) must answer 200. PutParameter with DataType 'integer' and a non-integer value (task#3) must answer 400 with the value-level message quoted above.
4. Causes 3/4: replay fixture ssm-parameter-empty-allowed-pattern-explicit-text-versions-by-selector-path-and-not-found and inspect LastModifiedDate on GetParameter/GetParameters/GetParameterHistory/GetParametersByPath/DescribeParameters, and AllowedPattern on DescribeParameters (task#3).
