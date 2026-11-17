Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__elbv2.create-rule-regex-and-value-condition-together-never-routes ec2__elbv2.elastic-ip-subnet-mapping-rejected-for-an-application-load-balancer ec2__elbv2.vpc-endpoint-service-configuration-accepts-only-network-load-balancers
Issue-Key: ec2__elbv2.create-rule-regex-and-value-condition-together-never-routes|_error_code#12|AssertionError|raise AssertionError("expected the call to be refused, but it succeeded")
Issue-Key: ec2__elbv2.elastic-ip-subnet-mapping-rejected-for-an-application-load-balancer|run#12|AssertionError|assert msg == "Elastic IPs are not supported for load balancers with type 'application'", (
Issue-Key: ec2__elbv2.vpc-endpoint-service-configuration-accepts-only-network-load-balancers|run#11|AssertionError|assert bad_id is None, f"an endpoint service configuration backed by an ALB was created: {bad_id!r}"
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# ec2__elbv2: 3 net-new causes — a path-pattern condition with both Values and RegexValues is accepted, the ALB Elastic-IP refusal is reworded, and an endpoint service configuration accepts an ALB

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRule

## Description
Three net-new root causes across the EC2/ELBv2 boundary.

Cause 1 of 3 — CreateRule accepts a path-pattern condition carrying both Values and RegexValues.
AWS: Smithy — `CreateRule.Conditions` (RuleConditionList) with `PathPatternConfig`; the two value lists are mutually exclusive, and AWS refuses a condition config that supplies both with error code `ValidationError` and message `You must provide exactly one of the following: ['Values' 'RegexValues'] for config of type 'PathPatternConfig'`. Fixed text: everything shown; the only variable part is the config type name ('PathPatternConfig'), taken from the condition in the request. No rule is created.
Emulator: the CreateRule SUCCEEDS and a rule is created holding both lists, so a rule that AWS refuses exists and (per the check's name) never routes anything.
Why: a client that mistakenly sets both fields gets a silently dead rule instead of an immediate validation error — requests fall through to the listener default and the misconfiguration is invisible in describe output that echoes both lists.
Fix: CreateRule/ModifyRule condition validation in the ELBv2 handler — exactly one of Values/RegexValues per condition config, refused with the ValidationError above.
Traces: ec2__elbv2.create-rule-regex-and-value-condition-together-never-routes.

Cause 2 of 3 — the Elastic-IP-on-an-ALB refusal message is emulator-authored.
AWS: Smithy — `CreateLoadBalancer.SubnetMappings` (SubnetMapping.AllocationId) and error `InvalidConfigurationRequestException` (awsQueryError InvalidConfigurationRequest, HTTP 400). Elastic IPs are a Network Load Balancer feature; for Type='application' AWS refuses with the message `Elastic IPs are not supported for load balancers with type 'application'`. Fixed text: "Elastic IPs are not supported for load balancers with type '" + the Type value from the request + "'". The same mapping on Type='network' is accepted and the NLB then reports that exact address per zone.
Emulator: the call is refused, but with the message `You cannot specify Elastic IP addresses with a load balancer of type 'application'` — different wording for the same error class.
Why: the status and code are right but the message is not AWS's. Clients, SDK retry/branch logic and operators that match on the message text (or surface it to users) see a string AWS never emits, and message-level assertions in test suites diverge.
Fix: the CreateLoadBalancer validation message in the ELBv2 handler — emit AWS's wording for the Elastic-IP-on-non-network refusal.
Traces: ec2__elbv2.elastic-ip-subnet-mapping-rejected-for-an-application-load-balancer.

Cause 3 of 3 — CreateVpcEndpointServiceConfiguration accepts an Application Load Balancer ARN.
AWS: recorded behavior only for the exact refusal (the EC2 model declares the generic client-error surface for CreateVpcEndpointServiceConfiguration): a PrivateLink endpoint service configuration may be backed only by Network (or Gateway) Load Balancers, so an ALB ARN passed in NetworkLoadBalancerArns is refused and no configuration is created. The recording asserts no configuration comes back for the ALB attempt while the NLB attempt succeeds.
Emulator: the ALB-backed call SUCCEEDS and a service configuration is created (ServiceId returned), so an endpoint service exists that AWS would never allow.
Why: a client's PrivateLink wiring appears valid against an ALB and then has no equivalent in AWS; the negative control passes, and a consumer endpoint can be pointed at a service configuration that cannot exist, so the divergence only shows up as a dead data path.
Fix: CreateVpcEndpointServiceConfiguration validation in the EC2 handler — reject a load balancer ARN whose type is 'application' in NetworkLoadBalancerArns, creating no configuration.
Traces: ec2__elbv2.vpc-endpoint-service-configuration-accepts-only-network-load-balancers.

## Evidence
Cause 1 — expected: ClientError ('ValidationError', "You must provide exactly one of the following: ['Values' 'RegexValues'] for config of type 'PathPatternConfig'"); no rule created.
Actual (replay):
  AssertionError: expected the call to be refused, but it succeeded
  (check_ec2__elbv2_create_rule_regex_and_value_condition_together_never_routes.py line 79, _error_code)

Cause 2 — expected message: "Elastic IPs are not supported for load balancers with type 'application'".
Actual (replay):
  AssertionError: ALB with Elastic IP mappings message: "You cannot specify Elastic IP addresses with a load balancer of type 'application'"
  (check_ec2__elbv2_elastic_ip_subnet_mapping_rejected_for_an_application_load_balancer.py line 42)

Cause 3 — expected: the ALB-backed CreateVpcEndpointServiceConfiguration is refused and leaves no configuration.
Actual (replay):
  AssertionError: an endpoint service configuration backed by an ALB was created: 'vpce-svc-<id>'
  (check_ec2__elbv2_vpc_endpoint_service_configuration_accepts_only_network_load_balancers.py line 42)
(the service id is a per-run generated value)

## Steps to Reproduce
Cause 1:
1. Create a VPC/subnets/SG, an internal ALB and a listener.
2. CreateRule with Conditions=[{Field:'path-pattern', PathPatternConfig:{Values:['/a'], RegexValues:['^/a$']}}] and a fixed-response action.
Observed: the rule is created. Expected: ValidationError with the exactly-one-of message.

Cause 2:
1. Allocate one Elastic IP per subnet.
2. CreateLoadBalancer Name=<n>, Type='application', Scheme='internet-facing', SubnetMappings=[{SubnetId, AllocationId}…].
Observed: refused, message "You cannot specify Elastic IP addresses with a load balancer of type 'application'". Expected: "Elastic IPs are not supported for load balancers with type 'application'".
3. Repeat with Type='network': accepted, and DescribeLoadBalancers reports each allocated address per zone.

Cause 3:
1. Create a VPC with subnets; create an internal NLB and an internal ALB; wait for both active.
2. ec2.create_vpc_endpoint_service_configuration(NetworkLoadBalancerArns=[<nlb arn>]) — succeeds.
3. Call it again with NetworkLoadBalancerArns=[<alb arn>].
Observed: a ServiceId is returned. Expected: refused, with no configuration created (DescribeVpcEndpointServiceConfigurations shows only the NLB-backed one).
