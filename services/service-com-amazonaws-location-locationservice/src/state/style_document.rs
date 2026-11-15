// SPDX-License-Identifier: CC0-1.0

//! The style descriptor a map resource publishes: what a client has to know to
//! render the tiles the map serves.
//!
//! The `GetMapStyleDescriptor` doc says what the document IS: "The style
//! descriptor contains speciﬁcations on how features render on a map. For
//! example, what data to display, what order to display the data in, and the
//! style for the data. Style descriptors follow the Mapbox Style
//! Specification." So the document below is a Mapbox/MapLibre style (version 8):
//! a source naming the map's own tile URL template and the zoom range it has
//! tiles for, then one layer per layer those tiles actually carry, in draw
//! order.
//!
//! Everything in it is derived from the map resource's stored configuration and
//! the tile module beside it — the zoom range is [`super::MapStyle::max_zoom`],
//! which `GetMapTile` enforces, and the layers are
//! [`super::map_tiles::tile_layers`], which `GetMapTile` encodes — so what the
//! style DECLARES and what a tile read SERVES cannot drift apart.

use super::map_tiles::{self, Ground, LayerGeometry, TileLayer};
use super::MapStyle;

/// The media type a style descriptor is served with. The
/// `GetMapStyleDescriptorResponse$ContentType` doc: "The style descriptor's
/// content type. For example, application/json."
pub const STYLE_CONTENT_TYPE: &str = "application/json";

/// The Mapbox Style Specification version this document is written in: 8 is the
/// version the specification the member doc names defines.
const STYLE_SPECIFICATION_VERSION: u32 = 8;

/// The source name a style document reads the map's own tiles from.
pub const BASEMAP_SOURCE: &str = "basemap";

/// The `GetMapTile` URI as the model lays it out, with the `{z}/{x}/{y}`
/// placeholders a style's tile template uses in place of the `{Z}/{X}/{Y}` path
/// labels.
fn tiles_template(endpoint: &str, map_name: &str) -> String {
    format!("{endpoint}/maps/v0/maps/{map_name}/tiles/{{z}}/{{x}}/{{y}}")
}

/// The `GetMapSprites` URI, up to the file name: a style's `sprite` member names
/// the sheet's BASE, and a renderer appends `.json`, `.png` and the `@2x`
/// variants — which are exactly the four names that operation publishes.
fn sprite_base(endpoint: &str, map_name: &str) -> String {
    format!("{endpoint}/maps/v0/maps/{map_name}/sprites/sprites")
}

/// The style descriptor of the map named `map_name` carrying `style`, as the
/// bytes the operation answers with.
///
/// `endpoint` is the base URL this emulator is served at, so the templates in
/// the document lead back to this emulator's own `GetMapTile` and
/// `GetMapSprites`.
pub fn style_descriptor(map_name: &str, style: &MapStyle, endpoint: &str) -> Vec<u8> {
    let palette = map_tiles::palette(style.data_source);
    let mut document = ::serde_json::Map::new();
    document.insert(
        "version".to_string(),
        STYLE_SPECIFICATION_VERSION.into(),
    );
    document.insert("name".to_string(), style.style.into());
    document.insert(
        "sources".to_string(),
        ::serde_json::Value::Object(sources(map_name, style, endpoint)),
    );
    document.insert(
        "sprite".to_string(),
        sprite_base(endpoint, map_name).into(),
    );
    document.insert(
        "layers".to_string(),
        ::serde_json::Value::Array(layers(style, &palette)),
    );
    // Serialization of a document built in memory cannot fail: every value is a
    // string, a finite number or a nested object of those.
    ::serde_json::to_vec(&::serde_json::Value::Object(document)).unwrap_or_default()
}

/// The one source a style reads: the map's own tiles, with the zoom range they
/// exist for and the provider their data is credited to.
fn sources(
    map_name: &str,
    style: &MapStyle,
    endpoint: &str,
) -> ::serde_json::Map<String, ::serde_json::Value> {
    let mut source = ::serde_json::Map::new();
    let raster = map_tiles::tile_format(style.style) == map_tiles::TileFormat::Raster;
    source.insert(
        "type".to_string(),
        if raster { "raster" } else { "vector" }.into(),
    );
    source.insert(
        "tiles".to_string(),
        ::serde_json::Value::Array(vec![tiles_template(endpoint, map_name).into()]),
    );
    source.insert("minzoom".to_string(), map_tiles::MIN_ZOOM.into());
    source.insert("maxzoom".to_string(), style.max_zoom.into());
    source.insert("attribution".to_string(), attribution(style).into());
    if raster {
        // The side of the images `GetMapTile` answers with, which a renderer
        // needs to place them.
        source.insert("tileSize".to_string(), map_tiles::RASTER_TILE_SIZE.into());
    }
    let mut sources = ::serde_json::Map::new();
    sources.insert(
        BASEMAP_SOURCE.to_string(),
        ::serde_json::Value::Object(source),
    );
    sources
}

/// The credit line the map's data is served under: the provider the style's
/// tiles are sourced from, which is the member every map read reports as its
/// `DataSource`.
fn attribution(style: &MapStyle) -> String {
    format!("\u{a9} {}", style.data_source)
}

/// The layers a renderer draws, in draw order: the ground, then one layer per
/// layer the tiles carry.
///
/// A raster basemap's tiles are images, so there is nothing to paint per layer:
/// the single `raster` layer draws the imagery itself.
fn layers(
    style: &MapStyle,
    palette: &map_tiles::TilePalette,
) -> Vec<::serde_json::Value> {
    if map_tiles::tile_format(style.style) == map_tiles::TileFormat::Raster {
        return vec![::serde_json::json!({
            "id": "basemap",
            "type": "raster",
            "source": BASEMAP_SOURCE,
        })];
    }

    let mut layers = vec![::serde_json::json!({
        "id": "ground",
        "type": "background",
        "paint": { "background-color": map_tiles::hex(palette.land) },
    })];
    for layer in map_tiles::tile_layers() {
        layers.push(vector_layer(layer, palette));
    }
    layers
}

/// One vector layer of the style: the source layer the tiles carry it in, the
/// kind of geometry it holds, the lowest zoom the tiles draw it at, and the
/// paint for it.
fn vector_layer(
    layer: &TileLayer,
    palette: &map_tiles::TilePalette,
) -> ::serde_json::Value {
    let colour = match layer.ground {
        // Water is painted in the water colour; everything else is land
        // detail, drawn in the relief colour so it reads against the ground.
        Ground::Water => palette.water,
        Ground::Any | Ground::Land => palette.relief,
    };
    let colour = map_tiles::hex(colour);
    let mut entry = ::serde_json::Map::new();
    entry.insert("id".to_string(), layer.name.into());
    entry.insert("source".to_string(), BASEMAP_SOURCE.into());
    entry.insert("source-layer".to_string(), layer.name.into());
    match layer.geometry {
        LayerGeometry::Area => {
            entry.insert("type".to_string(), "fill".into());
            entry.insert(
                "paint".to_string(),
                ::serde_json::json!({ "fill-color": colour }),
            );
        }
        LayerGeometry::Way => {
            entry.insert("type".to_string(), "line".into());
            entry.insert(
                "paint".to_string(),
                ::serde_json::json!({ "line-color": colour, "line-width": 1 }),
            );
        }
        LayerGeometry::Point => {
            // A circle rather than a symbol: a symbol layer needs a glyph
            // range, and the layer carries the feature's `name` attribute for a
            // client that wants to label it itself.
            entry.insert("type".to_string(), "circle".into());
            entry.insert(
                "paint".to_string(),
                ::serde_json::json!({ "circle-color": colour, "circle-radius": 3 }),
            );
        }
    }
    if layer.min_zoom > map_tiles::MIN_ZOOM {
        entry.insert("minzoom".to_string(), layer.min_zoom.into());
    }
    ::serde_json::Value::Object(entry)
}
