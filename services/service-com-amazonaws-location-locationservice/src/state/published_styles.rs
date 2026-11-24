// SPDX-License-Identifier: CC0-1.0

//! The published style asset a map style carries: the provider's own layer
//! catalog and the instants the asset itself reports.
//!
//! A style descriptor is PUBLISHED CONTENT of the provider's basemap style
//! rather than something a request composes: the provider publishes one document
//! per style, a client renders from it, and two reads of it — in any region, for
//! any account, at any time — hand back the same document. So the layer catalog
//! and the asset's own stamps live here as catalog data keyed by the style name
//! ([`published_style`]), and [`super::style_document`] serves that stored asset
//! with the addresses of the requesting map's own routes substituted into its
//! templates.
//!
//! A published layer names the tile layer it paints as the published tile schema
//! spells it (`Land`, `Water area`, `Road`), carries an id of the published
//! `<Source layer>/<sub-class>` form (`Land/Not ice`), selects its sub-class with
//! a [`SYMBOL_ATTRIBUTE`] predicate, and sets `layout`, `paint` and `minzoom`.
//!
//! What is pinned and what is this emulator's schema: the first entry of
//! [`PUBLISHED_TILE_SCHEMA`] — the `Land` source layer and its `Land/Not ice`
//! class at zoom 0 — is recorded behaviour of `GetMapStyleDescriptor`, as is
//! [`PUBLISHED_STYLE_INSTANT`]. The remaining source layers, their sub-classes
//! and the zoom each comes in at are this emulator's schema (the same footing as
//! [`super::MapStyle::max_zoom`] for a provider no recording covers), written in
//! the published FORM above and taken from the `MapConfiguration$Style` doc's own
//! account of what these basemaps include: "The map includes highways major
//! roads, minor roads, railways, water features, cities, parks, landmarks,
//! building footprints, and administrative boundaries."
//!
//! The schema is the ONE table `GetMapTile` encodes a tile from and
//! `GetMapStyleDescriptor` paints from, so the `source-layer` a published layer
//! names is a layer the tiles really carry and the class its filter selects is a
//! class those tiles really tag their features with.

use super::map_tiles::{Ground, LayerGeometry, Tint, TileLayer};

/// The feature attribute a published layer's filter reads to select its
/// sub-class out of the source layer it paints.
///
/// Recorded behaviour of `GetMapStyleDescriptor`: a published layer of this plane
/// filters its source layer with an `["==", "_symbol", <class>]` predicate, so
/// the class a feature belongs to is carried on the feature under this name.
pub const SYMBOL_ATTRIBUTE: &str = "_symbol";

/// The instant a published style asset reports as its own, in both `created` and
/// `modified`.
///
/// Recorded behaviour of `GetMapStyleDescriptor`: the document's `created` and
/// `modified` members are the zero-value timestamp of the published asset —
/// a FIXED constant of the asset, identical on every call, in every region and
/// every account, rather than an instant of the serving clock or of the map
/// resource that was read.
pub const PUBLISHED_STYLE_INSTANT: &str = "0001-01-01T00:00:00Z";

/// One sub-class of a source layer: the class a published layer selects out of
/// the layer it paints, and the id the published layer carries for it.
#[derive(Debug, Clone, Copy)]
pub struct PublishedClass {
    /// The value of [`SYMBOL_ATTRIBUTE`] the features of this class carry, which
    /// is what the published layer's filter compares against.
    pub symbol: i64,
    /// The published layer's id: the `<Source layer>/<sub-class>` name the
    /// published document spells it with.
    pub id: &'static str,
}

/// One entry of the published tile schema, written out so the table below is a
/// line per source layer.
const fn source_layer(
    name: &'static str,
    geometry: LayerGeometry,
    ground: Ground,
    tint: Tint,
    min_zoom: u32,
    classes: &'static [PublishedClass],
) -> TileLayer {
    TileLayer {
        name,
        geometry,
        ground,
        tint,
        min_zoom,
        classes,
    }
}

/// One sub-class, written out for the same reason.
const fn class(symbol: i64, id: &'static str) -> PublishedClass {
    PublishedClass { symbol, id }
}

/// The source layers the published tile schema carries, in the order a tile
/// encodes them and a style paints them.
///
/// The ground comes FIRST because the schema carries it as a layer like any
/// other: a renderer paints the land from the `Land` polygons the tile carries,
/// which is what lets every published layer be bound to the source the document
/// declares rather than to a background of the style's own.
pub const PUBLISHED_TILE_SCHEMA: [TileLayer; 9] = [
    source_layer(
        "Land",
        LayerGeometry::Area,
        Ground::Land,
        Tint::Ground,
        0,
        &[class(0, "Land/Not ice"), class(1, "Land/Ice")],
    ),
    source_layer(
        "Water area",
        LayerGeometry::Area,
        Ground::Water,
        Tint::Water,
        0,
        &[class(0, "Water area/Lake or river")],
    ),
    source_layer(
        "Boundary line",
        LayerGeometry::Way,
        Ground::Any,
        Tint::Detail,
        0,
        &[class(0, "Boundary line/Admin0")],
    ),
    source_layer(
        "City",
        LayerGeometry::Point,
        Ground::Land,
        Tint::Detail,
        0,
        &[class(0, "City/Town")],
    ),
    source_layer(
        "Road",
        LayerGeometry::Way,
        Ground::Land,
        Tint::Detail,
        5,
        &[class(0, "Road/Minor road"), class(1, "Road/Highway")],
    ),
    source_layer(
        "Railroad",
        LayerGeometry::Way,
        Ground::Land,
        Tint::Detail,
        7,
        &[class(0, "Railroad/Railway")],
    ),
    source_layer(
        "Openspace or forest",
        LayerGeometry::Area,
        Ground::Land,
        Tint::Detail,
        10,
        &[class(0, "Openspace or forest/Park")],
    ),
    source_layer(
        "Landmark",
        LayerGeometry::Point,
        Ground::Land,
        Tint::Detail,
        12,
        &[class(0, "Landmark/Point of interest")],
    ),
    source_layer(
        "Building",
        LayerGeometry::Area,
        Ground::Land,
        Tint::Detail,
        14,
        &[class(0, "Building/Building")],
    ),
];

/// The published style asset of one map style: the layer catalog the provider
/// publishes for it and the instants the asset carries.
///
/// A raster basemap's published asset paints the imagery itself rather than
/// features of a tile schema, so its [`Self::tile_schema`] is empty: there are no
/// source layers to select classes out of.
#[derive(Debug, Clone, Copy)]
pub struct PublishedStyle {
    /// The style this asset is published for, spelled as the catalog spells it.
    pub style: &'static str,
    /// The source layers the asset's layers paint, in draw order.
    pub tile_schema: &'static [TileLayer],
    /// The instant the asset reports as its creation.
    pub created: &'static str,
    /// The instant the asset reports as its last modification.
    pub modified: &'static str,
}

impl PublishedStyle {
    /// The published layers of this asset, in draw order: one per sub-class of
    /// each source layer the schema carries.
    pub fn layers(&self) -> Vec<PublishedLayer> {
        self.tile_schema
            .iter()
            .flat_map(|layer| {
                layer.classes.iter().map(move |class| PublishedLayer {
                    id: class.id,
                    source_layer: layer.name,
                    symbol: class.symbol,
                    geometry: layer.geometry,
                    tint: layer.tint,
                    min_zoom: layer.min_zoom,
                })
            })
            .collect()
    }
}

/// One published layer of a style asset: everything the document states about it
/// apart from the source id, which is the provider the reading map's tiles are
/// credited to.
#[derive(Debug, Clone, Copy)]
pub struct PublishedLayer {
    pub id: &'static str,
    pub source_layer: &'static str,
    /// The class of the source layer this layer paints, which its filter selects
    /// on [`SYMBOL_ATTRIBUTE`].
    pub symbol: i64,
    pub geometry: LayerGeometry,
    pub tint: Tint,
    pub min_zoom: u32,
}

/// The published style asset for `style`, or `None` when no published style is
/// spelled that way.
///
/// Keyed by the style NAME, which is what a map resource stores: a style read
/// looks the map's own style up here and serves the asset it finds, so two maps
/// of one style publish one document and a repeat read publishes it again
/// unchanged.
pub fn published_style(style: &str) -> Option<PublishedStyle> {
    let entry = super::map_style(style)?;
    let raster = super::map_tiles::tile_format(entry.style) == super::map_tiles::TileFormat::Raster;
    Some(PublishedStyle {
        style: entry.style,
        tile_schema: if raster { &[] } else { &PUBLISHED_TILE_SCHEMA },
        created: PUBLISHED_STYLE_INSTANT,
        modified: PUBLISHED_STYLE_INSTANT,
    })
}
