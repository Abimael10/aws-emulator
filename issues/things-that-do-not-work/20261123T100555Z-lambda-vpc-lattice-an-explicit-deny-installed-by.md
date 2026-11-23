Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__vpc-lattice.auth-policy-deny-overrides-a-working-in-vpc-invoke
Issue-Key: lambda__vpc-lattice.auth-policy-deny-overrides-a-working-in-vpc-invoke|run#10|TimeoutError|raise TimeoutError("the deny policy taking effect: the run budget ended first; "
Issue-Commit: 4de1224534ccf5deabb70883d8a6d9cce6b35fa7

# lambda__vpc-lattice: an explicit Deny installed by PutAuthPolicy is never enforced on the Lattice service data path — the signed in-VPC GET keeps returning 200 instead of 403 AccessDeniedException

> _Note: this issue was identified against the code base at commit `4de1224534ccf5deabb70883d8a6d9cce6b35fa7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutAuthPolicy

## Description
One net-new root cause.

Cause 1 of 1: a VPC Lattice service created with authType=AWS_IAM now serves the SigV4-signed in-VPC HTTP GET correctly (the Allow half of the check converges to HTTP 200 with the Lambda target's echo), but replacing the service's auth policy with the identical statement at Effect=Deny changes nothing: the same signed GET keeps answering HTTP 200 with the target's body for the whole 600 s poll budget and never flips to HTTP 403. The authorizer on the Lattice data plane reads the request but does not evaluate the stored auth policy's Deny statements. Failing check: lambda__vpc-lattice.auth-policy-deny-overrides-a-working-in-vpc-invoke.

AWS: the control plane is modelled — Smithy: PutAuthPolicy (PUT /authpolicy/{resourceIdentifier}, http code 200; resourceIdentifier -> ResourceIdentifier @required @httpLabel @length min 17 max 200 and @pattern ^((((sn)|(svc)|(rcfg))-[0-9a-z]{17})|(arn...:((servicenetwork/sn)|(resourceconfiguration/rcfg)|(service/svc))-[0-9a-z]{17}))$; policy -> AuthPolicyString @required @length max 36864; output {policy, state -> AuthPolicyState}; errors AccessDeniedException/403, ValidationException/400, ResourceNotFoundException/404, ThrottlingException/429, InternalServerException/500), and GetAuthPolicy returns the stored {policy, state, createdAt, lastUpdatedAt}. The ENFORCEMENT leg is not in the Smithy model at all — it happens on the vpc-lattice-svcs data-plane endpoint — so it is recorded behavior only: on AWS, once a service with authType=AWS_IAM carries an auth policy whose statement is Effect=Deny for the calling principal on Action vpc-lattice-svcs:Invoke and Resource <serviceArn>/*, a caller inside the associated VPC that resolves the service's dnsEntry.domainName and issues a GET signed with SigV4 for signing name `vpc-lattice-svcs` is answered HTTP 403 and the target function is NOT invoked. The 403 body is `AccessDeniedException: User: <caller arn> is not authorized to perform: vpc-lattice-svcs:Invoke on resource: <resource> with an explicit deny in a service-based policy`. Message rule: the fixed text is `AccessDeniedException: User: `, ` is not authorized to perform: `, ` on resource: ` and ` with an explicit deny in a service-based policy`; the variable parts are the caller's assumed-role ARN in the form arn:aws:sts::<account>:assumed-role/<role name>/<session name>, the action `vpc-lattice-svcs:Invoke` taken from the request's signing scope, and the resource, which is the service ARN with the request's URI path appended (<serviceArn><path>). The account id, role name, session name, service ARN and path seen in any one run are values from that request and that run's state and are not part of the template. An explicit Deny in a service-based policy overrides any Allow in the caller's identity policy, so the driver role's own inline Allow for vpc-lattice-svcs:Invoke must not rescue the call.

Emulator: every control-plane step succeeds and the data path now works — the target group reaches ACTIVE, RegisterTargets reports the Lambda ARN successful, the service reaches ACTIVE, both the service-network service association and the service-network VPC association reach ACTIVE, PutAuthPolicy returns without error, and the signed in-VPC GET returns HTTP 200 whose body is exactly the target's `<nonce>:<path>` echo. PutAuthPolicy is then called again with the same statement at Effect=Deny and returns without error, but the identical signed GET keeps returning HTTP 200 with the same target echo on every retry for the full 600 s poll budget; no 403 and no AccessDeniedException body is ever produced, and the run budget ends with `TimeoutError: the deny policy taking effect: the run budget ended first; not graded`. So the emulator stores the Deny policy but its data-plane authorizer never consults it: the deny is inert and the Lambda target is still invoked.

Why: a client relying on AWS's behavior protects a Lattice service with authType=AWS_IAM and controls who may call it with PutAuthPolicy. Against the emulator a Deny is accepted and reads back, but the call it forbids still succeeds end-to-end and still invokes the backend function. Authorization tests pass against a policy that denies nothing, revoking access by policy appears to work while traffic keeps flowing, and a client cannot distinguish an enforced deny from an unenforced one — the inert deny is the worst possible failure mode for the feature whose entire purpose is to refuse the call.

Fix: the VPC Lattice data-plane authorizer (the component that terminates the service's listener and authenticates the SigV4 `vpc-lattice-svcs` request). It must evaluate the service's stored auth policy for every request against the authenticated caller's principal, the action vpc-lattice-svcs:Invoke and the resource <serviceArn><request path>, apply explicit-Deny precedence over any identity-policy Allow, and on a Deny short-circuit before the listener's forward action runs (the target function must not be invoked), answering HTTP 403 with the AccessDeniedException body described above. PutAuthPolicy's handler/state must make the replacement policy effective for subsequent requests rather than only persisting it for GetAuthPolicy.

Related open issue that does NOT cover this: 20261120T165146Z-lambda-vpc-lattice-aws-iam-auth-policy-enforcement.md describes the earlier state of this same check, where the POSITIVE control never converged so the deny half was never reached; its replay key is pinned to the `the positive control reaching the Lambda target` timeout. The positive control now converges, and the failure has moved to the deny never taking effect — which that issue records as unreachable, not as observed. Also related and not covering it: 20261114T190024Z-vpc-lattice-announces-dnsentry-domainname-in-the-loopback.md (the announced domainName's zone).

Traces: lambda__vpc-lattice.auth-policy-deny-overrides-a-working-in-vpc-invoke.

## Evidence
Expected (AWS):
1. Positive control: with service authType=AWS_IAM and an auth policy Allowing the driver role for vpc-lattice-svcs:Invoke on <serviceArn>/*, the SigV4-signed GET http://<dnsEntry.domainName><path> from inside the associated VPC returns HTTP 200 whose body is the Lambda target's echo "<nonce>:<path>".
2. After PutAuthPolicy replaces that statement with Effect=Deny for the same principal, the identical signed GET returns HTTP 403 and the target is not invoked. Body:
   AccessDeniedException: User: arn:aws:sts::<account>:assumed-role/<driver role name>/<driver function name> is not authorized to perform: vpc-lattice-svcs:Invoke on resource: <serviceArn><path> with an explicit deny in a service-based policy
   (fixed text: "AccessDeniedException: User: ", " is not authorized to perform: ", " on resource: ", " with an explicit deny in a service-based policy"; variable: the assumed-role ARN, the action from the signing scope, and <serviceArn> + the request path.)

Actual (emulator):
1. Step 1 succeeds — status 200, body "<nonce>:<path>" — so the data path and the Allow half are fine.
2. PutAuthPolicy with the Deny statement returns without error, but every subsequent identical signed GET still returns status 200 with the same target echo. No 403, no error code and no AccessDeniedException message is ever emitted. After the check's 600 s poll budget plus the remaining run budget the check ends with:

  TimeoutError: the deny policy taking effect: the run budget ended first; not graded

The divergence is the status (200 where AWS answers 403), the absent error code (none where AWS answers AccessDeniedException) and the absent message (the target's body where AWS answers the explicit-deny text above). The backend Lambda is also invoked where AWS would not invoke it.

## Steps to Reproduce
1. CreateVpc (10.93.0.0/16) with DNS hostnames enabled, one subnet, and a security group admitting TCP 80 from the VPC CIDR.
2. lambda CreateFunction (python3.12) for the target, returning {isBase64Encoded:false, statusCode:200, statusDescription:"200 OK", headers:{Content-Type:text/plain}, body:<nonce>+":"+event["path"]}.
3. vpc-lattice CreateTargetGroup(type=LAMBDA, config={lambdaEventStructureVersion:V2}); poll GetTargetGroup to ACTIVE; lambda AddPermission for Principal=vpc-lattice.amazonaws.com with SourceArn=<targetGroupArn>; RegisterTargets with the target function ARN.
4. CreateServiceNetwork(authType=NONE); CreateService(authType=AWS_IAM); poll GetService to ACTIVE; CreateListener(protocol=HTTP, port=80, defaultAction=forward to the target group); CreateServiceNetworkServiceAssociation and CreateServiceNetworkVpcAssociation; poll both to ACTIVE.
5. lambda CreateFunction for a driver with VpcConfig on that subnet/security group, whose execution role carries an inline Allow for vpc-lattice-svcs:Invoke on <serviceArn>/*. The driver SigV4-signs a GET (signing name `vpc-lattice-svcs`) to the URL it is handed and returns {status, body}.
6. PutAuthPolicy(resourceIdentifier=<serviceId>) with Effect=Allow, Principal={AWS:[<driver role arn>]}, Action=vpc-lattice-svcs:Invoke, Resource=<serviceArn>/*.
7. Invoke the driver against http://<GetService dnsEntry.domainName>/probe/<nonce> and poll until status==200 and body=="<nonce>:/probe/<nonce>".
   Observed: converges (this half is correct).
8. PutAuthPolicy(resourceIdentifier=<serviceId>) again with the SAME statement at Effect=Deny.
9. Invoke the driver against the same URL again and poll for status==403.
   Expected: HTTP 403 with the AccessDeniedException explicit-deny body, and no invocation of the target function.
   Observed: HTTP 200 with the target echo on every attempt for 600 s; the check ends with `TimeoutError: the deny policy taking effect: the run budget ended first; not graded`.
10. GetAuthPolicy(resourceIdentifier=<serviceId>) shows the Deny policy is stored — only the data-plane authorizer ignores it.
