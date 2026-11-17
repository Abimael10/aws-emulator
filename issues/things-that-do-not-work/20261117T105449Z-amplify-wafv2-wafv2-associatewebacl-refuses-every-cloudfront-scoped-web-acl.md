Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: amplify__wafv2.associate-webacl-refuses-a-regional-acl-for-an-amplify-app-and-accepts-cloudfront
Issue-Key: amplify__wafv2|AssociateWebACL|WAFInvalidParameterException
Issue-Commit: dadb011cf8b38a521b5f566109c5c6b86765cb16

# wafv2 AssociateWebACL refuses every CLOUDFRONT-scoped web ACL on scope alone, so an Amplify app can never be WAF-protected

> _Note: this issue was identified against the code base at commit `dadb011cf8b38a521b5f566109c5c6b86765cb16`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateWebACL

## Description
Cause 1 of 1: AssociateWebACL rejects a web ACL whose Scope is CLOUDFRONT without looking at what ResourceArn names, so the supported Amplify-app association path is unreachable.

AWS: Smithy (services/service-com-amazonaws-wafv2-awswaf-20190729/smithy-model/com-amazonaws-wafv2-awswaf-20190729.json) AssociateWebACL takes WebACLArn + ResourceArn and models WAFInvalidParameterException (400) for "an ARN that is malformed, or corresponds to a resource with which a web ACL can't be associated"; its documentation excludes exactly ONE resource type — "Use this for all resource types except for Amazon CloudFront distributions. For Amazon CloudFront, call UpdateDistribution" — it does not exclude CLOUDFRONT-SCOPED web ACLs on other resource types. An AWS Amplify app is such a resource: Amplify is fronted by CloudFront, so its firewall is attached by creating a CLOUDFRONT-scope web ACL at the us-east-1 WAFv2 endpoint and associating it with the app's ARN (arn:<partition>:amplify:<region>:<account>:apps/<appId>) through this operation (Amplify User Guide, amplify-waf-configuration.html). Recorded behavior: the call returns 200 with an empty body; Amplify's GetApp then reports wafConfiguration with webAclArn equal to the associated ACL's ARN and wafStatus moving asynchronously to ASSOCIATION_SUCCESS; wafv2 GetWebACLForResource(ResourceArn=<app ARN>) reports that same web ACL. A REGIONAL-scope web ACL against an Amplify app ARN is the refusal case, and it is refused with code WAFInvalidParameterException and message "The resource is not supported in current region" (fixed text; no request values in it), leaving the app unassociated.

Emulator: the handler refuses on the stored ACL's scope before considering the resource at all, answering WAFInvalidParameterException (400) with "Error reason: a CLOUDFRONT-scoped web ACL is put in front of a CloudFront distribution through the distribution's own configuration, not through this operation, field: WEB_ACL, parameter: <WebACLArn>". Because of that, the Amplify association never happens, GetApp reports no wafConfiguration, GetWebACLForResource reports nothing for the app, and the REGIONAL refusal boundary (and its "The resource is not supported in current region" sentence) is never exercised.

Why: a client following AWS's documented way to put WAF in front of an Amplify app is refused outright and cannot protect the app; conversely, code that expects the REGIONAL association to be the only refused one cannot distinguish supported from unsupported scope, since both are refused with the same class but the emulator's own message text.

Fix: the AssociateWebACL handler's scope/resource-kind decision in services/service-com-amazonaws-wafv2-awswaf-20190729/src/ops/associate_web_acl.rs (the unconditional SCOPE_CLOUDFRONT refusal at lines 256-279) must key on the KIND of resource ResourceArn names rather than on scope alone — accepting a CLOUDFRONT-scope ACL for resource kinds whose traffic is CloudFront-fronted (an Amplify app ARN, which the protected-resource ARN-form catalogue crate::state::PROTECTED_RESOURCE_FORMS must therefore recognise) and refusing a REGIONAL-scope ACL for such a resource with WAF's own "The resource is not supported in current region" sentence; plus the Amplify side's app state/GetApp projection, which must carry wafConfiguration (webAclArn and a wafStatus that settles at ASSOCIATION_SUCCESS) for an associated app and omit it for an unassociated one, and DisassociateWebACL/GetWebACLForResource reading the same record.

Traces: amplify__wafv2.associate-webacl-refuses-a-regional-acl-for-an-amplify-app-and-accepts-cloudfront.

## Evidence
Check amplify__wafv2.associate-webacl-refuses-a-regional-acl-for-an-amplify-app-and-accepts-cloudfront, errored on the first association call.

Expected (AWS): wafv2(us-east-1).AssociateWebACL(WebACLArn=<CLOUDFRONT-scope web ACL ARN>, ResourceArn=<Amplify appArn>) succeeds (HTTP 200, empty body); GetApp(appId).wafConfiguration.webAclArn == that ARN with wafStatus reaching ASSOCIATION_SUCCESS; GetWebACLForResource(ResourceArn=appArn).WebACL.ARN == that ARN. Only the REGIONAL-scope ACL against an Amplify app is refused: ("WAFInvalidParameterException", "The resource is not supported in current region").

Actual (emulator): botocore.errorfactory.WAFInvalidParameterException — "An error occurred (WAFInvalidParameterException) when calling the AssociateWebACL operation: Error reason: a CLOUDFRONT-scoped web ACL is put in front of a CloudFront distribution through the distribution's own configuration, not through this operation, field: WEB_ACL, parameter: arn:aws:wafv2:us-east-1:<account>:global/webacl/<name>/<id>". The check never reaches the wafConfiguration, GetWebACLForResource or REGIONAL-refusal assertions.

## Steps to Reproduce
1. amplify CreateApp -> note appId and appArn.
2. wafv2 in us-east-1: CreateWebACL(Scope='CLOUDFRONT', DefaultAction={'Allow':{}}, Rules=[], VisibilityConfig=...) -> note Summary.ARN.
3. wafv2 in us-east-1: AssociateWebACL(WebACLArn=<that ARN>, ResourceArn=<appArn>). The emulator refuses with WAFInvalidParameterException "Error reason: a CLOUDFRONT-scoped web ACL is put in front of a CloudFront distribution through the distribution's own configuration, not through this operation, field: WEB_ACL, parameter: <ARN>"; AWS returns an empty 200.
4. On AWS, poll amplify GetApp(appId).wafConfiguration until wafStatus == ASSOCIATION_SUCCESS and check webAclArn, then GetWebACLForResource(ResourceArn=appArn) reports the ACL — none of this is reachable on the emulator.
5. In a regional endpoint, CreateWebACL(Scope='REGIONAL') and AssociateWebACL it with a second Amplify app's ARN: AWS refuses with ("WAFInvalidParameterException", "The resource is not supported in current region") and leaves the app unassociated.
