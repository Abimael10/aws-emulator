// SPDX-License-Identifier: CC0-1.0

//! The style descriptor a map resource publishes, end to end over the wire, and
//! the media type the tile plane beside it labels a tile with.
//!
//! Every call goes through the shared harness (`crate::harness`), so each one is
//! a separate HTTP request against one shared engine: what a descriptor declares
//! came from the map resource the create call PERSISTED, not from the descriptor
//! request itself.
//!
//! The rules these suites assert:
//!
//! * a style descriptor is a loadable Mapbox/MapLibre style of the map it is
//!   read for — it declares its tile source under the id of the data provider
//!   the map's own style is sourced from, templates that source's tiles, its
//!   sprite sheet and its glyph ranges onto this emulator's own routes for THIS
//!   map resource, reports the published style asset's own instants, and binds
//!   every layer it paints to the source it declared (no layer of the style's
//!   own invention);
//! * a tile's `Content-Type` follows the OPERATION rather than the encoding of
//!   the bytes inside it: every style's tile is labelled with the one media type
//!   this plane serves binary assets as, while the body still decodes as the
//!   format the map's style publishes.
//!
//! The tables span those classes: every published style (so every data
//! provider, and both tile formats), two map resources of one style (so a
//! template cannot be a fixed string), and the deepest and shallowest zoom of
//! the pyramid the document declares.

use serde_json::Value;

use service_com_amazonaws_location_locationservice::state;

// The map-plane helpers live in the harness, shared with the tile suite beside
// this one, so the published catalog, the tile media type and the create and
// descriptor steps are stated once for the crate.
use crate::harness::{
    call, call_bytes, create_map, emulator, published_styles, raster_basemap, style_descriptor,
    TILE_CONTENT_TYPE,
};

/// The source id a style descriptor declares its tiles under, per data
/// provider.
///
/// Source: recorded behaviour of `GetMapStyleDescriptor` — the descriptor of an
/// Esri-backed map declares its one source as `esri`, the provider's own name as
/// a style source id. The rows are every provider this API publishes a map style
/// for, as the `MapConfiguration$Style` doc groups them ("Valid Esri map
/// styles", "Valid HERE Technologies map styles", "Valid GrabMaps map styles",
/// "Valid Open Data map styles") and the
/// `CreateRouteCalculatorRequest$DataSource` doc spells them — including the two
/// the issue's recording never touches.
const SOURCE_IDS: [(&str, &str); 4] = [
    ("Esri", "esri"),
    ("Here", "here"),
    ("Grab", "grab"),
    ("Open Data", "opendata"),
];

/// The deepest zoom an Esri-backed VECTOR map's source declares, and so the
/// deepest tile such a map serves.
///
/// Source: recorded behaviour of `GetMapStyleDescriptor` — the descriptor AWS
/// serves for an Esri-backed vector map declares `maxzoom` 15 on its provider
/// source. Written here as the literal the recording carries, so this suite
/// fails if the catalog's depth for that family moves, rather than reading the
/// depth back out of the catalog it is about.
const ESRI_VECTOR_SOURCE_MAX_ZOOM: u64 = 15;

/// The instant a published style asset reports as both its `created` and its
/// `modified`.
///
/// Source: recorded behaviour of `GetMapStyleDescriptor` — the document AWS
/// serves carries the zero-value timestamp in both members, a FIXED constant of
/// the published asset rather than an instant of the serving clock or of the map
/// resource that was read. Written here as the literal the recording carries, so
/// this suite fails if the asset's stamp moves, rather than reading the stamp
/// back out of the code it is about.
const PUBLISHED_STYLE_INSTANT: &str = "0001-01-01T00:00:00Z";

/// The source id the table above gives `provider`.
fn expected_source_id(provider: &str) -> &'static str {
    SOURCE_IDS
        .into_iter()
        .find(|(name, _)| *name == provider)
        .map(|(_, id)| id)
        .unwrap_or_else(|| {
            panic!("the provider {provider:?} is published with no source id in this table")
        })
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

/// The one source a descriptor declares, with the id it is declared under.
fn only_source(case: &str, document: &Value) -> (String, Value) {
    let sources = document
        .get("sources")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("case {case:?}: the style must carry sources, got {document}"));
    let mut declared: Vec<(String, Value)> = sources
        .iter()
        .map(|(name, source)| (name.clone(), source.clone()))
        .collect();
    assert_eq!(
        declared.len(),
        1,
        "case {case:?}: the style declares the map's own tiles as its one source, got {:?}",
        declared.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>()
    );
    declared.remove(0)
}

/// Every layer of a descriptor, as objects.
fn declared_layers(case: &str, document: &Value) -> Vec<Value> {
    document
        .get("layers")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: the style must carry layers, got {document}"))
        .clone()
}

#[tokio::test]
async fn every_published_styles_descriptor_declares_its_providers_source_and_this_maps_routes() {
    let router = emulator();
    for (index, (style, provider, max_zoom)) in published_styles().into_iter().enumerate() {
        let map = format!("styled.{index}_{style}");
        create_map(&router, &map, style).await;
        let case = format!("{style} of {provider}");
        let document = style_descriptor(&router, &map).await;

        // The source is declared under the provider's own id, not a name of this
        // service's choosing.
        let (source_id, source) = only_source(&case, &document);
        let expected_id = expected_source_id(provider);
        assert_eq!(
            source_id, expected_id,
            "case {case:?}: the source must be declared as {expected_id:?}, got {source_id:?} \
             in {document}"
        );

        // That source reads THIS map's tiles, over the pyramid its own style
        // publishes, credited to its own provider.
        let template = source
            .get("tiles")
            .and_then(Value::as_array)
            .and_then(|tiles| tiles.first())
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: the source must carry a tile template, got {source}")
            })
            .to_string();
        assert!(
            template.ends_with(&format!("/maps/v0/maps/{map}/tiles/{{z}}/{{x}}/{{y}}")),
            "case {case:?}: the tile template must address this map's own tiles, got {template:?}"
        );
        assert_eq!(
            source.get("type").and_then(Value::as_str),
            Some(if raster_basemap(style) {
                "raster"
            } else {
                "vector"
            }),
            "case {case:?}: the source must declare the kind of tiles the style serves, got \
             {source}"
        );
        assert_eq!(
            source.get("minzoom").and_then(Value::as_u64),
            Some(0),
            "case {case:?}: the source must declare the world tile as its lowest zoom, got {source}"
        );
        assert_eq!(
            source.get("maxzoom").and_then(Value::as_u64),
            Some(u64::from(max_zoom)),
            "case {case:?}: the source must declare maxzoom {max_zoom}, got {source}"
        );
        assert!(
            source
                .get("attribution")
                .and_then(Value::as_str)
                .is_some_and(|credit| credit.contains(provider)),
            "case {case:?}: the source must credit {provider}, got {source}"
        );

        // The glyph and sprite templates address this emulator's own routes for
        // this map resource, as the model lays those routes out.
        let glyphs = string_member(&case, &document, "glyphs");
        assert!(
            glyphs.ends_with(&format!("/maps/v0/maps/{map}/glyphs/{{fontstack}}/{{range}}.pbf")),
            "case {case:?}: the glyph template must address this map's own glyph route, got \
             {glyphs:?}"
        );
        let substituted = glyphs
            .replace("{fontstack}", "Arial%20Regular")
            .replace("{range}", "0-255");
        assert!(
            substituted.ends_with(&format!(
                "/maps/v0/maps/{map}/glyphs/Arial%20Regular/0-255.pbf"
            )),
            "case {case:?}: a substituted glyph template must spell the modelled GetMapGlyphs \
             URI, got {substituted:?}"
        );
        let sprite = string_member(&case, &document, "sprite");
        assert!(
            sprite.ends_with(&format!("/maps/v0/maps/{map}/sprites/sprites")),
            "case {case:?}: the sprite base must address this map's own sprite sheet, got \
             {sprite:?}"
        );

        // Every layer the style paints reads the source the document declared:
        // a client that loads this style has a tile source for every layer of
        // it, starting with the first.
        let layers = declared_layers(&case, &document);
        assert!(
            !layers.is_empty(),
            "case {case:?}: the style must paint at least one layer, got {document}"
        );
        for (position, layer) in layers.iter().enumerate() {
            assert_eq!(
                layer.get("source").and_then(Value::as_str),
                Some(source_id.as_str()),
                "case {case:?}: layer {position} must read the declared source {source_id:?}, \
                 got {layer}"
            );
            assert_ne!(
                layer.get("type").and_then(Value::as_str),
                Some("background"),
                "case {case:?}: layer {position} must be bound to the source rather than be a \
                 background of the style's own, got {layer}"
            );
            assert!(
                layer.get("id").and_then(Value::as_str).is_some(),
                "case {case:?}: layer {position} must carry an id, got {layer}"
            );
        }

        // The style names the style the map was created with.
        assert_eq!(
            document.get("name").and_then(Value::as_str),
            Some(style),
            "case {case:?}: the style document must name the style it describes, got {document}"
        );
        assert_eq!(
            document.get("version").and_then(Value::as_u64),
            Some(8),
            "case {case:?}: the document must declare the Mapbox Style Specification version it \
             is written in, got {document}"
        );
    }
}

#[tokio::test]
async fn an_esri_vector_styles_source_declares_the_depth_that_provider_publishes() {
    let router = emulator();
    // Rows: every Esri-backed VECTOR style the catalog publishes — the street,
    // navigation and topographic basemaps and both canvas styles are tiles of
    // one vector basemap family, so the recorded depth covers them all. The
    // Esri imagery style is a raster basemap of its own and is not one of them.
    let esri = SOURCE_IDS[0].0;
    let rows: Vec<&str> = published_styles()
        .into_iter()
        .filter(|(style, provider, _)| *provider == esri && !raster_basemap(style))
        .map(|(style, _, _)| style)
        .collect();
    assert!(
        rows.len() > 1,
        "the catalog must publish more than one {esri} vector style for this rule to be read \
         over a family, got {rows:?}"
    );
    for (index, style) in rows.into_iter().enumerate() {
        let map = format!("depth.{index}_{style}");
        create_map(&router, &map, style).await;
        let document = style_descriptor(&router, &map).await;
        let (_, source) = only_source(style, &document);
        assert_eq!(
            source.get("maxzoom").and_then(Value::as_u64),
            Some(ESRI_VECTOR_SOURCE_MAX_ZOOM),
            "case {style:?}: the source must declare the depth {esri} publishes its vector \
             tiles to, got {source}"
        );

        // The declared depth is the pyramid the tile plane really serves: that
        // tile answers, and the one past it is refused.
        let deepest = call_bytes(
            &router,
            "GET",
            &format!("/maps/v0/maps/{map}/tiles/{ESRI_VECTOR_SOURCE_MAX_ZOOM}/0/0"),
        )
        .await;
        assert_eq!(
            deepest.status, 200,
            "case {style:?}: the declared maxzoom must answer, got {}",
            deepest.status
        );
        let past = call(
            &router,
            "GET",
            &format!(
                "/maps/v0/maps/{map}/tiles/{}/0/0",
                ESRI_VECTOR_SOURCE_MAX_ZOOM + 1
            ),
            None,
        )
        .await;
        assert_eq!(
            past.status, 400,
            "case {style:?}: the zoom past the declared maxzoom must be refused, got {} {:?}",
            past.status, past.body
        );
    }
}

#[tokio::test]
async fn a_vector_styles_layers_are_the_layers_its_tiles_carry() {
    let router = emulator();
    // Rows: one vector style per data provider, so the layer rule is read for
    // every catalog the service publishes rather than for one provider's.
    for (index, (style, provider, _)) in published_styles()
        .into_iter()
        .filter(|(style, _, _)| !raster_basemap(style))
        .enumerate()
    {
        let map = format!("layered.{index}_{style}");
        create_map(&router, &map, style).await;
        let case = format!("{style} of {provider}");
        let document = style_descriptor(&router, &map).await;
        let layers = declared_layers(&case, &document);

        let painted: Vec<String> = layers
            .iter()
            .map(|layer| {
                layer
                    .get("source-layer")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: every layer of a vector style names the tile layer it \
                             paints, got {layer}"
                        )
                    })
                    .to_string()
            })
            .collect();
        let carried: Vec<String> = state::map_tiles::tile_layers()
            .iter()
            .map(|layer| layer.name.to_string())
            .collect();
        // A source layer is painted by one published layer per sub-class it
        // carries, so the sequence of source layers the document paints is the
        // tile schema expanded over those classes, in the order the tiles encode
        // them.
        let expected: Vec<String> = state::map_tiles::tile_layers()
            .iter()
            .flat_map(|layer| {
                layer
                    .classes
                    .iter()
                    .map(move |_| layer.name.to_string())
            })
            .collect();
        assert_eq!(
            painted, expected,
            "case {case:?}: the style must paint the layers its tiles carry, in the order the \
             tiles encode them, got {painted:?}"
        );

        // The ground is one of those tile layers, so the FIRST layer of the
        // document is bound to the source like every other.
        let first = layers.first().unwrap_or_else(|| {
            panic!("case {case:?}: a vector style must paint at least one layer")
        });
        assert_eq!(
            first.get("source-layer").and_then(Value::as_str),
            carried.first().map(String::as_str),
            "case {case:?}: the first layer must paint the first layer the tiles carry, got \
             {first}"
        );
        assert!(
            first.get("paint").is_some(),
            "case {case:?}: the first layer must carry paint for the features it draws, got \
             {first}"
        );
    }
}

#[tokio::test]
async fn a_descriptor_reports_the_published_assets_own_instants() {
    let router = emulator();
    // Rows: a vector style and a raster one, under names of different shapes —
    // the instants belong to the published ASSET, so neither the style, the
    // provider nor the name changes them.
    for (map, style) in [
        ("instants.first_map", "VectorEsriNavigation"),
        ("INSTANTS-2", "RasterEsriImagery"),
        ("i", "VectorOpenDataStandardDark"),
    ] {
        create_map(&router, map, style).await;
        let described = call(&router, "GET", &format!("/maps/v0/maps/{map}"), None).await;
        assert_eq!(
            described.status, 200,
            "case {map:?}: the map must be readable, got {} {:?}",
            described.status, described.body
        );
        let described = described.json();
        let document = style_descriptor(&router, map).await;

        for member in ["created", "modified"] {
            let rendered = string_member(map, &document, member);
            assert_eq!(
                rendered, PUBLISHED_STYLE_INSTANT,
                "case {map:?}: {member} must be the published asset's own instant \
                 {PUBLISHED_STYLE_INSTANT:?}, got {rendered:?}"
            );
            // The asset's stamp is not the instant the RESOURCE carries: the map
            // was created by the call above, so its own instants are of this
            // run.
            for resource_member in ["CreateTime", "UpdateTime"] {
                let stored = described
                    .get(resource_member)
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert_ne!(
                    rendered, stored,
                    "case {map:?}: {member} must be the published stamp rather than the \
                     resource's {resource_member}, got {rendered:?}"
                );
            }
        }

        // Read twice: a descriptor is the stored published asset, so nothing in
        // it moves because it was read.
        let again = style_descriptor(&router, map).await;
        assert_eq!(
            again, document,
            "case {map:?}: a repeat read must answer the same document"
        );
    }
}

#[tokio::test]
async fn two_maps_of_one_style_each_template_their_own_routes() {
    let router = emulator();
    let style = "VectorHereExplore";
    create_map(&router, "twin.one", style).await;
    create_map(&router, "twin.two", style).await;

    let one = style_descriptor(&router, "twin.one").await;
    let two = style_descriptor(&router, "twin.two").await;
    for (map, document) in [("twin.one", &one), ("twin.two", &two)] {
        for member in ["glyphs", "sprite"] {
            let template = string_member(map, document, member);
            assert!(
                template.contains(&format!("/maps/v0/maps/{map}/")),
                "case {map:?}: {member} must address this map resource, got {template:?}"
            );
        }
        let (_, source) = only_source(map, document);
        let tiles = source
            .get("tiles")
            .and_then(Value::as_array)
            .and_then(|tiles| tiles.first())
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            tiles.contains(&format!("/maps/v0/maps/{map}/")),
            "case {map:?}: the tile template must address this map resource, got {tiles:?}"
        );
    }
    assert_ne!(
        one, two,
        "two maps of one style must each publish their own templates"
    );
}

#[tokio::test]
async fn every_styles_tile_is_labelled_as_the_binary_asset_it_is() {
    let router = emulator();
    for (index, (style, provider, max_zoom)) in published_styles().into_iter().enumerate() {
        let map = format!("labelled.{index}_{style}");
        create_map(&router, &map, style).await;
        // Rows: the world tile and the deepest tile the style publishes — the
        // label follows the operation, so neither end of the pyramid changes
        // it.
        for zoom in [0, max_zoom] {
            let case = format!("{style} of {provider} at zoom {zoom}");
            let served = call_bytes(
                &router,
                "GET",
                &format!("/maps/v0/maps/{map}/tiles/{zoom}/0/0"),
            )
            .await;
            assert_eq!(
                served.status, 200,
                "case {case:?}: expected 200, got {} {:?}",
                served.status,
                String::from_utf8_lossy(&served.body)
            );
            assert_eq!(
                served.header("content-type").as_deref(),
                Some(TILE_CONTENT_TYPE),
                "case {case:?}: expected Content-Type {TILE_CONTENT_TYPE:?}, got {:?}",
                served.header("content-type")
            );
            // The label is the operation's; the BODY is still the format the
            // map's own style publishes.
            if raster_basemap(style) {
                let decoder = png::Decoder::new(served.body.as_slice());
                assert!(
                    decoder.read_info().is_ok(),
                    "case {case:?}: a raster basemap's tile must still be a PNG"
                );
            } else {
                let tile = state::vector_tile::decode_tile(&served.body).unwrap_or_else(|error| {
                    panic!(
                        "case {case:?}: a vector basemap's tile must still decode as a Mapbox \
                         Vector Tile, got {error}"
                    )
                });
                assert!(
                    !tile.layers.is_empty(),
                    "case {case:?}: a vector tile must carry at least one layer"
                );
            }
        }
    }
}
