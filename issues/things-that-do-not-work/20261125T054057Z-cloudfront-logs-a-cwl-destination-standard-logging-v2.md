Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__logs.xsvc-cloudfront-3-standard-logging-v2-access-log-reaches-log-group
Issue-Key: cloudfront__logs.xsvc-cloudfront-3-standard-logging-v2-access-log-reaches-log-group|_poll_until#40|Timeout|time.sleep(interval_s)
Issue-Commit: 5dff3459c9fa0114d27ae1b22046b841878cac27

# cloudfront__logs: a CWL-destination standard-logging-v2 delivery never produces an access-log record, because the distribution's edge serves nothing

> _Note: this issue was identified against the code base at commit `5dff3459c9fa0114d27ae1b22046b841878cac27`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: a cloudfront ACCESS_LOGS delivery source delivered to a CloudWatch Logs log group never yields an access-log record, because the distribution's edge never serves the viewer request that would be logged. The check exhausts the whole 60 s harness budget with no verdict. Failing check: cloudfront__logs.xsvc-cloudfront-3-standard-logging-v2-access-log-reaches-log-group.

AWS: Smithy: CreateDistribution -> Distribution.DomainName ("The distribution's CloudFront domain name..."); CloudWatch Logs PutDeliverySource (service "cloudfront", logType ACCESS_LOGS, resourceArns = [the distribution ARN]), PutDeliveryDestination for a log-group ARN (deliveryDestinationType CWL) and CreateDelivery. A GET through the Deployed distribution's domain is answered by the edge with the origin's exact bytes, and that request produces one access-log event in the destination log group whose cs-uri-stem is the requested URI stem, whose sc-status is the status the edge answered (200), and whose x-host-header is this distribution's own DomainName. Recorded behavior only for the per-record field layout.

Emulator: the edge answers nothing. The check's edge fetch of the object key never returns 200, so no access-log record is ever written to the log group, and the check is killed by the 60 s cap ("Check timed out after 60.0s") before the record assertions run.

Why: a caller who wires CloudFront access logs into a log group sees an empty log group for traffic that was supposedly served — indistinguishable from "no requests". Request-level observability (URI stem, status, host header) is unavailable, and the subsequent contract that deleting the delivery stops delivery cannot be distinguished from delivery never having worked.

Fix: the CloudFront edge/viewer data plane for a Deployed distribution (serving the origin's bytes for a viewer GET), and the access-log vending path that must emit one record per answered request into the ACCESS_LOGS delivery's CWL destination with cs-uri-stem, sc-status and x-host-header populated from the served request.

Traces: cloudfront__logs.xsvc-cloudfront-3-standard-logging-v2-access-log-reaches-log-group ("Check timed out after 60.0s"). The sibling check in this service is owned by 20261025T174921Z-logs-cloudfront-cloudfront-vended-access-logs-are-never.md, which describes the same absence but does not carry this check's replay key — named here so the two can be reconciled rather than closed independently.

## Evidence
Expected (AWS): after put_delivery_source/put_delivery_destination (CWL)/create_delivery and Status == "Deployed", GET https://<Distribution.DomainName>/<key> returns 200 with the origin's exact bytes, and the destination log group then carries an access-log record with cs-uri-stem == "/<key>", sc-status == "200" and x-host-header == Distribution.DomainName.

Actual (emulator): the edge fetch never returns 200, so no bytes are served and the log group stays empty; the check is cut off with "Error: Check timed out after 60.0s" before reaching the cs-uri-stem / sc-status / x-host-header assertions.

## Steps to Reproduce
1. Create a distribution (Managed-CachingDisabled) over an origin serving a known object, and wait for Status == "Deployed".
2. In us-east-1: logs.put_delivery_source(service="cloudfront", logType="ACCESS_LOGS", resourceArn=<distribution ARN>); logs.put_delivery_destination for a log-group ARN (expect deliveryDestinationType CWL); logs.create_delivery.
3. GET https://<Distribution.DomainName>/<key> through the edge.
4. Observe the edge never answers 200 and the destination log group never receives an access-log record.
5. Re-replay check cloudfront__logs.xsvc-cloudfront-3-standard-logging-v2-access-log-reaches-log-group to confirm.
