Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__servicediscovery.ecs-srv-service-registry-without-a-port-rejected
Issue-Key: ecs__servicediscovery.ecs-srv-service-registry-without-a-port-rejected|_poll_until#18|Timeout|last = probe()
Issue-Commit: f520e482964e34c3af4b1cbe916bb0f71c86032d

# ecs__servicediscovery: CreateService with a serviceRegistry SRV registration and no port never returns its refusal

> _Note: this issue was identified against the code base at commit `f520e482964e34c3af4b1cbe916bb0f71c86032d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
Cause 1 of 1: the no-port SRV service-registry refusal is never produced.
AWS: Smithy (ecs): CreateService.serviceRegistries entries carry registryArn plus optional containerName/containerPort/port; when the Cloud Map service uses an SRV record, ECS requires a port source and refuses the create with InvalidParameterException naming the missing containerName/containerPort (or port) — a prompt, synchronous refusal.
Emulator: recorded behavior only — the check exhausts its 60s budget with no verdict, so neither the refusal nor an (incorrect) success is observable for this call.
Why: a client that misconfigures an SRV registration gets a hang instead of the modelled InvalidParameterException and cannot learn which member is missing.
Fix: the CreateService handler's serviceRegistries validation (the SRV port-source rule) and its error path.
Traces: ecs__servicediscovery.ecs-srv-service-registry-without-a-port-rejected.

## Evidence
Error: Check timed out after 60.0s — no verdict. Expected CreateService to be refused with InvalidParameterException naming the missing port source for an SRV Cloud Map registration; no response was observed.

## Steps to Reproduce
1. CreatePrivateDnsNamespace and a Cloud Map service with an SRV record type.
2. CreateCluster + RegisterTaskDefinition (awsvpc).
3. CreateService with serviceRegistries=[{registryArn: <srv service arn>}] and no containerName/containerPort/port: the call never returns its refusal inside the budget.
