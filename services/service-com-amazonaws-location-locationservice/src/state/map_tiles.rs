// SPDX-License-Identifier: CC0-1.0

//! The tile pyramid a map resource publishes, and the bytes one tile of it
//! carries.
//!
//! The `GetMapTile` doc states how a tile is addressed: "they're addressed using
//! a grid arrangement with an X coordinate, Y coordinate, and Z (zoom) level.
//! The origin (0, 0) is the top left of the map. Increasing the zoom level by 1
//! doubles both the X and Y dimensions, so a tile containing data for the entire
//! world at (0/0/0) will be split into 4 tiles at zoom 1". So zoom `Z` has
//! `2^Z` tiles on each axis, the tile at `(Z, X, Y)` covers a known piece of
//! ground, and an `X` or `Y` at or past that axis addresses no tile at all. How
//! DEEP the pyramid goes is a property of the map's own style, which
//! [`super::MapStyle::max_zoom`] carries beside that style's data provider.
//!
//! Nothing is stored per tile: every function here is a pure function of the map
//! resource's stored configuration (the style it was created with and the
//! provider that style's tiles are sourced from) and the requested address, so a
//! repeat read of one tile answers the same bytes and a tile read writes nothing
//! at all.
//!
//! The ground a tile draws is this emulator's own: the API publishes no map
//! data, so the shapes below come from one smooth field over the globe
//! ([`ground_field`]) sampled over the tile's own extent — which is what makes
//! two different tiles of one map carry different features, and one tile of two
//! maps whose styles come from different providers carry different colours.

use super::map_sprites::{Canvas, Rgba};
use ::vector_tile::{GeomType, GeometryWriter, LayerBuilder, TagValue, Tile};

/// The member a refusal about the requested zoom names, as the model spells it
/// on `GetMapTileRequest`.
pub const ZOOM_MEMBER: &str = "Z";

/// The member a refusal about the requested column names.
pub const X_MEMBER: &str = "X";

/// The member a refusal about the requested row names.
pub const Y_MEMBER: &str = "Y";

/// The pattern the model declares on `GetMapTileRequest$Z`, `$X` and `$Y`
/// (`smithy_query(query="operation",operation="GetMapTile")`), which a tile
/// address label has to satisfy to name a number at all.
pub const TILE_LABEL_PATTERN: &str = r"\d+";

/// The shallowest zoom of every pyramid: the single tile covering "data for the
/// entire world at (0/0/0)" the operation's own doc describes.
pub const MIN_ZOOM: u32 = 0;

/// The media type EVERY tile of this plane is served with, whatever the bytes
/// inside it encode.
///
/// The `GetMapTileResponse$ContentType` doc names a media type only "for
/// example" and the model fixes no value, so what this operation really labels
/// a tile with is recorded behaviour: a v1 map tile is handed back as the
/// provider's own opaque bytes and labelled as the binary payload it is — the
/// same media type the `GetMapGlyphsResponse$ContentType` doc names for the
/// other binary asset of this plane ("For example, application/octet-stream").
/// The label is therefore a property of the OPERATION rather than of the
/// style's tile format: a raster basemap's tile carries it exactly as a vector
/// basemap's does.
pub const TILE_CONTENT_TYPE: &str = "application/octet-stream";

/// The `Cache-Control` directive every tile is served with.
///
/// A tile is a function of the map's stored style and the tile's address alone,
/// so it only changes when the map resource does; the freshness window is this
/// emulator's choice — the model documents the member as "The HTTP Cache-Control
/// directive for the value" and publishes no value for it.
pub const TILE_CACHE_CONTROL: &str = "max-age=86400";

/// The coordinate grid a vector tile's geometry is measured on, over the whole
/// tile: 4096 is the Mapbox Vector Tile specification's own default extent.
pub const TILE_EXTENT: u32 = 4096;

/// The side of a raster tile, in pixels: the Web Mercator tile size the `Z`/`X`/
/// `Y` grid in the operation's doc is defined over.
pub const RASTER_TILE_SIZE: u32 = 256;

/// The number a `{Z}`, `{X}` or `{Y}` label addresses, or `None` when the label
/// does not spell a decimal number.
///
/// The model's pattern on these three members is unanchored, so a label that
/// merely CONTAINS digits reaches the handler; this is what refuses the rest.
/// A label of more digits than any address needs saturates instead of failing
/// here: such a label is outside the pyramid, which the range checks below
/// refuse with the sentence that names the bound it missed.
pub fn tile_label(raw: &str) -> Option<u64> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(raw.parse::<u64>().unwrap_or(u64::MAX))
}

/// The sentence a `Z` deeper than the zoom range the map's style publishes is
/// refused with: the member as the model spells it in single quotes, then the
/// bound it missed — named as the maximum zoom OF THE STYLE the addressed map
/// carries, with that style's own name and its deepest zoom in parentheses, and
/// closed with a period.
///
/// The ceiling this operation applies is not a property of the request but of
/// the map resource, so the sentence names the style it came from: a caller
/// refused here learns both WHICH style's ceiling was applied and what that
/// ceiling is, and so can clamp its zoom or pick another style from the refusal
/// alone. The number is the one the MAP's style publishes, which is also the
/// `maxzoom` its style descriptor declares, so the refusal and the document a
/// client renders from state one pyramid. The requested value is never echoed:
/// `Z` targets a `@sensitive` shape.
pub fn zoom_out_of_range(style: &str, max_zoom: u32) -> String {
    format!("'{ZOOM_MEMBER}' must not be greater than maximum zoom of style {style} ({max_zoom}).")
}

/// The sentence an `X` or `Y` outside the grid of the requested zoom is refused
/// with: the member in single quotes, then the whole range of columns (or rows)
/// the requested zoom defines — stated both as the rule it follows, `0..2^Zoom-1`,
/// and as the pair of numbers that rule works out to for this request — closed
/// with a period.
///
/// A zoom level's axis holds `2^Z` tiles, so its last column and row is
/// `axis - 1`; the sentence states the computed RANGE rather than only its top,
/// so a caller learns the whole set of addresses the zoom it asked for admits.
/// The value the request sent is not echoed, since `X` and `Y` target
/// `@sensitive` shapes.
pub fn axis_out_of_range(member: &str, axis_tiles: u64) -> String {
    format!(
        "'{member}' must be an integer in the range 0..2^Zoom-1 (0..{}).",
        axis_tiles.saturating_sub(1)
    )
}

/// One tile's address in the pyramid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileAddress {
    pub z: u32,
    pub x: u32,
    pub y: u32,
}

impl TileAddress {
    /// How many tiles one axis holds at `zoom`: `2^Z`, the doubling the
    /// operation's doc describes.
    ///
    /// Capped at the shift a `u64` can take, so the function is total for every
    /// zoom a caller could name; a zoom that deep is already outside every
    /// style's published range.
    pub fn axis_tiles(zoom: u32) -> u64 {
        1_u64 << zoom.min(63)
    }

    /// The `[longitude, latitude]` of a point inside this tile, given where it
    /// sits across (`across`) and down (`down`) the tile as fractions in `0..1`
    /// — the inverse of the Web Mercator projection the `Z`/`X`/`Y` grid is laid
    /// out in, with the origin at the top left as the doc states.
    pub fn position(&self, across: f64, down: f64) -> (f64, f64) {
        let axis = Self::axis_tiles(self.z) as f64;
        let nx = (f64::from(self.x) + across) / axis;
        let ny = (f64::from(self.y) + down) / axis;
        let longitude = nx * 360.0 - 180.0;
        let latitude = (std::f64::consts::PI * (1.0 - 2.0 * ny))
            .sinh()
            .atan()
            .to_degrees();
        (longitude, latitude)
    }

    /// The ground at the middle of this tile, which is what decides which
    /// features it carries.
    pub fn centre(&self) -> (f64, f64) {
        self.position(0.5, 0.5)
    }
}

/// Which kind of tile a style's basemap is drawn from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileFormat {
    /// Vector data, as Mapbox Vector Tile bytes.
    Vector,
    /// An image of the ground.
    Raster,
}

/// The kind of tile the style named `style` serves.
///
/// The `MapConfiguration$Style` doc describes each style as one or the other and
/// spells the kind at the front of every style name: "RasterEsriImagery – The
/// Esri Imagery map style. A raster basemap", "HybridHereExploreSatellite – A
/// global map displaying the road network, street names, and city labels over
/// satellite imagery", against the `Vector…` styles' "vector basemap". A hybrid
/// style's ground IS that satellite imagery, so it is served as a raster tile.
pub fn tile_format(style: &str) -> TileFormat {
    if style.starts_with("Raster") || style.starts_with("Hybrid") {
        TileFormat::Raster
    } else {
        TileFormat::Vector
    }
}

/// The bytes of the tile at `address` of a map whose style is `style`, sourced
/// from `data_source`: vector data for a vector basemap, an image for a raster
/// one.
pub fn tile_bytes(style: &str, data_source: &str, address: TileAddress) -> Vec<u8> {
    match tile_format(style) {
        TileFormat::Vector => vector_tile_bytes(style, data_source, address),
        TileFormat::Raster => raster_tile_bytes(data_source, address),
    }
}

/// What a tile layer's features are drawn as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerGeometry {
    /// A filled area: a water body, a park, a building footprint.
    Area,
    /// A line: a road, a railway, a boundary.
    Way,
    /// A point: a city, a landmark.
    Point,
}

/// Which ground a layer's features are drawn over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ground {
    /// Drawn wherever the tile lies.
    Any,
    /// Drawn only where the tile's ground is land.
    Land,
    /// Drawn only where the tile's ground is water.
    Water,
}

/// Which of the provider's three colours a layer's features are drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tint {
    /// The provider's water colour.
    Water,
    /// The provider's ground colour: the land every other feature is drawn on
    /// top of.
    Ground,
    /// The provider's relief colour, which land detail is drawn in so it reads
    /// against the ground.
    Detail,
}

/// One layer a vector tile carries: its name, what its features are drawn as,
/// the ground they are drawn over, the colour a style paints them in, and the
/// lowest zoom they are drawn at.
#[derive(Debug, Clone, Copy)]
pub struct TileLayer {
    pub name: &'static str,
    pub geometry: LayerGeometry,
    pub ground: Ground,
    pub tint: Tint,
    pub min_zoom: u32,
}

/// The layers a vector tile of this API carries.
///
/// The names and what each one holds come from the `MapConfiguration$Style`
/// doc's own account of what these basemaps include: "The map includes highways
/// major roads, minor roads, railways, water features, cities, parks, landmarks,
/// building footprints, and administrative boundaries." Which zoom each layer
/// comes in at is this emulator's schema: a basemap draws its coarse features
/// (water, boundaries, cities) at every zoom and its fine ones only once the
/// tiles are small enough to hold them.
///
/// The ground comes FIRST because it is a layer of the tile data like any
/// other: a renderer paints the land from the `land` polygon the tile carries,
/// which is what lets every layer of the style document be bound to the source
/// the document declares rather than to a background of the style's own.
const TILE_LAYERS: [TileLayer; 9] = [
    TileLayer {
        name: "land",
        geometry: LayerGeometry::Area,
        ground: Ground::Land,
        tint: Tint::Ground,
        min_zoom: 0,
    },
    TileLayer {
        name: "water",
        geometry: LayerGeometry::Area,
        ground: Ground::Water,
        tint: Tint::Water,
        min_zoom: 0,
    },
    TileLayer {
        name: "boundaries",
        geometry: LayerGeometry::Way,
        ground: Ground::Any,
        tint: Tint::Detail,
        min_zoom: 0,
    },
    TileLayer {
        name: "places",
        geometry: LayerGeometry::Point,
        ground: Ground::Land,
        tint: Tint::Detail,
        min_zoom: 0,
    },
    TileLayer {
        name: "roads",
        geometry: LayerGeometry::Way,
        ground: Ground::Land,
        tint: Tint::Detail,
        min_zoom: 5,
    },
    TileLayer {
        name: "railways",
        geometry: LayerGeometry::Way,
        ground: Ground::Land,
        tint: Tint::Detail,
        min_zoom: 7,
    },
    TileLayer {
        name: "parks",
        geometry: LayerGeometry::Area,
        ground: Ground::Land,
        tint: Tint::Detail,
        min_zoom: 10,
    },
    TileLayer {
        name: "landmarks",
        geometry: LayerGeometry::Point,
        ground: Ground::Land,
        tint: Tint::Detail,
        min_zoom: 12,
    },
    TileLayer {
        name: "buildings",
        geometry: LayerGeometry::Area,
        ground: Ground::Land,
        tint: Tint::Detail,
        min_zoom: 14,
    },
];

/// The layers a vector tile at `zoom` over the given ground carries, in the
/// order the tile encodes them: a layer comes in at its own zoom, and is drawn
/// only over the ground it belongs to.
///
/// The ONE place that rule is applied, so the tile a read answers and the layers
/// the style document names cannot disagree about which of them a zoom draws.
fn carried_layers(zoom: u32, over_water: bool) -> impl Iterator<Item = &'static TileLayer> {
    TILE_LAYERS
        .iter()
        .filter(move |layer| zoom >= layer.min_zoom && layer.ground != unreachable_ground(over_water))
}

/// Every layer a vector tile of this API can carry, in the order a tile encodes
/// them.
///
/// The style document a map publishes paints exactly these layers, so the
/// `source-layer` a style names and the layer a tile carries are one list rather
/// than two that can drift apart.
pub fn tile_layers() -> &'static [TileLayer] {
    &TILE_LAYERS
}

/// The names of the layers a vector tile of a map at `zoom` whose ground is land
/// or water carries, in the order the tile encodes them.
///
/// Public so a tile read's own suite can state which layers a zoom publishes
/// without restating the table above.
pub fn tile_layer_names(zoom: u32, over_water: bool) -> Vec<&'static str> {
    carried_layers(zoom, over_water)
        .map(|layer| layer.name)
        .collect()
}

/// The ground a tile does NOT lie over, so a layer bound to it is left out.
fn unreachable_ground(over_water: bool) -> Ground {
    if over_water {
        Ground::Land
    } else {
        Ground::Water
    }
}

/// Whether the ground at `(longitude, latitude)` is water.
pub fn over_water(longitude: f64, latitude: f64) -> bool {
    ground_field(longitude, latitude) < WATER_LEVEL
}

/// The value of [`ground_field`] the shoreline runs along: below it the ground
/// is water, above it land.
const WATER_LEVEL: f64 = 0.0;

/// One smooth field over the globe, standing in for the ground a tile covers:
/// below [`WATER_LEVEL`] it is water, above it land, and how far above decides
/// how high the land is drawn.
///
/// Three sinusoids of different periods, so the shoreline it traces is neither a
/// straight line nor a grid: adjacent tiles of one map carry different features
/// and one tile keeps the same ones at every zoom it is read at.
fn ground_field(longitude: f64, latitude: f64) -> f64 {
    let lon = longitude.to_radians();
    let lat = latitude.to_radians();
    0.55 * (1.7 * lon + 0.6).sin() * (1.3 * lat).cos()
        + 0.30 * (3.1 * lon - 1.2).sin() * (2.2 * lat + 0.4).cos()
        + 0.15 * (5.3 * lat).sin()
}

/// A vector tile: one layer per entry of [`TILE_LAYERS`] the requested zoom and
/// the tile's own ground admit, each carrying geometry on the tile's extent grid
/// and the attributes a renderer filters and labels by.
fn vector_tile_bytes(style: &str, data_source: &str, address: TileAddress) -> Vec<u8> {
    let extent = i32::try_from(TILE_EXTENT).unwrap_or(4096);
    let (longitude, latitude) = address.centre();
    let water = over_water(longitude, latitude);
    let mut layers = Vec::with_capacity(TILE_LAYERS.len());

    for (index, layer) in carried_layers(address.z, water).enumerate() {
        let mut builder = LayerBuilder::new(layer.name, TILE_EXTENT);
        let attributes = vec![
            (
                "name",
                TagValue::Str(format!(
                    "{} {}/{}/{}",
                    layer.name, address.z, address.x, address.y
                )),
            ),
            ("style", TagValue::Str(style.to_string())),
            ("provider", TagValue::Str(data_source.to_string())),
            ("z", TagValue::Int(i64::from(address.z))),
            ("x", TagValue::Int(i64::from(address.x))),
            ("y", TagValue::Int(i64::from(address.y))),
            ("longitude", TagValue::Double(longitude)),
            ("latitude", TagValue::Double(latitude)),
        ];
        // Each layer is drawn inside its own inset of the tile, so the layers of
        // one tile are distinguishable to a client that decodes them.
        let margin = (i32::try_from(index).unwrap_or(0) + 1) * 192;
        let margin = margin.min(extent / 2 - 64);
        let id = feature_id(address, index);

        match layer.geometry {
            LayerGeometry::Area => {
                let mut ring = GeometryWriter::new();
                ring.move_to(margin, margin);
                ring.line_to(&[
                    (extent - margin, margin),
                    (extent - margin, extent - margin),
                    (margin, extent - margin),
                ]);
                ring.close_path();
                builder.add_feature(id, GeomType::Polygon, ring.finish(), &attributes);
            }
            LayerGeometry::Way => {
                let mut line = GeometryWriter::new();
                line.move_to(0, margin);
                line.line_to(&[(extent / 2, extent / 2), (extent, extent - margin)]);
                builder.add_feature(id, GeomType::LineString, line.finish(), &attributes);
            }
            LayerGeometry::Point => {
                let mut point = GeometryWriter::new();
                point.move_to(extent / 2, extent / 2);
                builder.add_feature(id, GeomType::Point, point.finish(), &attributes);
            }
        }

        if !builder.is_empty() {
            layers.push(builder.finish());
        }
    }

    ::vector_tile::encode_tile(&Tile { layers })
}

/// A feature id unique within its tile and stable across reads of it.
fn feature_id(address: TileAddress, layer: usize) -> u64 {
    (u64::from(address.z) << 56)
        ^ (u64::from(address.x) << 32)
        ^ (u64::from(address.y) << 8)
        ^ (layer as u64)
}

/// The three colours one provider's maps are drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TilePalette {
    pub water: Rgba,
    pub land: Rgba,
    pub relief: Rgba,
}

/// That colour as the `#rrggbb` a style document paints with.
pub fn hex(colour: Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", colour[0], colour[1], colour[2])
}

/// The colour of `palette` a layer tinted `tint` is painted in.
///
/// The ONE place a tile layer's colour is decided, so the style document that
/// paints a layer and the raster imagery drawn from the same palette cannot
/// disagree about what the provider's map looks like.
pub fn tint_colour(palette: &TilePalette, tint: Tint) -> Rgba {
    match tint {
        Tint::Water => palette.water,
        Tint::Ground => palette.land,
        Tint::Detail => palette.relief,
    }
}

/// The palette a map whose style is sourced from `data_source` draws its raster
/// tiles in, and the style document it publishes paints its vector tiles with,
/// so two maps whose styles come from different providers really are served
/// different imagery rather than the same bytes.
///
/// `data_source` is the provider stored on the map resource when it was created
/// (one of the names [`super::MAP_STYLES`] carries); a value from outside that
/// catalog cannot reach here through a map resource, and takes the Open Data
/// palette rather than failing a read.
pub fn palette(data_source: &str) -> TilePalette {
    match data_source {
        super::PROVIDER_ESRI => TilePalette {
            water: [0x9f, 0xc4, 0xe0, 0xff],
            land: [0xe8, 0xe3, 0xd6, 0xff],
            relief: [0x8a, 0x7f, 0x63, 0xff],
        },
        super::PROVIDER_HERE => TilePalette {
            water: [0x8e, 0xc7, 0xcc, 0xff],
            land: [0xef, 0xea, 0xdf, 0xff],
            relief: [0x6f, 0x82, 0x5d, 0xff],
        },
        super::PROVIDER_GRAB => TilePalette {
            water: [0x9a, 0xd4, 0xc1, 0xff],
            land: [0xe4, 0xf0, 0xe2, 0xff],
            relief: [0x3f, 0x7a, 0x55, 0xff],
        },
        _ => TilePalette {
            water: [0xa8, 0xbd, 0xd8, 0xff],
            land: [0xee, 0xe9, 0xe1, 0xff],
            relief: [0x7a, 0x6e, 0x8a, 0xff],
        },
    }
}

/// A raster tile: the ground field sampled over the tile's own pixel grid, so
/// the image really is a picture of the ground this address covers.
///
/// Drawn into the crate's own RGBA surface (`super::map_sprites::Canvas`), which
/// the sprite sheet beside it is drawn into too, so one primitive encodes every
/// raster asset this service serves.
fn raster_tile_bytes(data_source: &str, address: TileAddress) -> Vec<u8> {
    let palette = palette(data_source);
    let side = RASTER_TILE_SIZE;
    let mut canvas = Canvas::new(side, side);
    let span = f64::from(side);
    for row in 0..side {
        for column in 0..side {
            let (longitude, latitude) = address.position(
                (f64::from(column) + 0.5) / span,
                (f64::from(row) + 0.5) / span,
            );
            canvas.set(column, row, ground_colour(&palette, longitude, latitude));
        }
    }
    // A one-pixel edge along the tile's top and left, which is what makes the
    // seam between two tiles visible in a rendered map.
    for index in 0..side {
        canvas.set(index, 0, palette.relief);
        canvas.set(0, index, palette.relief);
    }
    canvas.to_png()
}

/// The colour the ground at one position is drawn in: the water colour below the
/// shoreline, and land shaded towards the relief colour as the field rises.
fn ground_colour(palette: &TilePalette, longitude: f64, latitude: f64) -> Rgba {
    let height = ground_field(longitude, latitude);
    if height < WATER_LEVEL {
        return palette.water;
    }
    let share = (height / 0.8).clamp(0.0, 1.0);
    let mut colour = palette.land;
    // The alpha channel is left as the palette sets it: only the three colour
    // channels are shaded.
    for (channel, value) in colour.iter_mut().enumerate().take(3) {
        let land = f64::from(palette.land[channel]);
        let relief = f64::from(palette.relief[channel]);
        *value = (land + (relief - land) * share).round().clamp(0.0, 255.0) as u8;
    }
    colour
}

