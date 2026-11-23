Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-byvpc-traj-batched-ids-hit-per-call-limits
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-byvpc-traj-batched-ids-hit-per-call-limits ec2-page-describe-instances-tag-filtered-two-pages ec2-dvpc-traj-truncated-ids-not-found-and-filter-empty ec2-ecc-traj-instance-reference-mistakes ec2-nat-gateway-route-server-and-security-group-vpc-errors
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstances|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|AssociateSecurityGroupVpc|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|AssociateSecurityGroupVpc|4xx->4xx|$.Response.Errors.Error.Message
Issue-Related: 20261103T071133Z-ec2-pagination-and-filter-validation-refusals-use-emulator.md
Issue-Commit: 94f7b27553b727586aac9de0add3c17de28dbaa9

# ec2 describe and associate refusals carry emulator-authored codes and messages instead of EC2's

> _Note: this issue was identified against the code base at commit `94f7b27553b727586aac9de0add3c17de28dbaa9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstances

## Description
Three net-new causes. They share one fault: the emulator's refusal layer re-words EC2's error strings and, in two of the three, picks a different error code class, so the HTTP status is right but the contract-bearing code/message are not. EC2's Smithy model binds no errors to these operations (smithy_query operation=DescribeInstances/AssociateSecurityGroupVpc -> "errors": []), so the code and message authority below is "recorded behavior only".

Cause 1 of 3 — DescribeInstances with more than 200 values on one filter (fixture ec2-byvpc-traj-batched-ids-hit-per-call-limits, step task#1).
AWS: recorded behavior only — HTTP 400, Code FilterLimitExceeded, Message exactly "The maximum number of filter values specified on a single call is 200". The whole sentence is fixed text; the limit 200 is the documented per-call filter-value ceiling, not a value taken from the request.
Emulator: HTTP 400 but Code InvalidParameterValue and a generated message "The filter '<filter-name>' carries <n> values, above the maximum of 200" (recorded instance: filter 'vpc-id', 201 values).
Why: FilterLimitExceeded is the code a client branches on to split a batch into smaller calls; seeing InvalidParameterValue instead, it treats a chunkable request as permanently invalid and gives up.
Fix: the DescribeInstances filter validation (shared EC2 filter-limit check) — raise FilterLimitExceeded with AWS's fixed sentence.

Cause 2 of 3 — DescribeInstances naming several nonexistent instance ids in one call (fixture ec2-dvpc-traj-truncated-ids-not-found-and-filter-empty, step task#2).
AWS: recorded behavior only — HTTP 400, Code InvalidInstanceID.NotFound, and the message names EVERY id that was not found, in request order, comma-separated, with the plural noun: "The instance IDs '<id1>, <id2>' do not exist". Fixed text is "The instance IDs '", "' do not exist" (singular form "The instance ID '<id>' does not exist" when exactly one id is missing); the id list is taken from the request.
Emulator: reports only the first missing id and keeps the singular form — "The instance ID '<id1>' does not exist".
Why: the caller is told one id is bad, retries with the rest, and fails again on the next one — one round trip per bad id instead of one, and tooling that parses the id list out of the message repairs the request incorrectly.
Fix: the DescribeInstances id-resolution error path — collect all unresolved ids and pluralize the sentence.

Cause 3 of 3 — AssociateSecurityGroupVpc with a VpcId that fails EC2's id-shape check (fixture ec2-nat-gateway-route-server-and-security-group-vpc-errors, step task#5).
AWS: recorded behavior only — HTTP 400, Code InvalidVpcId.Malformed, Message "The vpc ID '<vpc-id>' is malformed"; fixed text is "The vpc ID '" + "' is malformed" with the id echoed from the request.
Emulator: HTTP 400 with Code InvalidVpcID.NotFound and Message "The vpc ID '<vpc-id>' does not exist" — it skips the shape check and reports a lookup miss.
Why: malformed-vs-nonexistent is a different failure class: a caller that retries NotFound after creating the VPC loops forever on an id that can never be valid, and the code casing differs too (InvalidVpcId vs InvalidVpcID).
Fix: AssociateSecurityGroupVpc's input validation — run the vpc-id shape check before state lookup, as the other EC2 id validators do.

Related open issues that do NOT carry these replay keys: 20261120T081949Z-ec2-refusal-fidelity-malformed-vs-nonexistent-id-classes.md and 20261121T222634Z-ec2-associatedhcpoptions-answers-invaliddhcpoptionid-notfound-for-an-id.md.

## Evidence
Cause 1 — expected $.Response.Errors.Error.Code 'FilterLimitExceeded' and Message 'The maximum number of filter values specified on a single call is 200'; actual Code 'InvalidParameterValue', Message "The filter 'vpc-id' carries 201 values, above the maximum of 200" (status 400 both). Cluster also covers ec2-page-describe-instances-tag-filtered-two-pages#task#16.
Cause 2 — expected Message "The instance IDs 'i-12345678, i-87654321' do not exist"; actual "The instance ID 'i-12345678' does not exist" (status 400 both). Cluster also covers ec2-ecc-traj-instance-reference-mistakes#task#1.
Cause 3 — expected Code 'InvalidVpcId.Malformed' / Message "The vpc ID 'vpc-0123456789abcdef0' is malformed"; actual Code 'InvalidVpcID.NotFound' / Message "The vpc ID 'vpc-0123456789abcdef0' does not exist" (status 400 both).

## Steps to Reproduce
Replay trace set simparity-wire-aws-v1 and inspect ec2-byvpc-traj-batched-ids-hit-per-call-limits task#1, ec2-dvpc-traj-truncated-ids-not-found-and-filter-empty task#2, and ec2-nat-gateway-route-server-and-security-group-vpc-errors task#5. Equivalently: DescribeInstances with 201 values on Filter.1 (Name=vpc-id); DescribeInstances with two nonexistent instance ids; AssociateSecurityGroupVpc with a GroupId plus a VpcId whose id body fails EC2's 8/17-hex-char shape check.
