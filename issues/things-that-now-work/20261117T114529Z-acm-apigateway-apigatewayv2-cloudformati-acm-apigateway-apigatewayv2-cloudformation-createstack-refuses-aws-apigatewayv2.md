Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__apigateway__apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-routingrule-matches-then-updates-and-replaces acm__apigateway__apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-routingrule-priority-over-maximum-rejected
Issue-Key: acm__apigateway__apigatewayv2__cloudformation|CreateStack|ValidationError
Issue-Commit: 14940b74adde738ffb545252b71aede81788f071

# acm__apigateway__apigatewayv2__cloudformation: CreateStack refuses AWS::ApiGatewayV2::RoutingRule as an unrecognized resource type

> _Note: this issue was identified against the code base at commit `14940b74adde738ffb545252b71aede81788f071`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: CloudFormation does not register the AWS::ApiGatewayV2::RoutingRule resource type, so any template declaring one is refused at CreateStack.
AWS: recorded behavior only for the registry contents — CloudFormation accepts a template declaring AWS::ApiGatewayV2::RoutingRule and provisions it through apigatewayv2 (CreateRoutingRule on a custom domain name: Priority, Conditions, Actions), so the stack reaches CREATE_COMPLETE and the rule is readable through apigatewayv2 GetRoutingRule/ListRoutingRules; an out-of-range Priority is refused by the resource handler (reported on the resource's CREATE_FAILED row), not by template parsing. CloudFormation's "Template format error: Unrecognized resource types: [...]" ValidationError is AWS's answer only for a type that does not exist at all.
Emulator: both stacks are refused at CreateStack with HTTP 400, Error.Code "ValidationError" and Message "Template format error: Unrecognized resource types: [AWS::ApiGatewayV2::RoutingRule]" — fixed text "Template format error: Unrecognized resource types: [" ... "]", the bracketed list taken from the template's declared types.
Why: a client cannot deploy HTTP-API routing rules through infrastructure-as-code at all — the template is rejected before any resource is created, so neither the happy-path round trip (create, match, update, replace) nor the modeled rejection of an over-maximum Priority can be exercised. The failure also looks like a user template error rather than a missing capability.
Fix: the CloudFormation resource-type registry plus a resource provider for AWS::ApiGatewayV2::RoutingRule that creates/updates/replaces/deletes through the apigatewayv2 routing-rule state (Ref/physical id = the rule identifier) and surfaces the service's own refusal for an out-of-range Priority on the resource's CREATE_FAILED row.
Traces: acm__apigateway__apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-routingrule-matches-then-updates-and-replaces, acm__apigateway__apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-routingrule-priority-over-maximum-rejected.

## Evidence
Expected: CreateStack accepts the template and the stack reaches a terminal status whose Certificate/RoutingRule resources are readable (happy path), or the stack fails on the RoutingRule resource's own Priority validation (negative path).
Actual, both checks: botocore.exceptions.ClientError: An error occurred (ValidationError) when calling the CreateStack operation: Template format error: Unrecognized resource types: [AWS::ApiGatewayV2::RoutingRule]

## Steps to Reproduce
1. Request/issue an ACM certificate and create an apigatewayv2 custom domain name using it.
2. cloudformation CreateStack with a template containing a resource of Type AWS::ApiGatewayV2::RoutingRule (Properties: DomainNameArn, Priority, Conditions, Actions).
3. The call is refused 400 ValidationError "Template format error: Unrecognized resource types: [AWS::ApiGatewayV2::RoutingRule]" instead of creating the stack. The same refusal blocks the variant whose Priority exceeds the allowed maximum, which AWS reports as the resource's own CREATE_FAILED.
