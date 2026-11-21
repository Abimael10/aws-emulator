Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: d7cb5d802aaf50f8ef4cbec9c9028bf6415c27df

# StartApplicationRefresh settles DiscoveryStatus to SUCCESS without running the crate's discovery hop, so a re-discovery of an instance EC2 reports as not running is reported successful

_Filed: 2026-11-21T03:43:20Z_
_Operation: StartApplicationRefresh | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `d7cb5d802aaf50f8ef4cbec9c9028bf6415c27df`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

A refresh is, by this handler's own words, a re-run of the application's DISCOVERY. The crate owns exactly one implementation of that discovery — `crate::state::discovery::run_document`, which asks EC2 whether a document can be run on each registered instance and yields a refusal when it cannot — and `RegisterApplication` calls it and settles the application to FAILED/REGISTRATION_FAILED (with the refusal on its operation record) when it answers one. `StartApplicationRefresh` never calls it: it hardcodes `DISCOVERY_STATUS_SUCCESS` as the owed result and files its operation record through `record_operation` (no refusal channel), so the re-discovery always reports SUCCESS.

The handler justifies this with "The emulator discovers nothing on the registered instances", a premise the crate contradicts: discovery.rs exists precisely to ask EC2, and `RegisterApplication` awaits it. The same crate also provides the route that makes the premise observably wrong: `StopApplication` with `IncludeEc2InstanceShutdown=true` asks EC2 to stop the application's instances (`crate::state::ec2_shutdown::stop_instances`). After that stop, a registration naming the same instance converges to FAILED/REGISTRATION_FAILED ("the Amazon EC2 instance of an SAP registration is in no state to run the discovery document"), while a refresh of the already-registered application reports DiscoveryStatus SUCCESS and an operation record in SUCCESS. Two paths in one crate answer the same discovery question two ways.

## Evidence

Register runs the hop and lets its answer decide the outcome — services/service-com-amazonaws-ssmsap-ssmsap/src/ops/register_application.rs:189-196:
```
// Registering an application DISCOVERS it: the service runs a Systems Manager
// document on the instances the request named, and what that run can do is what
// the registration settles as.
let discovery_refusal =
    crate::state::discovery::run_document(ctx, &region, &input.instances).await;
```
and register_application.rs:424-433 settles to `APPLICATION_STATUS_FAILED` / `DISCOVERY_STATUS_REGISTRATION_FAILED` when `discovery_refusal.is_some()`, filing it via `record_operation_outcome(..., discovery_refusal)` (register_application.rs:463-469).

The refresh never asks — services/service-com-amazonaws-ssmsap-ssmsap/src/ops/start_application_refresh.rs:196-215:
```
// A refresh re-runs the application's DISCOVERY, which is what
// `Application$DiscoveryStatus` reports the latest result of: ...
            // The emulator discovers nothing on the registered instances, so what a
            // refresh re-discovers is the registration this store already holds: the
            // discovery the clock owes this application succeeds, exactly as the one
            // its registration started did.
            discovery_status: crate::state::DISCOVERY_STATUS_SUCCESS.to_string(),
```
and start_application_refresh.rs:220-227 files the record with `record_operation` (no refusal), so it always reaches SUCCESS.

The premise is contradicted by the crate's own discovery module — services/service-com-amazonaws-ssmsap-ssmsap/src/state/discovery.rs:12-18:
```
//! An instance is EC2's resource, not this service's, so whether a document can be run on
//! one is EC2's to answer: this asks it, over the one client every hop into EC2 goes
//! through ([`super::ec2::client`]), and reports success only for an instance EC2 itself
//! answers for and reports running.
```
and discovery.rs:90-100 returns `document_refusal(...)` for an instance EC2 reports in any state other than `Running`.

The in-crate route that stops the instance — services/service-com-amazonaws-ssmsap-ssmsap/src/ops/stop_application.rs:187-188:
```
let asked =
    crate::state::ec2_shutdown::stop_instances(ctx, &region, &instances_to_shut_down).await;
```

Test coverage: the refresh suite (tests/all/application_refresh.rs:286-389) asserts DiscoveryStatus SUCCESS, but it runs on `Harness::new()` (no EC2 mounted beside the service), where `run_document` observes nothing and yields `None` anyway — only tests/all/ec2_instance_shutdown.rs uses `Harness::with_ec2()` (tests/all/application_lifecycle.rs:130). No test exercises a refresh with EC2 mounted, so nothing pins the hardcoded SUCCESS.

## Steps to Reproduce

1. Mount EC2 beside this service on one engine (the `Harness::with_ec2()` seam, tests/all/application_lifecycle.rs:130) and launch an instance so EC2 reports it `running`.
2. POST /register-application naming that instance; wait past TRANSITION_SPAN and confirm GetApplication reports Status ACTIVATED and DiscoveryStatus SUCCESS.
3. POST /stop-application with `IncludeEc2InstanceShutdown: true` for that application, so this crate asks EC2 to stop the instance; wait past TRANSITION_SPAN (EC2 now reports the instance `stopped`, not `running`).
4. POST /start-application and wait past TRANSITION_SPAN so the application has no pending change.
5. POST /start-application-refresh, wait past TRANSITION_SPAN, then POST /get-application: observe DiscoveryStatus SUCCESS and GetOperation for the refresh id in SUCCESS.
6. For contrast, POST /register-application for a NEW ApplicationId naming that same stopped instance: the registration settles FAILED / REGISTRATION_FAILED with the `document_refusal` text on its operation record — the same discovery question, answered the opposite way.
