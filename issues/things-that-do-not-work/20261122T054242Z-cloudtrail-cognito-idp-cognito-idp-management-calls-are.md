Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudtrail__cognito-idp.cognito-secret-request-parameters-are-hidden-in-the-delivered-record
Issue-Key: cloudtrail__cognito-idp.cognito-secret-request-parameters-are-hidden-in-the-delivered-record|run#26|Timeout|time.sleep(_POLL_INTERVAL_S)
Issue-Commit: fceda236fd74c050af49bdc92c31da84e151e4fd

# cloudtrail__cognito-idp: cognito-idp management calls are never auditable through LookupEvents, so the secret-redaction audit poll never completes

> _Note: this issue was identified against the code base at commit `fceda236fd74c050af49bdc92c31da84e151e4fd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
LookupEvents

## Description
Cause 1 of 1: no CloudTrail record for a cognito-idp management call is ever returned by LookupEvents, so the audit poll that the redaction check is built on never finds either the secret-bearing event or its positive control.
AWS: recorded behavior only for the delivery itself (LookupEvents is modelled, the record content is not). CloudTrail records every cognito-idp management call and LookupEvents returns it (indexed in roughly 80-136 s in the recording) with eventSource 'cognito-idp.amazonaws.com', recipientAccountId = the caller's account, and requestParameters holding the call's arguments: for DescribeUserPool the pool id verbatim, and for InitiateAuth the non-secret members verbatim (authFlow, clientId) with the secret-bearing member's VALUE replaced by the fixed literal string 'HIDDEN_DUE_TO_SECURITY_REASONS' — the member authParameters is kept and its map value becomes that plain string, so neither the plaintext password nor any token the sign-in minted appears anywhere in the delivered record.
Emulator: LookupEvents returns no event for either call. The check polls LookupEvents by EventName for 'InitiateAuth' and 'DescribeUserPool' (made seconds earlier, both successful) and never sees them, so it runs to the harness cap and the check is reported as 'Check timed out after 60.0s'. Because no record is ever served, the redaction rule itself is untested: it cannot be told apart from 'this target records no request arguments at all'.
Why: a client that monitors authentication or administrative activity on a user pool — a SIEM poll, an incident query, a compliance control that asserts credentials are absent from audit logs — gets an empty answer from the emulator and silently concludes nothing happened. Any such integration passes vacuously against the emulator and fails against AWS.
Fix: the CloudTrail LookupEvents read path and the recording hook that feeds it for cognito-idp management operations (the cloudtrail service crate's event store and the per-operation audit emission), plus the request-parameter redaction rule for InitiateAuth's authParameters once records are served. This is the same shape as the open issues 20261108T081736Z-autoscaling-cloudtrail-... (Auto Scaling calls never auditable through LookupEvents) and 20261029T173328Z-cloudtrail-records-and-delivers-no-audit-record-for.md; neither names cognito-idp or carries this check's replay key.
Traces: cloudtrail__cognito-idp.cognito-secret-request-parameters-are-hidden-in-the-delivered-record.

## Evidence
Error reported by the suite: 'Check timed out after 60.0s' for cloudtrail__cognito-idp.cognito-secret-request-parameters-are-hidden-in-the-delivered-record. The check performs InitiateAuth (USER_PASSWORD_AUTH, succeeding with an AuthenticationResult) and DescribeUserPool, then polls CloudTrail LookupEvents with LookupAttributes EventName=InitiateAuth and EventName=DescribeUserPool over a window starting 3 minutes earlier, paging to exhaustion every 2 s; it breaks out of the poll as soon as both records appear. It never breaks out, so neither record is served. Expected on the first successful poll: an InitiateAuth event whose CloudTrailEvent has eventSource='cognito-idp.amazonaws.com', requestParameters.authFlow='USER_PASSWORD_AUTH', requestParameters.clientId=<the app client id> and requestParameters.authParameters equal to the string 'HIDDEN_DUE_TO_SECURITY_REASONS', plus a DescribeUserPool event whose requestParameters name the pool id. Actual: LookupEvents returns no matching events at all.

## Steps to Reproduce
1. CreateUserPool + CreateUserPoolClient with ALLOW_USER_PASSWORD_AUTH; AdminCreateUser + AdminSetUserPassword(Permanent=True).
2. Call InitiateAuth with AuthFlow=USER_PASSWORD_AUTH and AuthParameters={USERNAME, PASSWORD} (it succeeds and returns tokens), then DescribeUserPool on the same pool.
3. Call cloudtrail LookupEvents with LookupAttributes=[{AttributeKey:'EventName', AttributeValue:'InitiateAuth'}] over a StartTime/EndTime window covering the call, paging NextToken to exhaustion; repeat for 'DescribeUserPool'. Both return no events, however long the poll runs.
4. With records served, assert requestParameters.authParameters == 'HIDDEN_DUE_TO_SECURITY_REASONS' while authFlow and clientId are present verbatim, and that neither the plaintext password nor any returned token appears in any InitiateAuth / AdminSetUserPassword / AdminCreateUser record.
