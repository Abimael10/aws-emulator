Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__vpc-lattice.in-vpc-http-get-through-service-network-reaches-lambda-target
Issue-Key: lambda__vpc-lattice.in-vpc-http-get-through-service-network-reaches-lambda-target|run#10|TimeoutError|raise TimeoutError("the in-VPC GET reaching the Lambda target: the run budget "
Issue-Commit: c31be09f961a7f044ea59076daac7fb377aa83e8

# vpc-lattice: an in-VPC HTTP GET through a service network never reaches its Lambda target — the data path never converges

> _Note: this issue was identified against the code base at commit `c31be09f961a7f044ea59076daac7fb377aa83e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateServiceNetworkVpcAssociation

## Description
One net-new root cause.

Cause 1 of 1: a VPC Lattice service network with a Lambda-target listener rule never carries an in-VPC HTTP GET to the Lambda target; the request is retried for the check's whole 600 s poll budget and never succeeds.

AWS: recorded behavior only for the data-plane leg; the control plane is modelled (CreateServiceNetwork, CreateService, CreateTargetGroup(type=LAMBDA), RegisterTargets, CreateListener/CreateRule, CreateServiceNetworkVpcAssociation). On AWS, once the service is associated with the service network and the VPC is associated with that network, a driver instance inside the associated VPC resolving the service's generated DNS name and issuing `GET /` over HTTP 80 is routed by Lattice to the Lambda target, which is invoked with the Lattice event and whose response becomes the HTTP response — a 200 with the handler's body, reached within seconds of the associations going ACTIVE.

Emulator: the in-VPC GET never reaches the Lambda target. The check polls the driver for the expected response until its 600 s budget and then the run budget, and reports `TimeoutError: the in-VPC GET reaching the Lambda target: the run budget ended first; not graded` — i.e. no response ever matched, and the path never converged, rather than converging to a wrong status or body.

Why: a client relying on AWS's behavior puts a Lambda behind a Lattice service and calls it from inside an associated VPC by the service's DNS name. Against the emulator that call never completes, so the entire Lattice data plane (DNS for the generated service domain, listener/rule matching, and the Lambda target invocation) is unusable, and no in-VPC consumer of a Lattice service can be exercised.

Fix: the VPC Lattice data plane — the generated service domain name must resolve inside an associated VPC, the listener's default/match rule must select the LAMBDA target group, and the matched request must be turned into a Lambda invocation whose result is serialized back as the HTTP response. The control-plane associations must also report ACTIVE only once that path is actually servable.

Traces: lambda__vpc-lattice.in-vpc-http-get-through-service-network-reaches-lambda-target. Note the sibling check in this service (auth-policy-deny-overrides-a-working-in-vpc-invoke) is already filed against the unimplemented PutAuthPolicy stub; this issue is the data path itself, which that stub issue does not cover.

## Evidence
Expected (AWS): from an instance in the VPC associated with the service network, `GET http://<service-dns-name>/` returns the Lambda target's response (HTTP 200 with the handler's body) within seconds of the service-network and VPC associations reporting ACTIVE.

Actual (emulator): the driver's GET never returns the expected response. After a 600 s poll the check raises `TimeoutError: the in-VPC GET reaching the Lambda target: the run budget ended first; not graded` — no status/body was ever observed from the Lattice data plane.

## Steps to Reproduce
1. CreateVpc + one subnet + a security group admitting HTTP from the VPC CIDR; launch a driver instance in that subnet.
2. CreateFunction (python3.12) returning a fixed body; CreateTargetGroup(Type=LAMBDA) and RegisterTargets with that function.
3. CreateServiceNetwork; CreateService; CreateServiceNetworkServiceAssociation; CreateListener(protocol=HTTP, port=80) with a rule forwarding to the Lambda target group.
4. CreateServiceNetworkVpcAssociation for the driver's VPC; wait for the associations to report ACTIVE.
5. From the driver, `GET http://<service dns name>/` and poll.
Observed: the GET never reaches the Lambda target — no matching response inside a 600 s budget. Expected: HTTP 200 carrying the function's body.
