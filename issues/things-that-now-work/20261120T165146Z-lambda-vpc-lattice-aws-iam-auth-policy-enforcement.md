Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__vpc-lattice.auth-policy-deny-overrides-a-working-in-vpc-invoke
Issue-Key: lambda__vpc-lattice.auth-policy-deny-overrides-a-working-in-vpc-invoke|run#10|TimeoutError|raise TimeoutError("the positive control reaching the Lambda target: the run "
Issue-Commit: 8345989f6f1c1ad254046c37ef06556dbc8ed377

# lambda__vpc-lattice: AWS_IAM auth-policy enforcement on a Lattice service is never reachable — the signed in-VPC GET positive control never converges, so an explicit Deny can never flip it to 403

> _Note: this issue was identified against the code base at commit `8345989f6f1c1ad254046c37ef06556dbc8ed377`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutAuthPolicy

## Description
One net-new root cause.

Cause 1 of 1: a VPC Lattice service created with authType=AWS_IAM and given an Allow auth policy for the caller's principal never serves a SigV4-signed in-VPC HTTP GET, so the authorization leg of the service (Allow -> 200, explicit Deny -> 403 AccessDeniedException) is never exercised at all. Failing check: lambda__vpc-lattice.auth-policy-deny-overrides-a-working-in-vpc-invoke.

AWS: the control plane is modelled — Smithy: CreateService.authType -> AuthType (service created AWS_IAM); PutAuthPolicy (PUT /authpolicy/{resourceIdentifier}, resourceIdentifier pattern ^((sn)|(svc)|(rcfg))-[0-9a-z]{17}...$, policy -> AuthPolicyString @length max 36864, output {policy, state: AuthPolicyState}, errors AccessDeniedException/403, ValidationException/400, ResourceNotFoundException/404); GetService.dnsEntry -> DnsEntry carries the generated domainName. The data-plane leg is recorded behavior only: on AWS, once the service-network service association and the service-network VPC association report ACTIVE and an auth policy Allowing the caller's role for vpc-lattice-svcs:Invoke on <serviceArn>/* is in place, a caller inside the associated VPC that resolves the service's dnsEntry.domainName and issues a GET signed with SigV4 for service name `vpc-lattice-svcs` is routed through the listener's forward action to the LAMBDA target group, the target function is invoked, and its response is returned as the HTTP response — HTTP 200 carrying the handler's body. Replacing that policy with the same statement at Effect=Deny makes the identical signed GET return HTTP 403 whose body is `AccessDeniedException: User: <caller arn> is not authorized to perform: vpc-lattice-svcs:Invoke on resource: <resource> with an explicit deny in a service-based policy`. Message rule: the fixed text is `AccessDeniedException: User: `, ` is not authorized to perform: `, ` on resource: `, and ` with an explicit deny in a service-based policy`; the variable parts are the caller's assumed-role ARN (arn:aws:sts::<account>:assumed-role/<role name>/<session name>), the action `vpc-lattice-svcs:Invoke` taken from the request's signing scope, and the resource, which is the service ARN with the request's URI path appended (<serviceArn><path>). The account, role name, session name, service ARN and path in any one recording are values from that request and that run's state, not part of the template.

Emulator: the Allow half never works, so the Deny half is never reached. PutAuthPolicy itself returns (it no longer refuses), and every control-plane step up to it succeeds — the target group reaches ACTIVE, RegisterTargets reports the Lambda ARN successful, the service reaches ACTIVE, and both the service-network service association and the service-network VPC association reach ACTIVE. But the signed GET issued from inside the associated VPC to the service's dnsEntry.domainName never returns HTTP 200 with the target's echo. The check retries that invoke for its full 600 s poll budget and then the run budget, and reports `TimeoutError: the positive control reaching the Lambda target: the run budget ended first; not graded`. So the emulator reports an ACTIVE, fully associated, AWS_IAM-protected service whose data path does not answer at all — it never converges to a wrong status or body, it simply never answers.

Why: a client relying on AWS's behavior protects a Lattice service with AWS_IAM and controls access with PutAuthPolicy, expecting an allowed principal to get its Lambda target's 200 and a denied principal to get a 403 AccessDeniedException. Against the emulator neither outcome is observable: the allowed call never completes, so the client cannot tell 'allowed' from 'denied' from 'broken'. Any test or deployment that validates Lattice authorization — the whole point of authType=AWS_IAM plus an auth policy — silently hangs instead of returning a verdict, and a deny that is not enforced is indistinguishable from a deny that is.

Fix: the VPC Lattice data plane and its authorizer. The generated dnsEntry.domainName must resolve and be served at an address reachable from the associated VPC; the listener's forward action must select the LAMBDA target group and invoke the registered function, serializing its {statusCode, headers, body} back as the HTTP response; and the request must be authorized by validating the SigV4 `vpc-lattice-svcs` signature against the service's stored auth policy, returning 200 on Allow and HTTP 403 with the AccessDeniedException body above on an explicit Deny. The control-plane statuses must report ACTIVE only once that path is actually servable. Related open issues that do NOT cover this check: 20261117T020113Z-lambda-vpc-lattice-vpc-lattice-an-in-vpc-http-get-through.md (the unauthenticated data path, a different check and replay key — its own text asserts this auth check was filed against a PutAuthPolicy stub, which is no longer the failure) and 20261114T190024Z-vpc-lattice-announces-dnsentry-domainname-in-the-loopback.md (the announced domainName being in the loopback zone, a plausible contributor to this non-convergence). The authorization behavior described here is filed by neither.

Traces: lambda__vpc-lattice.auth-policy-deny-overrides-a-working-in-vpc-invoke.

## Evidence
Expected (AWS):
1. With service authType=AWS_IAM and an auth policy Allowing the driver's role for vpc-lattice-svcs:Invoke on <serviceArn>/*, the SigV4-signed GET http://<dnsEntry.domainName><path> from inside the associated VPC returns HTTP 200 whose body is the Lambda target's response body.
2. After PutAuthPolicy replaces that statement with Effect=Deny, the identical signed GET returns HTTP 403 with body `AccessDeniedException: User: <assumed-role arn> is not authorized to perform: vpc-lattice-svcs:Invoke on resource: <serviceArn><path> with an explicit deny in a service-based policy`.

Actual (emulator): step 1 never happens. All control-plane calls succeed (CreateTargetGroup/GetTargetGroup -> ACTIVE, RegisterTargets -> successful contains the function ARN, CreateService/GetService -> ACTIVE, CreateListener, CreateServiceNetworkServiceAssociation -> ACTIVE, CreateServiceNetworkVpcAssociation -> ACTIVE, PutAuthPolicy -> no error), but the signed in-VPC GET never returns 200 with the target's echo within the 600 s poll budget. The check terminates with:

  TimeoutError: the positive control reaching the Lambda target: the run budget ended first; not graded

Because the positive control never converged, step 2 — the 403/AccessDeniedException the deny policy must produce — was never reached, so the emulator's auth-policy enforcement produced no observable status, error code or message at all.

## Steps to Reproduce
1. CreateVpc (10.93.0.0/16) with DNS hostnames enabled, one /24 subnet, and a security group admitting TCP 80 from the VPC CIDR.
2. CreateFunction (python3.12) for the target, returning {isBase64Encoded:false, statusCode:200, headers:{Content-Type:text/plain}, body:<nonce>+":"+event["path"]}.
3. vpc-lattice CreateTargetGroup(type=LAMBDA, config={lambdaEventStructureVersion:V2}); poll GetTargetGroup to ACTIVE; lambda AddPermission for Principal=vpc-lattice.amazonaws.com with SourceArn=<targetGroupArn>; RegisterTargets with the target function ARN.
4. CreateServiceNetwork(authType=NONE); CreateService(authType=AWS_IAM); poll GetService to ACTIVE.
5. CreateFunction for a driver with VpcConfig on the subnet/security group above, whose execution role has an inline Allow for vpc-lattice-svcs:Invoke on <serviceArn>/*; the driver SigV4-signs a GET with signing name `vpc-lattice-svcs` and issues it.
6. CreateListener(protocol=HTTP, port=80, defaultAction=forward to the target group); CreateServiceNetworkServiceAssociation and CreateServiceNetworkVpcAssociation; poll both to ACTIVE.
7. PutAuthPolicy(resourceIdentifier=<serviceId>) with Effect=Allow for the driver role, vpc-lattice-svcs:Invoke, Resource=<serviceArn>/*.
8. Invoke the driver against http://<GetService dnsEntry.domainName>/probe/<nonce> and poll.
   Observed: no response ever matches; after 600 s the check reports `TimeoutError: the positive control reaching the Lambda target: the run budget ended first; not graded`. Expected: HTTP 200 with body "<nonce>:/probe/<nonce>".
9. PutAuthPolicy again with the same statement at Effect=Deny and invoke the driver again.
   Expected: HTTP 403 with the AccessDeniedException body above. Observed: unreachable — step 8 never succeeded.
