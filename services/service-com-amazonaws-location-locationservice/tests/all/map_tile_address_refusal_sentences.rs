// SPDX-License-Identifier: CC0-1.0

//! The sentences a tile read refuses an out-of-pyramid ADDRESS with, end to end
//! over the wire.
//!
//! The rule these suites assert: a refusal of a tile address states the bound
//! the request missed in terms a caller can act on — a zoom past the ceiling
//! names the ceiling as the maximum zoom of the STYLE the addressed map stores,
//! with that style's name and its deepest zoom, and a column or row outside the
//! grid names the whole range of addresses the requested zoom admits
//! (`0..2^Zoom-1`) together with the pair of numbers that works out to — so a
//! client can learn the usable zoom range and the usable grid from the refusal
//! alone.
//!
//! The tables span that class rather than one recorded address: every published
//! style (so every style name and every ceiling the catalog holds), several
//! zooms past each ceiling, every zoom a grid rule is read at, both axis members,
//! and a column far past any address. Each sentence is checked against the
//! numbers the same map's own style descriptor declares, so the refusal and the
//! document a client renders from cannot drift apart.

use serde_json::Value;

use crate::harness::{call, call_bytes, create_map, emulator, published_styles, style_descriptor};

/// The sentence a `Z` past the ceiling the map's style publishes must be refused
/// with.
fn zoom_sentence(style: &str, max_zoom: u32) -> String {
    format!("'Z' must not be greater than maximum zoom of style {style} ({max_zoom}).")
}

/// The sentence a column or row outside the grid of `zoom` must be refused with:
/// that zoom's axis holds `2^Zoom` tiles, so its last address is `2^Zoom - 1`.
fn axis_sentence(member: &str, zoom: u32) -> String {
    let last = (1_u64 << zoom) - 1;
    format!("'{member}' must be an integer in the range 0..2^Zoom-1 (0..{last}).")
}

/// A refusal's `fieldList`, as the pairs of member name and sentence it carries.
fn field_list(case: &str, body: &Value) -> Vec<(String, String)> {
    body.get("fieldList")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a field refusal must carry a fieldList, got {body}"))
        .iter()
        .map(|entry| {
            (
                entry
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                entry
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The number a refusal states inside its parentheses, so the sentence a client
/// parses to discover the ceiling really yields that ceiling.
fn parenthesised(case: &str, sentence: &str) -> String {
    let (_, tail) = sentence
        .split_once('(')
        .unwrap_or_else(|| panic!("case {case:?}: the sentence must parenthesise its bound, got {sentence:?}"));
    let (inside, _) = tail
        .split_once(')')
        .unwrap_or_else(|| panic!("case {case:?}: the parenthesised bound must be closed, got {sentence:?}"));
    inside.to_string()
}

/// RULE rows: every style the service publishes, each at several zooms past its
/// own ceiling — so the sentence is read for every style name and every ceiling
/// the catalog holds, not for one of them.
#[tokio::test]
async fn a_zoom_refusal_names_the_maps_own_style_and_that_styles_ceiling() {
    let router = emulator();
    for (index, (style, _provider, max_zoom)) in published_styles().into_iter().enumerate() {
        let map = format!("ceiling.{index}_{style}");
        create_map(&router, &map, style).await;
        let expected = zoom_sentence(style, max_zoom);

        // The ceiling the sentence states is the one the map's OWN style
        // descriptor declares, read back through the document a client renders
        // from.
        let document = style_descriptor(&router, &map).await;
        let declared: Vec<u64> = document
            .get("sources")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("case {style:?}: the style must carry sources, got {document}"))
            .values()
            .filter_map(|source| source.get("maxzoom").and_then(Value::as_u64))
            .collect();
        assert!(
            !declared.is_empty() && declared.iter().all(|zoom| *zoom == u64::from(max_zoom)),
            "case {style:?}: the descriptor must declare maxzoom {max_zoom}, got {declared:?}"
        );

        // Rows: the first zoom past the ceiling, the next one, one well past it,
        // and a zoom no pyramid reaches.
        for zoom in [max_zoom + 1, max_zoom + 2, max_zoom + 11, 64] {
            let case = format!("{style} at zoom {zoom}");
            let refused = call(
                &router,
                "GET",
                &format!("/maps/v0/maps/{map}/tiles/{zoom}/0/0"),
                None,
            )
            .await;
            assert_eq!(
                refused.status, 400,
                "case {case:?}: expected 400, got {} {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_code(),
                "ValidationException",
                "case {case:?}: expected ValidationException, got {:?} ({:?})",
                refused.error_code(),
                refused.body
            );
            let message = refused.message();
            assert_eq!(
                message, expected,
                "case {case:?}: expected message {expected:?}, got {message:?}"
            );
            assert_eq!(
                parenthesised(&case, &message),
                max_zoom.to_string(),
                "case {case:?}: the parenthesised bound must be the declared maxzoom \
                 {max_zoom}, got {message:?}"
            );
            assert_eq!(
                field_list(&case, &refused.json()),
                vec![("Z".to_string(), expected.clone())],
                "case {case:?}: fieldList must name Z and repeat the sentence, got {:?}",
                refused.body
            );
        }
    }
}

/// RULE rows: one zoom read against two maps whose styles publish different
/// ceilings — the ceiling follows the addressed MAP's stored style, so the same
/// zoom is served by one map and refused by the other, and the refusal names the
/// style that refused it.
#[tokio::test]
async fn the_ceiling_a_refusal_states_follows_the_addressed_maps_stored_style() {
    let router = emulator();
    let catalog = published_styles();
    let shallowest = catalog
        .iter()
        .copied()
        .min_by_key(|(_, _, max_zoom)| *max_zoom)
        .expect("the catalog publishes styles");
    let deepest = catalog
        .iter()
        .copied()
        .max_by_key(|(_, _, max_zoom)| *max_zoom)
        .expect("the catalog publishes styles");
    assert!(
        shallowest.2 < deepest.2,
        "the catalog must publish styles of differing depth, got {} and {}",
        shallowest.2,
        deepest.2
    );

    create_map(&router, "ShallowStyleMap", shallowest.0).await;
    create_map(&router, "DeepStyleMap", deepest.0).await;

    // A zoom inside the deeper style's pyramid and outside the shallower one's.
    let zoom = shallowest.2 + 1;
    let case = format!("zoom {zoom}");

    let refused = call(
        &router,
        "GET",
        &format!("/maps/v0/maps/ShallowStyleMap/tiles/{zoom}/0/0"),
        None,
    )
    .await;
    let expected = zoom_sentence(shallowest.0, shallowest.2);
    assert_eq!(
        refused.status, 400,
        "case {case:?}: the shallower style must refuse it, got {}",
        refused.status
    );
    assert_eq!(
        refused.message(),
        expected,
        "case {case:?}: expected message {expected:?}, got {:?}",
        refused.message()
    );

    let served = call_bytes(
        &router,
        "GET",
        &format!("/maps/v0/maps/DeepStyleMap/tiles/{zoom}/0/0"),
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: the deeper style must serve it, got {} {:?}",
        served.status,
        String::from_utf8_lossy(&served.body)
    );
}

/// RULE rows: every zoom whose grid is read, each for both axis members, with
/// the first address outside the grid and one far outside it — the sentence
/// states the range the requested zoom admits, which doubles with the zoom.
#[tokio::test]
async fn an_axis_refusal_states_the_whole_range_the_requested_zoom_admits() {
    let router = emulator();
    // A style published deep enough that every zoom row below is inside its own
    // pyramid, so each row exercises the GRID rule rather than the ceiling.
    let (style, _provider, max_zoom) = published_styles()
        .into_iter()
        .max_by_key(|(_, _, max_zoom)| *max_zoom)
        .expect("the catalog publishes styles");
    create_map(&router, "GridRangeMap", style).await;

    // Rows: the world zoom (a one-tile grid), the first doubling, a small grid,
    // a middle zoom, and the deepest zoom this style publishes.
    for zoom in [0_u32, 1, 3, 10, max_zoom] {
        let axis = 1_u64 << zoom;
        for member in ["X", "Y"] {
            let expected = axis_sentence(member, zoom);
            // The first address outside the grid, one further out, and a label
            // of more digits than any address can hold.
            for value in [axis.to_string(), (axis + 7).to_string(), "9".repeat(30)] {
                let case = format!("{member}={value} at zoom {zoom}");
                let (x, y) = match member {
                    "X" => (value.clone(), "0".to_string()),
                    _ => ("0".to_string(), value.clone()),
                };
                let refused = call(
                    &router,
                    "GET",
                    &format!("/maps/v0/maps/GridRangeMap/tiles/{zoom}/{x}/{y}"),
                    None,
                )
                .await;
                assert_eq!(
                    refused.status, 400,
                    "case {case:?}: expected 400, got {} {:?}",
                    refused.status, refused.body
                );
                assert_eq!(
                    refused.error_code(),
                    "ValidationException",
                    "case {case:?}: expected ValidationException, got {:?} ({:?})",
                    refused.error_code(),
                    refused.body
                );
                let message = refused.message();
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected message {expected:?}, got {message:?}"
                );
                assert_eq!(
                    parenthesised(&case, &message),
                    format!("0..{}", axis - 1),
                    "case {case:?}: the parenthesised range must be the grid of this zoom, got \
                     {message:?}"
                );
                assert_eq!(
                    field_list(&case, &refused.json()),
                    vec![(member.to_string(), expected.clone())],
                    "case {case:?}: fieldList must name {member} and repeat the sentence, got {:?}",
                    refused.body
                );
            }

            // The top of the range the sentence names really is addressable, so
            // the range it states is the usable one rather than one off it.
            let last = axis - 1;
            let (x, y) = match member {
                "X" => (last, 0),
                _ => (0, last),
            };
            let case = format!("{member}={last} at zoom {zoom}");
            let served = call_bytes(
                &router,
                "GET",
                &format!("/maps/v0/maps/GridRangeMap/tiles/{zoom}/{x}/{y}"),
            )
            .await;
            assert_eq!(
                served.status, 200,
                "case {case:?}: the top of the stated range must be served, got {} {:?}",
                served.status,
                String::from_utf8_lossy(&served.body)
            );
        }
    }
}
