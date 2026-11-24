// SPDX-License-Identifier: CC0-1.0

//! The PUBLISHED style asset `GetMapStyleDescriptor` serves, end to end over the
//! wire.
//!
//! Every call goes through the shared harness (`crate::harness`), so each one is
//! a separate HTTP request against one shared engine.
//!
//! The rule these suites assert: the style descriptor of a map resource is the
//! published style asset of the style that map was created with, served from the
//! service's stored catalog rather than composed per request — each of its
//! layers carries the published `id` of the `<Source layer>/<sub-class>` form,
//! the published `source-layer` as the tile schema spells it, the published
//! `type`, the `filter` that selects the sub-class it paints, a `layout`, a
//! `paint` and a `minzoom`; and the document's `created` and `modified` are the
//! asset's own fixed instants rather than any instant of the serving clock or of
//! the map resource that was read.
//!
//! The tables span what that rule generalises over: every style the catalog
//! publishes (so every data provider and both tile formats), several map
//! resources of one style, repeat reads, and a read taken after the map resource
//! itself was written. The one layer a recording pins is asserted verbatim, and
//! the rest are asserted against the published FORM and against the tiles they
//! paint.

use serde_json::{json, Value};

use service_com_amazonaws_location_locationservice::state;

use crate::harness::{
    call, call_bytes, create_map, emulator, land_tile, published_styles, raster_basemap,
    style_descriptor, water_tile,
};

/// The instant a published style asset reports as both its `created` and its
/// `modified`.
///
/// Source: recorded behaviour of `GetMapStyleDescriptor` — the document AWS
/// serves carries the zero-value timestamp in both members, a FIXED constant of
/// the published asset, identical on every call.
const PUBLISHED_STYLE_INSTANT: &str = "0001-01-01T00:00:00Z";

/// The source id an Esri-backed map's descriptor declares its tiles under.
///
/// Source: recorded behaviour of `GetMapStyleDescriptor` — the descriptor of an
/// Esri-backed map declares its one source as `esri`, and every published layer
/// of it reads that source.
const ESRI_SOURCE_ID: &str = "esri";

/// The layer an Esri-backed vector basemap's published style paints FIRST, as
/// the published document spells it.
///
/// Source: recorded behaviour of `GetMapStyleDescriptor`, verbatim: the ground
/// layer of the published Esri style, painting the `Land` source layer's first
/// sub-class. It is the one published layer a recording pins, so it is written
/// here as a whole object — every other layer is asserted against the published
/// form instead.
fn published_esri_ground_layer() -> Value {
    json!({
        "filter": ["==", "_symbol", 0],
        "id": "Land/Not ice",
        "layout": {},
        "minzoom": 0,
        "paint": { "fill-color": "#f7f6d5" },
        "source": ESRI_SOURCE_ID,
        "source-layer": "Land",
        "type": "fill",
    })
}

/// Every layer of a descriptor, as objects.
fn layers_of(case: &str, document: &Value) -> Vec<Value> {
    document
        .get("layers")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: the style must carry layers, got {document}"))
        .clone()
}

/// The one source id a descriptor declares its tiles under.
fn declared_source(case: &str, document: &Value) -> String {
    let sources = document
        .get("sources")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("case {case:?}: the style must carry sources, got {document}"));
    let mut ids: Vec<String> = sources.keys().cloned().collect();
    assert_eq!(
        ids.len(),
        1,
        "case {case:?}: the style declares the map's own tiles as its one source, got {ids:?}"
    );
    ids.remove(0)
}

/// One member of a document as a string, or a panic naming the case.
fn string_member(case: &str, document: &Value, member: &str) -> String {
    document
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the style must carry {member} as a string, got {document}")
        })
        .to_string()
}

/// Every style the catalog publishes whose basemap is drawn from vector data,
/// with the provider it is sourced from.
fn vector_styles() -> Vec<(&'static str, &'static str)> {
    published_styles()
        .into_iter()
        .filter(|(style, _, _)| !raster_basemap(style))
        .map(|(style, provider, _)| (style, provider))
        .collect()
}

#[tokio::test]
async fn every_esri_vector_style_publishes_the_recorded_ground_layer_first() {
    let router = emulator();
    // Rows: every Esri-backed vector style the catalog publishes — the street,
    // navigation and topographic basemaps and both canvas styles are tiles of
    // one published vector basemap family, so the recorded ground layer covers
    // them all. Two maps per style, under names of different shapes, because the
    // layer belongs to the published ASSET and not to the resource that reads
    // it.
    let rows: Vec<&str> = vector_styles()
        .into_iter()
        .filter(|(_, provider)| *provider == "Esri")
        .map(|(style, _)| style)
        .collect();
    assert!(
        rows.len() > 1,
        "the catalog must publish more than one Esri vector style for this rule to be read over \
         a family, got {rows:?}"
    );
    let expected = published_esri_ground_layer();
    for (index, style) in rows.into_iter().enumerate() {
        for map in [format!("ground.{index}_{style}"), format!("G-{index}")] {
            create_map(&router, &map, style).await;
            let case = format!("{style} as {map}");
            let document = style_descriptor(&router, &map).await;
            let layers = layers_of(&case, &document);
            let first = layers
                .first()
                .unwrap_or_else(|| panic!("case {case:?}: the style must paint a layer"));
            assert_eq!(
                first, &expected,
                "case {case:?}: the first published layer must be the published ground layer \
                 {expected}, got {first}"
            );
        }
    }
}

#[tokio::test]
async fn every_published_layer_carries_the_members_the_published_form_sets() {
    let router = emulator();
    // Rows: every vector style the catalog publishes, so the form is read for
    // every provider's catalog rather than for the one a recording covers.
    for (index, (style, provider)) in vector_styles().into_iter().enumerate() {
        let map = format!("form.{index}_{style}");
        create_map(&router, &map, style).await;
        let case = format!("{style} of {provider}");
        let document = style_descriptor(&router, &map).await;
        let source = declared_source(&case, &document);
        if provider == "Esri" {
            assert_eq!(
                source, ESRI_SOURCE_ID,
                "case {case:?}: an Esri-backed map declares its source as {ESRI_SOURCE_ID:?}, \
                 got {source:?}"
            );
        }
        let layers = layers_of(&case, &document);
        assert!(
            layers.len() >= state::map_tiles::tile_layers().len(),
            "case {case:?}: the asset paints at least one layer per source layer its tiles \
             carry, got {} layers",
            layers.len()
        );

        for layer in &layers {
            let id = layer
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: every layer carries an id, got {layer}"));
            let source_layer = layer
                .get("source-layer")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: layer {id} must name the tile layer it paints: {layer}")
                });
            assert_eq!(
                layer.get("source").and_then(Value::as_str),
                Some(source.as_str()),
                "case {case:?}: layer {id} must read the declared source {source:?}, got {layer}"
            );

            // The source layer is one the tiles really carry, and the id is the
            // published `<Source layer>/<sub-class>` name of a class that layer
            // tags its features with.
            let carried = state::map_tiles::tile_layers()
                .iter()
                .find(|entry| entry.name == source_layer)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: layer {id} paints source-layer {source_layer:?}, which \
                         the tiles do not carry"
                    )
                });
            let class = carried
                .classes
                .iter()
                .find(|class| class.id == id)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: layer {id} is no published class of {source_layer:?}, \
                         which publishes {:?}",
                        carried
                            .classes
                            .iter()
                            .map(|class| class.id)
                            .collect::<Vec<_>>()
                    )
                });
            assert!(
                id.starts_with(&format!("{source_layer}/")),
                "case {case:?}: the published id {id:?} must name the source layer it paints \
                 first, got {layer}"
            );

            // The filter selects exactly that class, out of the attribute the
            // tiles tag it under.
            assert_eq!(
                layer.get("filter"),
                Some(&json!(["==", "_symbol", class.symbol])),
                "case {case:?}: layer {id} must filter to its own published class, got {layer}"
            );

            // Layout is present (possibly empty) and paint carries the
            // properties of the layer's own type.
            assert!(
                layer.get("layout").and_then(Value::as_object).is_some(),
                "case {case:?}: layer {id} must carry a layout object, got {layer}"
            );
            let kind = layer
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: layer {id} must carry a type: {layer}"));
            assert!(
                ["fill", "line", "circle"].contains(&kind),
                "case {case:?}: layer {id} must be drawn as one of the published kinds, got \
                 {kind:?}"
            );
            let paint = layer
                .get("paint")
                .and_then(Value::as_object)
                .unwrap_or_else(|| panic!("case {case:?}: layer {id} must carry paint: {layer}"));
            assert!(
                !paint.is_empty()
                    && paint
                        .keys()
                        .all(|property| property.starts_with(&format!("{kind}-"))),
                "case {case:?}: layer {id}'s paint must set properties of a {kind} layer, got \
                 {paint:?}"
            );
            assert_eq!(
                layer.get("minzoom").and_then(Value::as_u64),
                Some(u64::from(carried.min_zoom)),
                "case {case:?}: layer {id} must declare the lowest zoom its source layer is \
                 drawn at ({}), got {layer}",
                carried.min_zoom
            );
        }
    }
}

#[tokio::test]
async fn a_raster_styles_published_asset_paints_the_imagery_itself() {
    let router = emulator();
    // Rows: every style the catalog publishes whose basemap is images — there
    // are no feature classes to select out of such a tile, so its published
    // asset paints the raster source itself.
    let rows: Vec<&str> = published_styles()
        .into_iter()
        .filter(|(style, _, _)| raster_basemap(style))
        .map(|(style, _, _)| style)
        .collect();
    assert!(
        !rows.is_empty(),
        "the catalog must publish a raster basemap for this rule to be read"
    );
    for (index, style) in rows.into_iter().enumerate() {
        let map = format!("raster.{index}_{style}");
        create_map(&router, &map, style).await;
        let case = format!("{style} as {map}");
        let document = style_descriptor(&router, &map).await;
        let source = declared_source(&case, &document);
        let layers = layers_of(&case, &document);
        assert_eq!(
            layers.len(),
            1,
            "case {case:?}: a raster basemap's asset paints its imagery as one layer, got \
             {layers:?}"
        );
        let layer = &layers[0];
        assert_eq!(
            layer.get("type").and_then(Value::as_str),
            Some("raster"),
            "case {case:?}: the layer must draw the raster source, got {layer}"
        );
        assert_eq!(
            layer.get("source").and_then(Value::as_str),
            Some(source.as_str()),
            "case {case:?}: the layer must read the declared source {source:?}, got {layer}"
        );
        assert!(
            layer.get("source-layer").is_none() && layer.get("filter").is_none(),
            "case {case:?}: an image has no feature classes to select, got {layer}"
        );
        assert!(
            layer.get("layout").and_then(Value::as_object).is_some(),
            "case {case:?}: the layer must carry a layout object, got {layer}"
        );
    }
}

#[tokio::test]
async fn the_class_a_published_layer_filters_on_is_a_class_its_tiles_tag() {
    let router = emulator();
    // Rows: one vector style per data provider, each read over a tile whose
    // ground is land and a tile whose ground is water — between them every
    // source layer the published schema carries is encoded — at the deepest zoom
    // any source layer comes in at, so every layer of the schema is drawn.
    let deepest_threshold = state::map_tiles::tile_layers()
        .iter()
        .map(|layer| layer.min_zoom)
        .max()
        .expect("the published schema carries source layers");
    let addresses = [land_tile(deepest_threshold), water_tile(deepest_threshold)];
    // Every source layer the schema publishes must be read through one of the
    // two addresses, so no published layer goes unexercised.
    let mut covered: Vec<String> = Vec::new();
    for (index, (style, provider)) in vector_styles().into_iter().enumerate() {
        let map = format!("tagged.{index}_{style}");
        create_map(&router, &map, style).await;
        let document = style_descriptor(&router, &map).await;
        let layers = layers_of(style, &document);

        for address in addresses {
            let case = format!("{style} of {provider} at {address:?}");
            let served = call_bytes(
                &router,
                "GET",
                &format!(
                    "/maps/v0/maps/{map}/tiles/{}/{}/{}",
                    address.z, address.x, address.y
                ),
            )
            .await;
            assert_eq!(
                served.status, 200,
                "case {case:?}: the tile must be served, got {}",
                served.status
            );
            let tile = state::vector_tile::decode_tile(&served.body).unwrap_or_else(|error| {
                panic!("case {case:?}: the tile must decode as a Mapbox Vector Tile, got {error}")
            });

            for layer in &layers {
                let id = layer.get("id").and_then(Value::as_str).unwrap_or_default();
                let source_layer = layer
                    .get("source-layer")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let selected = layer
                    .get("filter")
                    .and_then(Value::as_array)
                    .and_then(|filter| filter.get(2))
                    .and_then(Value::as_i64)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: layer {id} must filter on a class, got {layer}")
                    });
                // A source layer is encoded only over the ground it belongs to,
                // so a layer this tile does not carry is read at the other
                // address.
                let Some(encoded) = tile.layers.iter().find(|held| held.name == source_layer)
                else {
                    continue;
                };
                if !covered.iter().any(|name| name == source_layer) {
                    covered.push(source_layer.to_string());
                }
                // The classes this tile layer really tags its features with,
                // read out of the layer's own interned key and value tables.
                let tagged: Vec<i64> = encoded
                    .features
                    .iter()
                    .flat_map(|feature| {
                        feature.tags.chunks(2).filter_map(|pair| {
                            let key = encoded.keys.get(*pair.first()? as usize)?;
                            if key != "_symbol" {
                                return None;
                            }
                            encoded.values.get(*pair.get(1)? as usize)?.int_value
                        })
                    })
                    .collect();
                assert!(
                    tagged.contains(&selected),
                    "case {case:?}: layer {id} filters to class {selected}, which no feature of \
                     {source_layer:?} is tagged with: {tagged:?}"
                );
            }
        }
    }
    for layer in state::map_tiles::tile_layers() {
        assert!(
            covered.iter().any(|name| name == layer.name),
            "source layer {:?} was carried by neither address, so its published layers were not \
             read: {covered:?}",
            layer.name
        );
    }
}

#[tokio::test]
async fn the_published_stamps_belong_to_the_asset_and_not_to_the_resource() {
    let router = emulator();
    // Rows: every style the catalog publishes, so the stamps are read for every
    // provider and both tile formats; each one is read twice, with the map
    // resource's own instants read in between, because the asset's instants are
    // a property of the ASSET.
    for (index, (style, provider, _)) in published_styles().into_iter().enumerate() {
        let map = format!("stamped.{index}_{style}");
        create_map(&router, &map, style).await;
        let case = format!("{style} of {provider}");
        let document = style_descriptor(&router, &map).await;
        let described = call(&router, "GET", &format!("/maps/v0/maps/{map}"), None).await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: the map must be readable, got {} {:?}",
            described.status, described.body
        );
        let described = described.json();
        for member in ["created", "modified"] {
            let stamp = string_member(&case, &document, member);
            assert_eq!(
                stamp, PUBLISHED_STYLE_INSTANT,
                "case {case:?}: {member} must be the published asset's own instant \
                 {PUBLISHED_STYLE_INSTANT:?}, got {stamp:?}"
            );
            // The map resource was created by the call above, so its own
            // instants are of this run: the asset's stamp is neither of them.
            for resource_member in ["CreateTime", "UpdateTime"] {
                let reported = described
                    .get(resource_member)
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert_ne!(
                    stamp, reported,
                    "case {case:?}: {member} must be the published stamp rather than the \
                     resource's {resource_member}, got {stamp:?}"
                );
            }
        }

        // Read again: the published asset a repeat read answers with is the same
        // document, stamps included.
        let after = style_descriptor(&router, &map).await;
        assert_eq!(
            after, document,
            "case {case:?}: a repeat read must answer the same published document"
        );
    }
}

#[tokio::test]
async fn two_maps_of_two_styles_publish_the_same_asset_instants() {
    let router = emulator();
    // Rows: two maps of one style and one of another provider's, created one
    // after the other — the asset's instants are a property of the asset, so all
    // three carry the same two values even though the resources do not.
    for (map, style) in [
        ("shared.one", "VectorEsriStreets"),
        ("shared.two", "VectorEsriStreets"),
        ("SHARED-3", "VectorHereExplore"),
    ] {
        create_map(&router, map, style).await;
    }
    let mut stamps: Vec<(String, String)> = Vec::new();
    for map in ["shared.one", "shared.two", "SHARED-3"] {
        let document = style_descriptor(&router, map).await;
        stamps.push((
            string_member(map, &document, "created"),
            string_member(map, &document, "modified"),
        ));
    }
    assert!(
        stamps.iter().all(|(created, modified)| created
            == PUBLISHED_STYLE_INSTANT
            && modified == PUBLISHED_STYLE_INSTANT),
        "every published asset reports {PUBLISHED_STYLE_INSTANT:?} in both members, got {stamps:?}"
    );
}
