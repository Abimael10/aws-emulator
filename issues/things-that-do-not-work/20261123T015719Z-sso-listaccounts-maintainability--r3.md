Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 56fc723d14e30db02a271205e6e68a837351c9c5

# can_handle hand-rolls the endpoint-host reading service_common::data_plane_addressing already owns (and gets the bracketed-IPv6 case wrong)

_Filed: 2026-11-23T01:57:19Z_
_Operation: ListAccounts | Class: maintainability_

> _Note: this issue was identified against the code base at commit `56fc723d14e30db02a271205e6e68a837351c9c5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`EmulatedService::can_handle` reads the endpoint host through a private `endpoint_host` helper that hand-rolls three readings `service_common::data_plane_addressing` already owns and that ~30 other services in this tree call: "the host the request named" (Host header, else the absolute-form request target's authority), userinfo stripping, and `:port` stripping. This crate already depends on that very module one line below, for `addresses_an_aws_endpoint`, so the copy is a second statement of a rule the tree keeps in one place.

The copies can — and already do — drift on a case the shared reading documents: the hand-rolled port strip is `split(':').next()`, which for a bracketed IPv6 authority (`[::1]:9297`) yields `"["`, while `host_only` keeps the brackets ("An IPv6 literal keeps its brackets, which is where its own colons live"). No response changes today, because neither `"["` nor `"[::1]"` is this service's endpoint or an AWS endpoint, so both readings land on the route-signature branch the existing test row pins — which is exactly what makes this a maintainability fix rather than a behaviour change.

Folding `endpoint_host` onto `service_common::data_plane_addressing` (the host reading, then `host_only`, then the existing `to_ascii_lowercase()` — which must stay, because the upper-case-host rows depend on it) leaves every response and every existing test unchanged and removes the second copy of the rule.

## Evidence

The hand-rolled copy — `services/service-com-amazonaws-sso-swbportalservice/src/service.rs:79-93`:
```rust
fn endpoint_host(parts: &http::request::Parts) -> Option<String> {
    let host = parts
        .headers
        .get(http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .or_else(|| parts.uri.authority().map(|a| a.as_str().to_string()))?;
    let without_userinfo = host.rsplit('@').next().unwrap_or(&host).to_string();
    let without_port = without_userinfo
        .split(':')
        .next()
        .unwrap_or(&without_userinfo)
        .to_ascii_lowercase();
    (!without_port.is_empty()).then_some(without_port)
}
```

The shared readings it restates — `services/common/src/data_plane_addressing.rs:58-70`:
```rust
/// A host with any userinfo and `:port` removed, which is the name a resource publishes.
/// An IPv6 literal keeps its brackets, which is where its own colons live.
pub fn host_only(host: &str) -> &str {
    let without_userinfo = host.rsplit('@').next().unwrap_or(host);
    if let Some(end) = without_userinfo.find(']') {
        return &without_userinfo[..=end];
    }
```
and `services/common/src/data_plane_addressing.rs:84-107`:
```rust
/// The host a request NAMED, as an owned value: the `Host` header it carries, and the
/// authority of an absolute-form request target for a request that carries its endpoint
/// there (HTTP/2, or an SDK-built request) and no header.
...
/// Both parts of the request are passed in rather than a whole request, because the
/// framework hands `Service::can_handle` the request's PARTS and the other seams a
/// runtime-API request: each has these two things and no common type.
pub fn addressed_host(uri: &str, host_header: Option<&str>) -> Option<String> {
```

The module is ALREADY this crate's dependency and is called from the next line of the same file — `src/service.rs:123-126`:
```rust
fn names_another_services_endpoint(host: &str) -> bool {
    !is_portal_endpoint(host)
        && ::service_common::data_plane_addressing::addresses_an_aws_endpoint(host)
}
```
with `Cargo.toml:34-37`:
```
# `can_handle`: the ONE reading of whether an endpoint host is one AWS itself publishes a
# service endpoint at, which is what tells another service's endpoint from an ordinary
# hostname an operator points a client at.
service-common = { path = "../common" }
```
Other services read the host through the shared functions from the same seam, e.g. `services/service-com-amazonaws-amplify-amplify/src/service.rs:621` and `services/service-com-amazonaws-mediapackagev2-mediapackagev2/src/service.rs:968` (`addressed_authority_of`), `services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/src/state/registry_address.rs:103` (`addressed_host`).

Tests that pin the behaviour the fix must not change: `tests/all/endpoint_host_claiming.rs:70-121` — the `ENDPOINTS` table, including `("a bracketed IPv6 loopback literal", Some("[::1]:9297"), Names::NoService)` at `:73`, `("a corporate DNS name in upper case", Some("EMULATOR.CORP.LOCAL:9297"), Names::NoService)` at `:79` and `("another service's endpoint in upper case", Some("S3.US-WEST-2.AMAZONAWS.COM"), Names::AnotherEndpoint)` at `:117-121` — plus `tests/all/endpoint_agnostic_routing.rs`, which carries one row per route.

## Steps to Reproduce

1. Read `services/service-com-amazonaws-sso-swbportalservice/src/service.rs:79-93` and `services/common/src/data_plane_addressing.rs:58-70` / `:84-107`: the same three readings (host the request named, userinfo strip, port strip) are stated twice, once per file.
2. Confirm the shared module is already linked and used by this crate: `Cargo.toml:34-37` and `src/service.rs:125` (`addresses_an_aws_endpoint`), so no new dependency is needed.
3. Observe the drift the duplication already carries: for `Host: [::1]:9297`, `endpoint_host` yields `"["` (`split(':').next()`), while `host_only("[::1]:9297")` yields `"[::1]"`. Both fall through `is_portal_endpoint` and `names_another_services_endpoint` to `serves_route`, which is the verdict `tests/all/endpoint_host_claiming.rs:73` pins (`Names::NoService`), so the response is identical either way.
4. Rewrite `endpoint_host` as the shared host reading followed by `data_plane_addressing::host_only` and the existing `to_ascii_lowercase()`/non-empty filter, and run `tests/all/endpoint_host_claiming.rs` and `tests/all/endpoint_agnostic_routing.rs`: every row must still pass unchanged.
