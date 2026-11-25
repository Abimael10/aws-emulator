Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticbeanstalk__logs.eb-stream-logs-delivers-the-application-stdout-to-the-derived-log-group
Issue-Key: elasticbeanstalk__logs.eb-stream-logs-delivers-the-application-stdout-to-the-derived-log-group|_routed_request#20|PublishedUrlRejected|target, route = ctx.endpoint_route(url)
Issue-Commit: 31f7b518324c5238b816f5187d2718623b5667f6

# An Elastic Beanstalk environment's CNAME is published as <name>.<region>.elasticbeanstalk.com, an address the emulator does not serve

> _Note: this issue was identified against the code base at commit `31f7b518324c5238b816f5187d2718623b5667f6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEnvironments

## Description
Cause 1 of 1: the environment announces an AWS production hostname instead of the address it is reached on.
AWS: EnvironmentDescription.CNAME is the URL a client requests to reach the running application, and the recorded run drives requests through it so the application's stdout reaches the derived log group (Smithy elasticbeanstalk EnvironmentDescription.CNAME).
Emulator: the CNAME is published as 'http://<env>-<id>.us-west-2.elasticbeanstalk.com/', whose host is not one of the emulator's own hosts — 'a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>'.
Why: the environment is unreachable for any client honouring the published CNAME, so streamed application logs cannot be produced or verified.
Fix: the CNAME/EndpointURL serializer in the elasticbeanstalk crate (publish the emulator's own host and port for the environment's listener).
Traces: elasticbeanstalk__logs.eb-stream-logs-delivers-the-application-stdout-to-the-derived-log-group.

## Evidence
'simparity.published.PublishedUrlRejected: endpoint_url was given http://sim2real-eb-log-env-<id>.us-west-2.elasticbeanstalk.com/ (published by the emulator or built by the check): its host is not one of the emulator's own hosts'.

## Steps to Reproduce
Replay elasticbeanstalk__logs.eb-stream-logs-delivers-the-application-stdout-to-the-derived-log-group: create the environment, read its CNAME and request it.

## Resolution
Cause 1 of 1: FIXED. The code base had DRIFTED since this report was filed: the CNAME/EndpointURL
serializer it asks for already publishes the emulator's own host and port. `state::environment_cname`
and `state::environment_endpoint_url` compose both values from the authority of a socket the emulator
itself opens (`state::environment_plane_endpoint::environment_plane_authority`, which opens a
core-owned plaintext listener and falls back to the emulator's own endpoint authority) plus this
service's own data-plane path, and `state::environment_launch` is the single place both launches
(`CreateEnvironment`, `ComposeEnvironments`) take them from; `service::environment_plane` serves what
is published. The name in Elastic Beanstalk's public zone stays where the API reports on a NAME
rather than on an address (`state::fully_qualified_cname`, read only by
`CheckDNSAvailability$FullyQualifiedCNAME`). Replaying this report's unit now answers `passed` with
this issue's key `fixed`.

The rule — an address an operation publishes for a client to dial is the `host:port` of one of the
emulator's own listeners followed by a path this emulator serves, never a production hostname — is
now PINNED by `tests/all/published_address_hosts.rs`: a table over the launches that mint an
environment's address (a supplied prefix with an application deployed, a generated prefix, another
Region, and a `ComposeEnvironments` group composed from manifests — the composition path no suite
covered before) asserts that neither published member sits in the public CNAME zone or any AWS
production domain, that both carry the loopback host and port of the one listener this service's
environment traffic is served on, that the published CNAME is never the `FullyQualifiedCNAME` the
same prefix forms, and that a client dialling the published port is answered by this emulator — with
the deployed bundle's own document where a version is deployed. `published_environment_name.rs` holds
the same addresses to their FORM.

Out of scope here: `DescribeEnvironmentResources$Queues[].URL` (`state::queue_url`) is an Amazon SQS
queue URL this service's stack record reports and nothing in this emulator provisions or serves; it
is not a cause of this report and is left to its own issue.
