// SPDX-License-Identifier: CC0-1.0

//! The style descriptor a map resource publishes: what a client has to know to
//! render the tiles the map serves.
//!
//! The `GetMapStyleDescriptor` doc says what the document IS: "The style
//! descriptor contains speciﬁcations on how features render on a map. For
//! example, what data to display, what order to display the data in, and the
//! style for the data. Style descriptors follow the Mapbox Style
//! Specification." So the document below is a Mapbox/MapLibre style (version
//! 8): a source named after the data provider the map's tiles come from,
//! carrying the map's own tile URL template and the zoom range it has tiles
//! for; the sprite and glyph templates a renderer loads its icons and fonts
//! from; the instants the style was created and last changed; and one layer per
//! layer those tiles carry, in draw order, each bound to that source.
//!
//! Everything in it is derived from the map resource's stored record and the
//! tile module beside it — the zoom range is [`super::MapStyle::max_zoom`],
//! which `GetMapTile` enforces, the layers are
//! [`super::map_tiles::tile_layers`], which `GetMapTile` encodes, and the
//! instants are the resource's own `CreateTime`/`UpdateTime`, which a map read
//! reports — so what this document DECLARES and what the rest of the plane
//! serves cannot drift apart.

use super::map_tiles::{self, LayerGeometry, TileLayer};
use super::{EpochMillis, MapStyle};

/// The media type a style descriptor is served with. The
/// `GetMapStyleDescriptorResponse$ContentType` doc: "The style descriptor's
/// content type. For example, application/json."
pub const STYLE_CONTENT_TYPE: &str = "application/json";

/// The Mapbox Style Specification version this document is written in: 8 is the
/// version the specification the member doc names defines.
const STYLE_SPECIFICATION_VERSION: u32 = 8;

/// The id a source is declared under when the provider's name spells no id at
/// all. Unreachable through the API — a stored map's provider always comes from
/// [`super::MAP_STYLES`] — and here so that naming a source is total.
const UNNAMED_SOURCE: &str = "basemap";

/// The map resource a style descriptor is rendered from: every value in the
/// document is read off this record, so a document is a function of what the
/// create PERSISTED and of the endpoint the request arrived at.
#[derive(Debug, Clone, Copy)]
pub struct StyledMap<'a> {
    /// The `MapName` the resource is stored under, which every template in the
    /// document addresses.
    pub name: &'a str,
    /// The catalog entry of the style the map was created with.
    pub style: MapStyle,
    /// The data provider the map's tiles are sourced from, as the map resource
    /// stored it and every map read reports it.
    pub data_source: &'a str,
    /// When the map resource was created, and so when the style it publishes
    /// came into being: the resource's own `CreateTime`.
    pub created: EpochMillis,
    /// When the map resource was last written, and so when the style it
    /// publishes last changed: the resource's own `UpdateTime`.
    pub modified: EpochMillis,
}

/// The id the map's one source is declared under: the data provider the tiles
/// come from, as a style source id — the provider's own name in lower case with
/// everything that is not a letter or digit dropped.
///
/// The model publishes no source names, so this is recorded behaviour: the
/// descriptor of an Esri-backed map declares its source as `esri`, keyed by the
/// provider rather than by a name of this service's choosing, which is what
/// lets a client that knows the map's `DataSource` find the source it reads.
pub fn source_name(data_source: &str) -> String {
    let id: String = data_source
        .chars()
        .filter(|glyph| glyph.is_ascii_alphanumeric())
        .map(|glyph| glyph.to_ascii_lowercase())
        .collect();
    if id.is_empty() {
        UNNAMED_SOURCE.to_string()
    } else {
        id
    }
}

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

/// The `GetMapGlyphs` URI as the model lays it out, with the `{fontstack}` and
/// `{range}` placeholders a style's glyph template uses in place of the
/// `{FontStack}`/`{FontUnicodeRange}` path labels: a renderer substitutes the
/// font stack a layer labels with and the 256-codepoint range it needs, and the
/// `.pbf` suffix is part of the range label's own modelled pattern
/// (`^[0-9]+-[0-9]+\.pbf$`), so it belongs to the template rather than to the
/// value substituted into it.
fn glyphs_template(endpoint: &str, map_name: &str) -> String {
    format!("{endpoint}/maps/v0/maps/{map_name}/glyphs/{{fontstack}}/{{range}}.pbf")
}

/// A stored instant as this document writes it: the ISO 8601 form every
/// timestamp of this API is rendered in (see [`super::EpochMillis`]).
///
/// The rendering of an instant the service's own clock produced cannot fail, so
/// the fallback is unreachable; it is here rather than an unwrap because a
/// handler of this crate answers a modelled response for every input and
/// panics for none.
fn rendered_instant(millis: EpochMillis) -> String {
    super::to_date_time(millis)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}

/// The style descriptor of `map`, as the bytes the operation answers with.
///
/// `endpoint` is the base URL this emulator is served at, so the templates in
/// the document lead back to this emulator's own `GetMapTile`, `GetMapSprites`
/// and `GetMapGlyphs` routes for this map resource.
pub fn style_descriptor(map: &StyledMap<'_>, endpoint: &str) -> Vec<u8> {
    let palette = map_tiles::palette(map.data_source);
    let source = source_name(map.data_source);
    let mut document = ::serde_json::Map::new();
    document.insert("version".to_string(), STYLE_SPECIFICATION_VERSION.into());
    document.insert("name".to_string(), map.style.style.into());
    document.insert(
        "sources".to_string(),
        ::serde_json::Value::Object(sources(map, &source, endpoint)),
    );
    document.insert(
        "sprite".to_string(),
        sprite_base(endpoint, map.name).into(),
    );
    document.insert(
        "glyphs".to_string(),
        glyphs_template(endpoint, map.name).into(),
    );
    document.insert(
        "layers".to_string(),
        ::serde_json::Value::Array(layers(&map.style, &source, &palette)),
    );
    document.insert("created".to_string(), rendered_instant(map.created).into());
    document.insert("modified".to_string(), rendered_instant(map.modified).into());
    // Serialization of a document built in memory cannot fail: every value is a
    // string, a finite number or a nested object of those.
    ::serde_json::to_vec(&::serde_json::Value::Object(document)).unwrap_or_default()
}

/// The one source a style reads, declared under the provider's own id: the
/// map's own tiles, with the zoom range they exist for and the provider their
/// data is credited to.
fn sources(
    map: &StyledMap<'_>,
    source: &str,
    endpoint: &str,
) -> ::serde_json::Map<String, ::serde_json::Value> {
    let mut entry = ::serde_json::Map::new();
    let raster = map_tiles::tile_format(map.style.style) == map_tiles::TileFormat::Raster;
    entry.insert(
        "type".to_string(),
        if raster { "raster" } else { "vector" }.into(),
    );
    entry.insert(
        "tiles".to_string(),
        ::serde_json::Value::Array(vec![tiles_template(endpoint, map.name).into()]),
    );
    entry.insert("minzoom".to_string(), map_tiles::MIN_ZOOM.into());
    entry.insert("maxzoom".to_string(), map.style.max_zoom.into());
    entry.insert("attribution".to_string(), attribution(map.data_source).into());
    if raster {
        // The side of the images `GetMapTile` answers with, which a renderer
        // needs to place them.
        entry.insert("tileSize".to_string(), map_tiles::RASTER_TILE_SIZE.into());
    }
    let mut sources = ::serde_json::Map::new();
    sources.insert(source.to_string(), ::serde_json::Value::Object(entry));
    sources
}

/// The credit line the map's data is served under: the provider the style's
/// tiles are sourced from, which is the member every map read reports as its
/// `DataSource`.
fn attribution(data_source: &str) -> String {
    format!("\u{a9} {data_source}")
}

/// The layers a renderer draws, in draw order: one layer per layer the tiles
/// carry, each bound to the source the document declares — the ground
/// included, since the tiles carry it as a layer of their own.
///
/// A raster basemap's tiles are images, so there is nothing to paint per layer:
/// the single `raster` layer draws the imagery itself, bound to that same
/// source.
fn layers(
    style: &MapStyle,
    source: &str,
    palette: &map_tiles::TilePalette,
) -> Vec<::serde_json::Value> {
    if map_tiles::tile_format(style.style) == map_tiles::TileFormat::Raster {
        return vec![::serde_json::json!({
            "id": source,
            "type": "raster",
            "source": source,
        })];
    }

    map_tiles::tile_layers()
        .iter()
        .map(|layer| vector_layer(layer, source, palette))
        .collect()
}

/// One vector layer of the style: the source it reads, the source layer the
/// tiles carry it in, the kind of geometry it holds, the lowest zoom the tiles
/// draw it at, and the paint for it.
fn vector_layer(
    layer: &TileLayer,
    source: &str,
    palette: &map_tiles::TilePalette,
) -> ::serde_json::Value {
    let colour = map_tiles::hex(map_tiles::tint_colour(palette, layer.tint));
    let mut entry = ::serde_json::Map::new();
    entry.insert("id".to_string(), layer.name.into());
    entry.insert("source".to_string(), source.into());
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
    entry.insert("minzoom".to_string(), layer.min_zoom.into());
    ::serde_json::Value::Object(entry)
}
