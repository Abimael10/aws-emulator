// SPDX-License-Identifier: CC0-1.0

//! What this module's pure readings make of a record and a path: which base path mapping
//! answers a request, which host addresses a record, and the path within the api a mapping
//! selects.
//!
//! The wire behaviour — a request on the address a regional custom domain name publishes,
//! served by the stage its mapping names — is driven end to end over a real socket in
//! `tests/all/regional_custom_domain_front_door.rs` and
//! `tests/all/custom_domain_arrival.rs`.

use super::*;

use crate::state::{BasePathMappingEntry, DomainNameEntry, NONE_BASE_PATH};

/// One row of the base-path table: the request path, and the api, stage and remaining path
/// it reaches (`None` for a path no mapping answers).
type MatchRow<'a> = (&'a str, Option<(&'a str, Option<&'a str>, &'a str)>);

/// A record carrying one mapping per `(base path, api, stage)` row.
fn record_with(mappings: &[(&str, &str, Option<&str>)]) -> DomainNameEntry {
    let mut record = DomainNameEntry {
        domain_name: "api.example.com".to_string(),
        ..DomainNameEntry::default()
    };
    for (sequence, (base_path, api, stage)) in mappings.iter().enumerate() {
        record.base_path_mappings.insert(
            (*base_path).to_string(),
            BasePathMappingEntry {
                base_path: (*base_path).to_string(),
                rest_api_id: (*api).to_string(),
                stage: stage.map(str::to_string),
                sequence: sequence as u64,
            },
        );
    }
    record
}

/// RULE: the longest base path that is a prefix of the request path ON A SEGMENT BOUNDARY
/// serves it, the `(none)` mapping serves every path no other mapping claims, and a path no
/// mapping answers to reaches no api.
#[test]
fn the_longest_base_path_on_a_segment_boundary_serves_the_path() {
    let record = record_with(&[
        ("shop", "api-shop", Some("live")),
        ("shop/admin", "api-admin", Some("beta")),
        ("stageless", "api-stageless", None),
    ]);
    // The rows span what the match generalises over: an exact base path, a path under it, a
    // longer base path winning over a shorter prefix, a segment that merely starts with a
    // base path, a mapping that names no stage, a path carrying a relative segment in either
    // spelling, and a path no mapping claims.
    let cases: &[MatchRow<'_>] = &[
        ("/shop", Some(("api-shop", Some("live"), ""))),
        ("/shop/items/9", Some(("api-shop", Some("live"), "items/9"))),
        ("/shop/admin/users", Some(("api-admin", Some("beta"), "users"))),
        ("/shopping", None),
        ("/stageless/live/x", Some(("api-stageless", None, "live/x"))),
        ("/shop/./items", None),
        ("/shop/../shop/items", None),
        ("/shop/%2e%2e/items", None),
        ("/shop/%2E./items", None),
        ("/", None),
        // A path naming a plane of this emulator rather than one a mapping answers: the
        // reserved data-plane namespace, and this service's own control-plane path space.
        ("/_aws/execute-api/other/live/x", None),
        ("/restapis", None),
    ];
    for (path, expected) in cases {
        let matched = matching_mapping(&record, path).map(|(mapping, remainder)| {
            (
                mapping.rest_api_id.clone(),
                mapping.stage.clone(),
                remainder.to_string(),
            )
        });
        let rendered = matched
            .as_ref()
            .map(|(api, stage, remainder)| (api.as_str(), stage.as_deref(), remainder.as_str()));
        assert_eq!(
            rendered, *expected,
            "case {path:?}: expected {expected:?}, got {rendered:?}"
        );
        assert_eq!(
            reaches_a_mapping(&record, path),
            expected.is_some(),
            "case {path:?}: the socket's own reading must agree with the plane's"
        );
    }
}

/// RULE: the `(none)` mapping answers every path no other mapping claims, whole.
#[test]
fn the_none_mapping_answers_every_other_path() {
    let record = record_with(&[
        (NONE_BASE_PATH, "api-root", Some("live")),
        ("shop", "api-shop", Some("live")),
    ]);
    let cases: &[(&str, &str, &str)] = &[
        ("/", "api-root", ""),
        ("/pets", "api-root", "pets"),
        ("/pets/9", "api-root", "pets/9"),
        ("/shop/items", "api-shop", "items"),
    ];
    for (path, api, remainder) in cases {
        let matched = matching_mapping(&record, path);
        let rendered =
            matched.map(|(mapping, left)| (mapping.rest_api_id.clone(), left.to_string()));
        assert_eq!(
            rendered
                .as_ref()
                .map(|(api, left)| (api.as_str(), left.as_str())),
            Some((*api, *remainder)),
            "case {path:?}: expected {api} and {remainder:?}, got {rendered:?}"
        );
    }
}

/// RULE: the path a mapping selects within the api is `/{stage}/{path under the base path}`
/// for a mapping that names a stage, and the path under the base path alone for one that
/// names none — where the first segment is the stage, as on the api's own endpoint.
#[test]
fn the_selected_invoke_path_carries_the_stage_the_mapping_names() {
    let record = record_with(&[
        ("shop", "api-shop", Some("live")),
        ("stageless", "api-stageless", None),
    ]);
    // The rows span what the composition generalises over: a mapping with and without a
    // stage, and a remainder that is empty, one segment and several. A path that IS the base
    // path carries the stage alone, which is the path a client of the api's own endpoint
    // sends for a stage's root.
    let cases: &[(&str, &str)] = &[
        ("/shop", "/live"),
        ("/shop/items", "/live/items"),
        ("/shop/items/9", "/live/items/9"),
        ("/stageless", "/"),
        ("/stageless/live/items", "/live/items"),
    ];
    for (path, expected) in cases {
        let (mapping, remainder) = matching_mapping(&record, path)
            .unwrap_or_else(|| panic!("case {path:?}: a mapping must answer it"));
        let composed = invoke_path(mapping, remainder);
        assert_eq!(
            composed, *expected,
            "case {path:?}: expected {expected:?}, got {composed:?}"
        );
    }
}

/// RULE: a request addresses a custom domain name record by the NAME itself, however cased
/// and whatever port it states, or by the `host:port` that record published as its
/// `regionalDomainName`; a host that names neither addresses no record, and neither does the
/// name of a record served by another endpoint type.
#[test]
fn a_record_is_addressed_by_its_name_or_by_the_address_it_published() {
    let mut store = ApiGatewayState::default();
    let mut regional = record_with(&[(NONE_BASE_PATH, "api-root", Some("live"))]);
    regional.domain_name = "shop.example.com".to_string();
    regional.endpoint_configuration = Some(crate::ops::types::EndpointConfiguration {
        types: Some(vec![crate::state::REGIONAL_ENDPOINT_TYPE.to_string()]),
        ..Default::default()
    });
    regional.regional_domain_name = Some("127.0.0.1:34567".to_string());
    store
        .domain_names
        .insert(regional.domain_name.clone(), regional);
    let mut edge = record_with(&[(NONE_BASE_PATH, "api-edge", Some("live"))]);
    edge.domain_name = "edge.example.com".to_string();
    edge.endpoint_configuration = Some(crate::ops::types::EndpointConfiguration {
        types: Some(vec!["EDGE".to_string()]),
        ..Default::default()
    });
    store.domain_names.insert(edge.domain_name.clone(), edge);

    // The rows span what the addressing generalises over: the name, the name upper-cased,
    // the name with a port, the published address, a host naming no record, an
    // edge-optimized record's own name, and no host at all.
    let cases: &[(&str, Option<&str>, Option<&str>)] = &[
        ("the name itself", Some("shop.example.com"), Some("api-root")),
        ("the name upper-cased", Some("SHOP.Example.COM"), Some("api-root")),
        ("the name with a port", Some("shop.example.com:443"), Some("api-root")),
        ("the published address", Some("127.0.0.1:34567"), Some("api-root")),
        ("a host naming no record", Some("other.example.com"), None),
        ("an edge-optimized record's name", Some("edge.example.com"), None),
        ("no host at all", None, None),
    ];
    for (case, host, expected) in cases {
        let reached = arrival(&store, *host, "/pets").map(|arrival| arrival.api_id.to_string());
        assert_eq!(
            reached.as_deref(),
            *expected,
            "case {case:?}: expected {expected:?}, got {reached:?}"
        );
    }
}
