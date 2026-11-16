Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-page-describe-security-groups-vpc-scoped-trailing-empty-page
Issue-Step: task#12
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-page-describe-security-groups-vpc-scoped-trailing-empty-page ec2-usg-traj-malformed-vs-absent-ids-and-filters-answer-empty ec2-page-describe-subnets-vpc-scoped-two-pages ec2-page-describe-volumes-tag-filtered-two-pages
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeSecurityGroups|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|DescribeSecurityGroups|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|DescribeSubnets|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|DescribeSubnets|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|DescribeVolumes|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|DescribeVolumes|4xx->4xx|$.Response.Errors.Error.Message
Issue-Commit: 0771e25bba8212ca0e4339575fc1faa7cfb6765d

# ec2 invalid pagination token rejected with InvalidPaginationToken instead of InvalidParameterValue

> _Note: this issue was identified against the code base at commit `0771e25bba8212ca0e4339575fc1faa7cfb6765d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeSecurityGroups

## Description
Every paginated EC2 describe operation rejects an unparsable NextToken with the emulator's own error code and sentence. This bundle carries 3 net-new divergence clusters (DescribeSecurityGroups, DescribeSubnets, DescribeVolumes), none owned by an open issue; they share one fault: the common pagination-token decoder's error path.

GENERAL FAULT
AWS: a garbage NextToken on an EC2 describe call is a parameter-validation failure - 400 with Code 'InvalidParameterValue' and a message in the standard parameter form 'Value ( <supplied-token> ) for parameter NextToken is invalid. The token is invalid.' (DescribeVolumes words the same class as 'Unable to parse pagination token'). The fixed text is the frame; the only variable part is the token echoed from the request.
Emulator: all three answer Code 'InvalidPaginationToken' with 'The passed pagination token is invalid' and never echo the token.
Why: 'InvalidPaginationToken' is not an EC2 error code, so SDK/CLI error handling and tests that match InvalidParameterValue (or that surface the offending value to the operator) do not recognize the failure; a paging loop cannot tell a bad token from any other client error.
Fix: the shared pagination-token decoding/validation path used by the describe handlers (the code constant and the message template), not the serializer.

Cause 1 of 3: DescribeSecurityGroups with NextToken=' garbage-not-a-token '. AWS: Code 'InvalidParameterValue', Message 'Value ( <token> ) for parameter NextToken is invalid. The token is invalid.'. Emulator: Code 'InvalidPaginationToken', Message 'The passed pagination token is invalid'. Traces: ec2-page-describe-security-groups-vpc-scoped-trailing-empty-page#task#12 (cluster also covers ec2-usg-traj-malformed-vs-absent-ids-and-filters-answer-empty#task#4 and #task#5).

Cause 2 of 3: DescribeSubnets with the same garbage token - identical AWS code and message form, identical emulator divergence. Traces: ec2-page-describe-subnets-vpc-scoped-two-pages#task#13.

Cause 3 of 3: DescribeVolumes with the same garbage token. AWS: Code 'InvalidParameterValue', Message 'Unable to parse pagination token' (fixed text, no values). Emulator: Code 'InvalidPaginationToken', Message 'The passed pagination token is invalid'. Traces: ec2-page-describe-volumes-tag-filtered-two-pages#task#12.

## Evidence
All three clusters: expected status 400, actual status 400, verdict mismatch.
Cause 1 (ec2-page-describe-security-groups-vpc-scoped-trailing-empty-page#task#12): Code expected 'InvalidParameterValue', got 'InvalidPaginationToken'; Message expected 'Value ( garbage-not-a-token ) for parameter NextToken is invalid. The token is invalid.', got 'The passed pagination token is invalid'.
Cause 2 (ec2-page-describe-subnets-vpc-scoped-two-pages#task#13): same expected/actual pair.
Cause 3 (ec2-page-describe-volumes-tag-filtered-two-pages#task#12): Code expected 'InvalidParameterValue', got 'InvalidPaginationToken'; Message expected 'Unable to parse pagination token', got 'The passed pagination token is invalid'.
('garbage-not-a-token' is the token these recordings send; the requirement is that the supplied token is echoed into the fixed frame.)

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Replay ec2-page-describe-security-groups-vpc-scoped-trailing-empty-page step task#12 - DescribeSecurityGroups with a NextToken that is not a valid token.
3. Repeat for ec2-page-describe-subnets-vpc-scoped-two-pages#task#13 and ec2-page-describe-volumes-tag-filtered-two-pages#task#12.
4. Each answers 400 with Code 'InvalidPaginationToken' and 'The passed pagination token is invalid' instead of the InvalidParameterValue form.
