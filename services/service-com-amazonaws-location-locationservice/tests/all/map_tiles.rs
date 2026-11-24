// SPDX-License-Identifier: CC0-1.0

//! The tile pyramid a map resource serves, and the style descriptor that
//! declares it, end to end over the wire.
//!
//! Every call goes through the shared harness (`crate::harness`), so each one is
//! a separate HTTP request against one shared engine: a tile a read answers with
//! came from the map resource the create call PERSISTED, not from the tile
//! request itself.
//!
//! The rule these suites assert: a tile read of a stored map answers with the
//! tile its `Z`/`X`/`Y` address names, in the format the map's own stored style
//! publishes, for an address INSIDE that style's pyramid — zoom 0 up to the
//! deepest zoom the style has tiles for, and a column and row inside the `2^Z`
//! grid that zoom defines — and refuses an address outside it as a bad field,
//! a map name nothing is stored under as not found, and a label that is not a
//! decimal number as a value outside the member's pattern. The tables span that
//! class: every published style (so every provider and both tile formats), the
//! shallowest, a middle and the deepest zoom each style publishes, the corner
//! and the far corner of a zoom's grid, and the shapes a label outside the
//! pattern can take.
//!
//! The style descriptor is asserted beside it because it DECLARES that pyramid:
//! the `maxzoom` it publishes is the deepest zoom a tile read of the same map
//! serves, which is proved here by fetching that tile and the one past it.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_location_locationservice::state;

// The map-plane helpers every suite of this crate shares — the published style
// catalog, the tile media type, the raster test, the create step and the
// descriptor read — live in the harness, so this suite and the style-document
// suite beside it cannot drift apart about any of them.
use crate::harness::{
    call, call_bytes, create_map, emulator, millis_of, published_styles, raster_basemap,
    style_descriptor, RawAnswer, TILE_CONTENT_TYPE,
};

async fn get_tile(
    router: &MultiServiceRouter,
    map: &str,
    z: &str,
    x: &str,
    y: &str,
) -> RawAnswer {
    call_bytes(router, "GET", &format!("/maps/v0/maps/{map}/tiles/{z}/{x}/{y}")).await
}

/// The `maxzoom` every source of a style document declares.
fn declared_max_zooms(case: &str, document: &Value) -> Vec<u64> {
    let sources = document
        .get("sources")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("case {case:?}: the style must carry sources, got {document}"));
    assert!(
        !sources.is_empty(),
        "case {case:?}: a style must read from at least one source, got {document}"
    );
    sources
        .values()
        .map(|source| {
            source
                .get("maxzoom")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every source must declare maxzoom, got {source}")
                })
        })
        .collect()
}

/// The layer names a decoded vector tile carries, in the order the tile encodes
/// them.
fn tile_layers(case: &str, bytes: &[u8]) -> Vec<String> {
    let tile = state::vector_tile::decode_tile(bytes).unwrap_or_else(|error| {
        panic!("case {case:?}: the body must decode as a Mapbox Vector Tile, got {error}")
    });
    for layer in &tile.layers {
        assert_eq!(
            layer.version, 2,
            "case {case:?}: layer {:?} must declare specification version 2, got {}",
            layer.name, layer.version
        );
        assert!(
            layer.extent > 0,
            "case {case:?}: layer {:?} must declare its extent",
            layer.name
        );
        assert!(
            !layer.features.is_empty(),
            "case {case:?}: layer {:?} must carry features",
            layer.name
        );
        for feature in &layer.features {
            assert!(
                !feature.geometry.is_empty(),
                "case {case:?}: every feature of layer {:?} must carry geometry",
                layer.name
            );
            assert_eq!(
                feature.tags.len() % 2,
                0,
                "case {case:?}: a feature's tags are (key, value) index pairs, got {:?}",
                feature.tags
            );
            for pair in feature.tags.chunks(2) {
                assert!(
                    (pair[0] as usize) < layer.keys.len() && (pair[1] as usize) < layer.values.len(),
                    "case {case:?}: feature tags {pair:?} must index layer {:?}'s own tables",
                    layer.name
                );
            }
        }
    }
    tile.layers.iter().map(|layer| layer.name.clone()).collect()
}

/// A tile address inside the `2^Z` grid of `zoom`: the grid's own corner, so
/// every row of a table can be read at every zoom.
const CORNER: (&str, &str) = ("0", "0");

#[tokio::test]
async fn every_published_style_serves_its_own_tiles_in_its_own_format() {
    let router = emulator();
    for (index, (style, _provider, max_zoom)) in published_styles().into_iter().enumerate() {
        let map = format!("tile.{index}_{style}");
        create_map(&router, &map, style).await;
        // Rows: the world tile, a middle zoom, and the deepest zoom the style
        // publishes — the whole range, not a sample of it.
        for zoom in [0, max_zoom / 2, max_zoom] {
            let case = format!("{style} at zoom {zoom}");
            let served = get_tile(&router, &map, &zoom.to_string(), CORNER.0, CORNER.1).await;
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
            assert!(
                served
                    .header("cache-control")
                    .is_some_and(|value| value.contains("max-age=")),
                "case {case:?}: expected a Cache-Control directive with a freshness window, got \
                 {:?}",
                served.header("cache-control")
            );
            assert!(
                !served.body.is_empty(),
                "case {case:?}: the served tile must carry a body"
            );

            // The body really is the format its media type claims.
            if raster_basemap(style) {
                let decoder = png::Decoder::new(served.body.as_slice());
                let reader = decoder.read_info().unwrap_or_else(|error| {
                    panic!("case {case:?}: a raster tile must be a PNG, got {error}")
                });
                let info = reader.info();
                assert_eq!(
                    (info.width, info.height),
                    (256, 256),
                    "case {case:?}: a raster tile is one 256-pixel tile, got {}x{}",
                    info.width,
                    info.height
                );
            } else {
                let layers = tile_layers(&case, &served.body);
                assert!(
                    !layers.is_empty(),
                    "case {case:?}: a vector tile must carry at least one layer"
                );
            }

            // A read is a function of stored state alone, so asking twice
            // answers the same bytes.
            let again = get_tile(&router, &map, &zoom.to_string(), CORNER.0, CORNER.1).await;
            assert_eq!(
                again.body, served.body,
                "case {case:?}: a repeat read must answer the same bytes"
            );
        }
    }
}

#[tokio::test]
async fn a_zoom_past_the_styles_own_pyramid_is_refused() {
    let router = emulator();
    for (index, (style, _provider, max_zoom)) in published_styles().into_iter().enumerate() {
        let map = format!("bound.{index}_{style}");
        create_map(&router, &map, style).await;
        // Rows: the first zoom past the style's own deepest, one well past it,
        // and a zoom no pyramid reaches.
        for zoom in [max_zoom + 1, max_zoom + 8, 30] {
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
                "case {case:?}: expected a 400 refusal, got {} {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_code(),
                "ValidationException",
                "case {case:?}: expected ValidationException, got {:?} ({:?})",
                refused.error_code(),
                refused.body
            );
            let sentence =
                format!("'Z' must not be greater than maximum zoom of style {style} ({max_zoom}).");
            assert_eq!(
                refused.message(),
                sentence,
                "case {case:?}: expected message {sentence:?}, got {:?}",
                refused.message()
            );
            // The refusal names the member it is about, in the shape every
            // field refusal of this service carries.
            let body = refused.json();
            assert_eq!(
                body.get("reason").and_then(Value::as_str),
                Some("FieldValidationFailed"),
                "case {case:?}: expected reason FieldValidationFailed, got {body}"
            );
            assert_eq!(
                body.get("fieldList")
                    .and_then(Value::as_array)
                    .map(|entries| entries
                        .iter()
                        .map(|entry| (
                            entry.get("name").and_then(Value::as_str).unwrap_or_default(),
                            entry
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                        ))
                        .collect::<Vec<_>>()),
                Some(vec![("Z", sentence.as_str())]),
                "case {case:?}: fieldList must name Z and repeat the sentence, got {body}"
            );
        }
    }
}

#[tokio::test]
async fn the_style_descriptor_declares_the_pyramid_a_tile_read_serves() {
    let router = emulator();
    for (index, (style, provider, max_zoom)) in published_styles().into_iter().enumerate() {
        let map = format!("declared.{index}_{style}");
        create_map(&router, &map, style).await;
        let document = style_descriptor(&router, &map).await;
        let declared = declared_max_zooms(style, &document);
        assert!(
            declared.iter().all(|zoom| *zoom == u64::from(max_zoom)),
            "case {style:?}: every source must declare maxzoom {max_zoom}, got {declared:?}"
        );
        assert_eq!(
            document.get("name").and_then(Value::as_str),
            Some(style),
            "case {style:?}: the style document must name the style it describes, got {document}"
        );
        let sources = document
            .get("sources")
            .and_then(Value::as_object)
            .expect("checked by declared_max_zooms");
        for (name, source) in sources {
            let template = source
                .get("tiles")
                .and_then(Value::as_array)
                .and_then(|tiles| tiles.first())
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {style:?}: source {name} must carry a tile template, got {source}")
                });
            assert!(
                template.ends_with(&format!("/maps/v0/maps/{map}/tiles/{{z}}/{{x}}/{{y}}")),
                "case {style:?}: source {name} must template this map's own tiles, got \
                 {template:?}"
            );
            assert!(
                source
                    .get("attribution")
                    .and_then(Value::as_str)
                    .is_some_and(|credit| credit.contains(provider)),
                "case {style:?}: source {name} must credit {provider}, got {source}"
            );
            assert_eq!(
                source.get("minzoom").and_then(Value::as_u64),
                Some(0),
                "case {style:?}: source {name} must declare the world tile as its lowest zoom, \
                 got {source}"
            );
        }

        // The declared maximum is the deepest zoom that really answers, and the
        // zoom past it really is refused: the document and the tile plane state
        // one pyramid.
        let deepest = get_tile(&router, &map, &max_zoom.to_string(), "0", "0").await;
        assert_eq!(
            deepest.status, 200,
            "case {style:?}: the declared maxzoom {max_zoom} must answer, got {} {:?}",
            deepest.status,
            String::from_utf8_lossy(&deepest.body)
        );
        let past = get_tile(&router, &map, &(max_zoom + 1).to_string(), "0", "0").await;
        assert_eq!(
            past.status, 400,
            "case {style:?}: the zoom past the declared maxzoom must be refused, got {}",
            past.status
        );
    }
}

#[tokio::test]
async fn the_layers_a_style_paints_are_the_layers_its_tiles_carry() {
    let router = emulator();
    create_map(&router, "LayeredMap", "VectorEsriStreets").await;
    let document = style_descriptor(&router, "LayeredMap").await;
    let painted: Vec<String> = document
        .get("layers")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the style must carry layers, got {document}"))
        .iter()
        .filter_map(|layer| {
            layer
                .get("source-layer")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert!(
        !painted.is_empty(),
        "the style must paint the source layers its tiles carry, got {document}"
    );

    // Every layer the tiles can carry is painted, and every layer a tile at the
    // deepest zoom carries is one the style paints.
    for layer in state::map_tiles::tile_layers() {
        assert!(
            painted.iter().any(|name| name == layer.name),
            "layer {:?} is carried by the tiles but painted by no style layer, got {painted:?}",
            layer.name
        );
    }
    // The deepest zoom this style's own pyramid publishes, read from the
    // catalog, so the row follows the style rather than a zoom written here.
    let deepest = published_styles()
        .into_iter()
        .find(|(style, _, _)| *style == "VectorEsriStreets")
        .map(|(_, _, max_zoom)| max_zoom)
        .expect("the catalog publishes VectorEsriStreets");
    let case = format!("VectorEsriStreets at zoom {deepest}");
    let deep = get_tile(&router, "LayeredMap", &deepest.to_string(), "0", "0").await;
    for carried in tile_layers(&case, &deep.body) {
        assert!(
            painted.contains(&carried),
            "tile layer {carried:?} is painted by no style layer, got {painted:?}"
        );
    }
}

/// The tile at `zoom` whose centre lies over land, found by walking the grid of
/// that zoom with the same ground the service reads — so a row of the table
/// below is about a tile whose land layers are drawn at all.
fn land_tile(zoom: u32) -> state::map_tiles::TileAddress {
    let axis = u32::try_from(state::map_tiles::TileAddress::axis_tiles(zoom)).unwrap_or(u32::MAX);
    for step in 0..256 {
        let address = state::map_tiles::TileAddress {
            z: zoom,
            x: (axis / 7).saturating_add(step) % axis,
            y: (axis / 3).saturating_add(step) % axis,
        };
        let (longitude, latitude) = address.centre();
        if !state::map_tiles::over_water(longitude, latitude) {
            return address;
        }
    }
    panic!("no tile of zoom {zoom} covers land, so the table below cannot be read");
}

/// What a tile carries follows its ADDRESS: the zoom decides which layers are
/// drawn at all, and the ground the tile covers decides whether it carries the
/// water layer or the land ones.
#[tokio::test]
async fn a_tiles_content_follows_its_own_address() {
    let router = emulator();
    create_map(&router, "AddressedMap", "VectorOpenDataStandardLight").await;

    /// The layers a tile over LAND carries at each zoom a layer comes in at.
    ///
    /// Source: this crate's tile schema (`state::map_tiles::tile_layers`),
    /// which names each layer and the lowest zoom it is drawn at; the rows are
    /// those thresholds, so each one adds exactly the layer that zoom brings in.
    const LAND_LAYERS: [(u32, &[&str]); 6] = [
        (0, &["land", "boundaries", "places"]),
        (5, &["land", "boundaries", "places", "roads"]),
        (7, &["land", "boundaries", "places", "roads", "railways"]),
        (
            10,
            &["land", "boundaries", "places", "roads", "railways", "parks"],
        ),
        (
            12,
            &[
                "land",
                "boundaries",
                "places",
                "roads",
                "railways",
                "parks",
                "landmarks",
            ],
        ),
        (
            14,
            &[
                "land",
                "boundaries",
                "places",
                "roads",
                "railways",
                "parks",
                "landmarks",
                "buildings",
            ],
        ),
    ];

    for (zoom, expected) in LAND_LAYERS {
        let address = land_tile(zoom);
        let case = format!("zoom {zoom} at {}/{}", address.x, address.y);
        let served = get_tile(
            &router,
            "AddressedMap",
            &zoom.to_string(),
            &address.x.to_string(),
            &address.y.to_string(),
        )
        .await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: expected 200, got {} {:?}",
            served.status,
            String::from_utf8_lossy(&served.body)
        );
        let mut carried = tile_layers(&case, &served.body);
        carried.sort();
        let mut wanted: Vec<String> = expected.iter().map(|name| name.to_string()).collect();
        wanted.sort();
        assert_eq!(
            carried, wanted,
            "case {case:?}: expected the land layers {wanted:?}, got {carried:?}"
        );
    }

    // A tile over WATER carries the water layer instead of the land ones, at
    // every zoom: the ground decides, not the zoom.
    let mut found_water = false;
    for (zoom, x, y) in [(1_u32, 0_u32, 1_u32), (3, 6, 4), (6, 40, 20), (9, 300, 100)] {
        let address = state::map_tiles::TileAddress { z: zoom, x, y };
        let (longitude, latitude) = address.centre();
        if !state::map_tiles::over_water(longitude, latitude) {
            continue;
        }
        found_water = true;
        let case = format!("water at zoom {zoom} {x}/{y}");
        let served = get_tile(
            &router,
            "AddressedMap",
            &zoom.to_string(),
            &x.to_string(),
            &y.to_string(),
        )
        .await;
        let carried = tile_layers(&case, &served.body);
        assert!(
            carried.contains(&"water".to_string()),
            "case {case:?}: a tile over water must carry the water layer, got {carried:?}"
        );
        assert!(
            !carried.contains(&"places".to_string()),
            "case {case:?}: a tile over water must carry no land layer, got {carried:?}"
        );
    }
    assert!(
        found_water,
        "none of the probed addresses covers water, so the water rule was not exercised"
    );

    // Two tiles of one map at one zoom cover different ground, so they are not
    // the same bytes; and the same tile of two maps whose styles come from
    // different providers is not the same bytes either.
    let here = get_tile(&router, "AddressedMap", "4", "3", "6").await;
    let there = get_tile(&router, "AddressedMap", "4", "12", "9").await;
    assert_ne!(
        here.body, there.body,
        "two tiles covering different ground must differ"
    );
    create_map(&router, "OtherProviderMap", "VectorHereExplore").await;
    let other = get_tile(&router, "OtherProviderMap", "4", "3", "6").await;
    assert_ne!(
        here.body, other.body,
        "the same tile of two providers' styles must differ"
    );
}

/// One row of the refusal table: the address to ask for and what is wrong with
/// it.
#[derive(Debug)]
struct BadAddress {
    case: &'static str,
    z: &'static str,
    x: &'static str,
    y: &'static str,
    member: &'static str,
    sentence: &'static str,
}
#[tokio::test]
async fn an_address_outside_the_grid_or_outside_the_pattern_is_refused() {
    let router = emulator();
    // Zoom 2 has 4 tiles on each axis, so 4 is the first column and row outside
    // it; zoom 0 has exactly one tile.
    create_map(&router, "GridMap", "VectorEsriStreets").await;

    let rows = [
        BadAddress {
            case: "a column past the grid of this zoom",
            z: "2",
            x: "4",
            y: "0",
            member: "X",
            sentence: "'X' must be an integer in the range 0..2^Zoom-1 (0..3).",
        },
        BadAddress {
            case: "a row past the grid of this zoom",
            z: "2",
            x: "0",
            y: "4",
            member: "Y",
            sentence: "'Y' must be an integer in the range 0..2^Zoom-1 (0..3).",
        },
        BadAddress {
            case: "the second tile of the world zoom",
            z: "0",
            x: "1",
            y: "0",
            member: "X",
            sentence: "'X' must be an integer in the range 0..2^Zoom-1 (0..0).",
        },
        BadAddress {
            case: "a column larger than any address",
            z: "2",
            x: "99999999999999999999999",
            y: "0",
            member: "X",
            sentence: "'X' must be an integer in the range 0..2^Zoom-1 (0..3).",
        },
        BadAddress {
            case: "a zoom that is not a number",
            z: "two",
            x: "0",
            y: "0",
            member: "Z",
            sentence: r"'Z' must match regex \d+",
        },
        BadAddress {
            case: "a column with a trailing unit",
            z: "2",
            x: "1px",
            y: "0",
            member: "X",
            sentence: r"'X' must match regex \d+",
        },
        BadAddress {
            case: "a row with a sign",
            z: "2",
            x: "0",
            y: "-1",
            member: "Y",
            sentence: r"'Y' must match regex \d+",
        },
        BadAddress {
            case: "a fractional row",
            z: "2",
            x: "0",
            y: "1.5",
            member: "Y",
            sentence: r"'Y' must match regex \d+",
        },
    ];

    for row in rows {
        let case = row.case;
        let refused = call(
            &router,
            "GET",
            &format!("/maps/v0/maps/GridMap/tiles/{}/{}/{}", row.z, row.x, row.y),
            None,
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected a 400 refusal of {}, got {} {:?}",
            row.member, refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?} ({:?})",
            refused.error_code(),
            refused.body
        );
        assert_eq!(
            refused.message(),
            row.sentence,
            "case {case:?}: expected message {:?}, got {:?}",
            row.sentence,
            refused.message()
        );
    }
}

#[tokio::test]
async fn a_map_name_no_map_is_stored_under_is_not_found() {
    let router = emulator();
    create_map(&router, "StoredTileMap", "VectorEsriStreets").await;

    for (case, map) in [
        ("a name no map holds", "AbsentMap"),
        ("a stored name in another case", "storedtilemap"),
        ("a stored name with a suffix", "StoredTileMap2"),
    ] {
        for path in ["tiles/4/2/3", "style-descriptor"] {
            let refused = call(&router, "GET", &format!("/maps/v0/maps/{map}/{path}"), None).await;
            assert_eq!(
                refused.status, 404,
                "case {case:?} ({map:?}, {path:?}): expected a 404, got {} {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_code(),
                "ResourceNotFoundException",
                "case {case:?} ({map:?}, {path:?}): expected ResourceNotFoundException, got {:?}",
                refused.error_code()
            );
            assert_eq!(
                refused.message(),
                format!("Map not found: {map}."),
                "case {case:?} ({map:?}, {path:?}): expected the map-not-found sentence, got {:?}",
                refused.message()
            );
        }
    }
}

#[tokio::test]
async fn a_tile_read_changes_nothing_a_later_caller_sees() {
    let router = emulator();
    create_map(&router, "UntouchedTileMap", "VectorGrabStandardLight").await;

    let before = call(&router, "GET", "/maps/v0/maps/UntouchedTileMap", None).await;
    for (z, x, y) in [("0", "0", "0"), ("6", "17", "23"), ("20", "1", "1")] {
        let served = get_tile(&router, "UntouchedTileMap", z, x, y).await;
        assert_eq!(
            served.status, 200,
            "case {z}/{x}/{y}: expected 200, got {}",
            served.status
        );
    }
    // A refused address, and a name no map is stored under, must not leave
    // anything behind either.
    let _ = get_tile(&router, "UntouchedTileMap", "30", "0", "0").await;
    let _ = get_tile(&router, "UntouchedTileMap", "2", "9", "0").await;
    let _ = get_tile(&router, "AbsentMap", "0", "0", "0").await;
    let _ = style_descriptor(&router, "UntouchedTileMap").await;
    let after = call(&router, "GET", "/maps/v0/maps/UntouchedTileMap", None).await;

    assert_eq!(
        before.json(),
        after.json(),
        "a tile read must leave the map resource exactly as it was"
    );
    assert_eq!(
        millis_of(&before.json(), "UpdateTime"),
        millis_of(&after.json(), "UpdateTime"),
        "a tile read must not move the map's UpdateTime"
    );
}
