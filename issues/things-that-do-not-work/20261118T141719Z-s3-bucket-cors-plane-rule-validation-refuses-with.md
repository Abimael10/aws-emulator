Issue-Source: trace-sweep-divergence
Issue-Trace: s3-cors-rule-count-and-id-length-limits
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: s3-cors-rule-count-and-id-length-limits s3-cors-rule-wildcard-and-method-refusals
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3|PutBucketCors|4xx->4xx|$.Error.Message
Issue-Key: s3|PutBucketCors|4xx->2xx|$
Issue-Key: s3|PutBucketCors|4xx->2xx|status
Issue-Key: s3|GetBucketCors|2xx->2xx|$.CORSConfiguration.CORSRule.AllowedHeader
Issue-Key: s3|GetBucketCors|2xx->2xx|$.CORSConfiguration.CORSRule.AllowedOrigin
Issue-Key: s3|GetBucketCors|2xx->2xx|$.CORSConfiguration.CORSRule.ExposeHeader
Issue-Commit: 35e32963f89bfa2c86c6d7f7351ac2825588f64a

# s3 bucket CORS plane: rule validation refuses with the wrong wording, accepts invalid wildcard/method rules, and GetBucketCors does not return the accepted configuration

> _Note: this issue was identified against the code base at commit `35e32963f89bfa2c86c6d7f7351ac2825588f64a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketCors

## Description
Three net-new causes in the bucket CORS plane, found by replaying the wire corpus (trace set simparity-wire-aws-v1). The CORS configuration validator refuses the right requests with its own sentences, does not refuse malformed rules at all, and the configuration that is accepted is not the configuration GetBucketCors subsequently returns.

Cause 1 of 3: PutBucketCors over the 100-rule limit is refused with an emulator-authored message.
AWS: a CORSConfiguration carrying more than 100 CORSRule elements is refused 400 with error code InvalidRequest and the FIXED message 'The number of CORS rules should not exceed allowed limit of 100 rules.' — the whole sentence is fixed text; neither the submitted rule count nor the member path appears in it, and 100 is the documented limit rather than a per-request value. Smithy (com.amazonaws.s3#AmazonS3): PutBucketCors takes CORSConfiguration as the required httpPayload; the 100-rule cap is not modelled, so the limit and wording are recorded behavior.
Emulator: status and code are right (400 / InvalidRequest) but the message is the emulator's own sentence, built from the member path and the submitted count: 'CORSConfiguration.CORSRules carries 101 rules, more than the 100 rules one cors configuration may hold' (101 is this run's rule count, i.e. the emulator interpolates a request value into text AWS keeps fixed).
Why: a client or console that matches on AWS's fixed sentence to tell 'too many CORS rules' apart from any other InvalidRequest never matches, and the member path the emulator leaks is not part of the S3 wire contract.
Fix: PutBucketCors' CORS-rule-count validation and the message it raises.
Traces: s3-cors-rule-count-and-id-length-limits#task#2 (cluster also covers s3-cors-rule-count-and-id-length-limits#task#3, s3-cors-rule-wildcard-and-method-refusals#task#6, s3-cors-rule-wildcard-and-method-refusals#task#7 — the same refusal-wording defect on the rule-count and ID-length paths).

Cause 2 of 3: PutBucketCors accepts rules AWS refuses — multi-wildcard AllowedOrigin and the other malformed wildcard/method rules are stored instead of rejected.
AWS: an AllowedOrigin containing more than one '*' is refused 400 with code InvalidRequest and the message 'AllowedOrigin "<origin>" can not have more than one wildcard.' — fixed text apart from the offending origin value, which is echoed verbatim from the request; the companion refusals in the same recording reject the other malformed wildcard/method rules the same way. Smithy models CORSRule.AllowedOrigins and AllowedMethods as required lists but states the valid method set only in prose ('Valid values are GET, PUT, HEAD, POST, and DELETE'), so the wildcard rule itself is recorded behavior only.
Emulator: answers 200 with an empty body and stores the rule.
Why: a client that relies on S3 to validate a CORS document gets a bucket whose CORS configuration real S3 would never hold, so the browser-preflight behavior it then provisions against cannot be reproduced, and the error path it handles is never exercised.
Fix: PutBucketCors' per-rule validation (AllowedOrigin wildcard count, AllowedMethod value set) before the configuration is written to bucket state.
Traces: s3-cors-rule-wildcard-and-method-refusals#task#3 (cluster also covers #task#4 and #task#5 of the same fixture).

Cause 3 of 3: GetBucketCors returns a different rule from the one PutBucketCors last accepted.
AWS: GetBucketCors (output xmlName CORSConfiguration, CORSRules serialized flattened as CORSRule) returns exactly the rule set last accepted — AllowedOrigin, AllowedHeader, ExposeHeader, AllowedMethod and MaxAgeSeconds all round-trip unchanged. Smithy: GetBucketCorsOutput.CORSRules -> CORSRule (xmlFlattened), members ID/AllowedHeaders/AllowedMethods/AllowedOrigins/ExposeHeaders/MaxAgeSeconds.
Emulator: serves a stale/incorrect rule — AllowedOrigin comes back as the earlier value instead of the accepted one, ExposeHeader comes back as '*' instead of the accepted header name, and AllowedHeader is omitted entirely. Concretely in the recording: expected AllowedOrigin 'https://*.example.com', got 'https://app.example.com'; expected ExposeHeader 'ETag', got '*'; expected AllowedHeader 'x-amz-*', got nothing. (Those strings are the recording's values; the requirement is that whatever was accepted is what is returned, including the optional AllowedHeaders/ExposeHeaders members.)
Why: a client that writes a CORS configuration and reads it back to confirm it sees a configuration it never sent; any read-modify-write of the CORS document silently drops AllowedHeaders.
Fix: the bucket CORS state writer (PutBucketCors must replace the stored rule set) and the GetBucketCors serializer (AllowedHeaders/ExposeHeaders must be emitted as stored).
Traces: s3-cors-rule-wildcard-and-method-refusals#task#8.

Related but not covering these: the open behavioral issue '20261117T143723Z-s3-7-net-new-refusal-wording-and-refusal.md' names the >100-CORS-rules wording as one of its behavioral checks, but it carries no replay key for these wire steps, so cause 1 is filed here with its fixture binding.

## Evidence
Cause 1 — PutBucketCors, 101 rules. Expected 400 InvalidRequest, message 'The number of CORS rules should not exceed allowed limit of 100 rules.'; actual 400 InvalidRequest, message 'CORSConfiguration.CORSRules carries 101 rules, more than the 100 rules one cors configuration may hold'. Fixed text = the whole expected sentence; the emulator's variable parts (member path 'CORSConfiguration.CORSRules', count '101') belong to no AWS message.
Cause 2 — PutBucketCors with AllowedOrigin 'https://*.*.example.com'. Expected status 400 with body {'Error': {'Code': 'InvalidRequest', 'Message': 'AllowedOrigin "https://*.*.example.com" can not have more than one wildcard.', 'RequestId': <per-run>, 'HostId': <per-run>}}; actual status 200 with an empty body. Fixed text = 'AllowedOrigin "' ... '" can not have more than one wildcard.'; the quoted origin is taken from the request.
Cause 3 — GetBucketCors after the accepted PUT. $.CORSConfiguration.CORSRule.AllowedOrigin expected 'https://*.example.com', got 'https://app.example.com'; $.CORSConfiguration.CORSRule.ExposeHeader expected 'ETag', got '*'; $.CORSConfiguration.CORSRule.AllowedHeader expected 'x-amz-*', got None (member absent).

## Steps to Reproduce
Cause 1: create a bucket, PutBucketCors with 101 CORSRule elements; inspect the Error.Message.
Cause 2: PutBucketCors with a single rule whose AllowedOrigin contains two wildcards (e.g. 'https://*.*.example.com'), then the variants with an unsupported AllowedMethod; each must be refused 400 InvalidRequest, but the emulator answers 200.
Cause 3: PutBucketCors with a rule carrying AllowedOrigin 'https://*.example.com', AllowedHeader 'x-amz-*' and ExposeHeader 'ETag', then GetBucketCors and compare the returned rule with the one sent.
Full replay: replay fixtures s3-cors-rule-count-and-id-length-limits (task#2, task#3) and s3-cors-rule-wildcard-and-method-refusals (task#3-#8).
