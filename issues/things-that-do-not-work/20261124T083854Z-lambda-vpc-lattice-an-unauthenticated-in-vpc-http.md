Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__vpc-lattice.in-vpc-http-get-through-service-network-reaches-lambda-target
Issue-Key: lambda__vpc-lattice.in-vpc-http-get-through-service-network-reaches-lambda-target|run#10|TimeoutError|raise TimeoutError("the in-VPC GET reaching the Lambda target: the run budget "
Issue-Commit: 24307ed54ea96c21794ebdf30494345495199505

# lambda__vpc-lattice: an unauthenticated in-VPC HTTP GET to an authType=NONE Lattice service still never reaches its Lambda target — the data path never answers

> _Note: this issue was identified against the code base at commit `24307ed54ea96c21794ebdf30494345495199505`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateServiceNetworkVpcAssociation

## Description
REOPENS 20261117T020113Z-lambda-vpc-lattice-vpc-lattice-an-in-vpc-http-get-through.md: its fix left the call refused. That issue is marked fixed and carries this exact check id and replay key, but the check still fails with the identical timeout, so the unauthenticated Lattice data path is still not servable.

One net-new root cause.

Cause 1 of 1: a VPC Lattice service created with authType=NONE, fronted by an HTTP:80 listener whose defaultAction forwards to a LAMBDA target group, never answers a plain (unsigned) HTTP GET issued from inside the VPC associated with the service network. Every control-plane step converges (target group ACTIVE, RegisterTargets reports the function ARN successful, service ACTIVE, service-network service association ACTIVE, service-network VPC association ACTIVE), and GetService returns a dnsEntry.domainName, but the GET to http://<domainName>/probe/<nonce> issued by an in-VPC driver Lambda never returns HTTP 200 with the target's body for the check's whole 600 s poll budget. Failing check: lambda__vpc-lattice.in-vpc-http-get-through-service-network-reaches-lambda-target.

AWS: the control plane is modelled — Smithy: CreateServiceNetworkVpcAssociation (POST /servicenetworkvpcassociations, http code 200, input {serviceNetworkIdentifier @required, vpcIdentifier @required @pattern ^vpc-(([0-9a-z]{8})|([0-9a-z]{17}))$, securityGroupIds @length max 5}, output {id @pattern ^snva-[0-9a-z]{17}$, status -> ServiceNetworkVpcAssociationStatus}); CreateListener (POST /services/{serviceIdentifier}/listeners, http code 201, input {protocol -> ListenerProtocol @required, port @range 1..65535, defaultAction -> RuleAction @required}); RegisterTargets (POST /targetgroups/{targetGroupIdentifier}/registertargets, output {successful, unsuccessful}); GetService (GET /services/{serviceIdentifier}, output member dnsEntry -> DnsEntry carrying the generated domainName). The data-plane leg is NOT modelled, so it is recorded behavior only: on AWS, once the service-network service association and the service-network VPC association report ACTIVE, a caller inside the associated VPC resolves the service's dnsEntry.domainName and an ordinary unsigned `GET /<path>` over HTTP port 80 is matched by the listener's defaultAction, forwarded to the LAMBDA target group, and the registered function is invoked with the Lattice V2 event; the function's {statusCode, headers, body} becomes the HTTP response, i.e. HTTP 200 whose body is exactly what the handler returned (here `<nonce>:<path>`, where the nonce and path are values of that run's request, not part of the requirement). A service whose authType is NONE requires no SigV4 signature at all: the unsigned request must be served.

Emulator: the request is never served. The in-VPC driver's urlopen of http://<dnsEntry.domainName><path> never yields status 200 with the target's echo; the check retries every 3 s for its 600 s poll budget and then the run budget, ending with `TimeoutError: the in-VPC GET reaching the Lambda target: the run budget ended first; not graded`. No status, error code or body is ever observed from the data plane — it does not converge to a wrong answer, it gives none. Note the contrast recorded in the sibling check's open issue 20261123T100555Z-lambda-vpc-lattice-an-explicit-deny-installed-by.md: on an authType=AWS_IAM service, a SigV4-signed (`vpc-lattice-svcs`) in-VPC GET now DOES return 200 with the target's echo. So the forward-to-Lambda path exists for the signed/AWS_IAM case while the unsigned authType=NONE case still answers nothing. Hypothesis: the data-plane listener only accepts requests carrying a vpc-lattice-svcs SigV4 signature (or only binds for AWS_IAM services), and/or the announced domainName is not reachable on the plain path for this configuration.

Why: a client relying on AWS's behavior puts a Lambda behind a Lattice service with authType=NONE — the ordinary, unauthenticated service-mesh case — and calls it by the service's DNS name from an associated VPC. Against the emulator that call never completes: it cannot be distinguished from a network fault, there is no status to assert, and every in-VPC consumer of an unauthenticated Lattice service hangs. Any suite that exercises the Lattice data plane without SigV4 is unusable.

Fix: the VPC Lattice data plane — the listener/endpoint component that terminates the service's HTTP listener and the target-group forward action. The address announced in GetService.dnsEntry.domainName must be reachable from the associated VPC for an unsigned request; the listener's defaultAction must select the LAMBDA target group and invoke the registered function with the V2 event; the function's response must be serialized back as the HTTP response; and admission must not require a SigV4 `vpc-lattice-svcs` signature when the service's authType is NONE. The control-plane statuses (service, service-network service association, service-network VPC association) must report ACTIVE only once that path is actually servable. Related open issues that do NOT cover this cause: 20261120T165146Z-lambda-vpc-lattice-aws-iam-auth-policy-enforcement.md and 20261123T100555Z-lambda-vpc-lattice-an-explicit-deny-installed-by.md are both the AWS_IAM auth-policy enforcement leg of the sibling check (a signed request and a Deny that is not enforced), not the unsigned authType=NONE data path.

Traces: lambda__vpc-lattice.in-vpc-http-get-through-service-network-reaches-lambda-target.

## Evidence
Expected (AWS): with ServiceNetwork(authType=NONE), Service(authType=NONE), a LAMBDA target group holding the target function, an HTTP:80 listener forwarding to it, and both the service-network service association and the service-network VPC association ACTIVE, an unsigned `GET http://<GetService dnsEntry.domainName>/probe/<nonce>` issued from a Lambda running in the associated VPC returns HTTP 200 whose body is the target handler's own body (`<nonce>:/probe/<nonce>` for this check's handler; the nonce and path are this run's request values).

Actual (emulator): all control-plane calls succeed — GetTargetGroup -> ACTIVE, RegisterTargets successful == {target function ARN}, GetService -> ACTIVE with a dnsEntry.domainName, CreateListener accepted, GetServiceNetworkServiceAssociation -> ACTIVE, GetServiceNetworkVpcAssociation -> ACTIVE — but the in-VPC GET never produces status 200 with that body. The check polls the driver every 3 s for 600 s and then exhausts the run budget, reporting:

  TimeoutError: the in-VPC GET reaching the Lambda target: the run budget ended first; not graded

No HTTP status, no error code and no body is ever returned by the Lattice data plane for the unsigned request, where AWS returns 200 with the target's body. The already-fixed issue 20261117T020113Z-lambda-vpc-lattice-vpc-lattice-an-in-vpc-http-get-through.md was filed for exactly this check and carries exactly this replay key; the check still fails identically, so the fix did not make the unauthenticated data path servable.

## Steps to Reproduce
1. ec2 CreateVpc (10.93.0.0/16) with DNS hostnames enabled, one /24 subnet in an AZ that admits Lambda ENIs, and a security group admitting TCP 80 from the VPC CIDR.
2. lambda CreateFunction (python3.12) for the target, returning {isBase64Encoded:false, statusCode:200, statusDescription:"200 OK", headers:{Content-Type:text/plain}, body:<nonce>+":"+event["path"]}; wait function_active_v2.
3. lambda CreateFunction for a driver with VpcConfig on that subnet/security group; its handler does an ordinary urllib GET of event["url"] with Host: event["host"] and returns {status, body}.
4. vpc-lattice CreateTargetGroup(type=LAMBDA, config={lambdaEventStructureVersion:V2}); poll GetTargetGroup to ACTIVE; lambda AddPermission(Principal=vpc-lattice.amazonaws.com, SourceArn=<targetGroupArn>); RegisterTargets with the target function ARN and assert successful contains it.
5. CreateServiceNetwork(authType=NONE); CreateService(authType=NONE); poll GetService to ACTIVE; CreateListener(protocol=HTTP, port=80, defaultAction=forward to the target group).
6. CreateServiceNetworkServiceAssociation(serviceNetwork, service) and CreateServiceNetworkVpcAssociation(serviceNetwork, vpc, securityGroupIds=[sg]); poll both to ACTIVE.
7. Read domainName = GetService(serviceIdentifier).dnsEntry.domainName and Invoke the driver with {url: "http://<domainName>/probe/<nonce>", host: "<domainName>"}; poll.
   Expected: status 200 and body "<nonce>:/probe/<nonce>".
   Observed: no attempt ever returns 200; after the 600 s poll budget the check ends with `TimeoutError: the in-VPC GET reaching the Lambda target: the run budget ended first; not graded`.
