// SPDX-License-Identifier: CC0-1.0

//! Mapbox Vector Tile encoding, shared by every service whose tiles are MVT.
//!
//! Two services serve vector tiles — the Amazon Location Service maps plane and
//! the Maps V2 plane beside it — and a tile either service answers with has to
//! decode in the same readers, so the FORMAT lives here once rather than once
//! per service crate.
//!
//! The messages below are the `vector_tile.proto` schema of the Mapbox Vector
//! Tile specification 2.1 transcribed as `prost` messages: the protobuf codec
//! itself is `prost`'s, not written here. What this crate adds on top of it is
//! the part of the format that is NOT protobuf — the command-and-parameter
//! geometry stream (§4.3 of the specification) and the key/value interning a
//! layer's attributes use (§4.4) — so a caller hands over extent coordinates and
//! attribute pairs and never touches an index or a zig-zag.

/// A whole tile: `repeated Layer layers = 3`.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Tile {
    #[prost(message, repeated, tag = "3")]
    pub layers: Vec<Layer>,
}

/// One layer of a tile: its name, its features, and the interned keys and values
/// those features' attributes index into.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Layer {
    /// `required uint32 version = 15` — 2 for specification 2.x.
    #[prost(uint32, tag = "15")]
    pub version: u32,
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(message, repeated, tag = "2")]
    pub features: Vec<Feature>,
    #[prost(string, repeated, tag = "3")]
    pub keys: Vec<String>,
    #[prost(message, repeated, tag = "4")]
    pub values: Vec<Value>,
    /// The layer's coordinate extent: the integer grid its geometry is measured
    /// in, over the whole tile. 4096 is the specification's default.
    #[prost(uint32, tag = "5")]
    pub extent: u32,
}

/// One feature: its attribute indices and its encoded geometry.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Feature {
    #[prost(uint64, tag = "1")]
    pub id: u64,
    /// Pairs of (key index, value index) into the layer's `keys` and `values`.
    #[prost(uint32, repeated, tag = "2")]
    pub tags: Vec<u32>,
    #[prost(enumeration = "GeomType", tag = "3")]
    pub r#type: i32,
    /// The command-and-parameter stream (see [`GeometryWriter`]).
    #[prost(uint32, repeated, tag = "4")]
    pub geometry: Vec<u32>,
}

/// One attribute value. The specification makes every variant optional and sets
/// exactly one per value.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Value {
    #[prost(string, optional, tag = "1")]
    pub string_value: Option<String>,
    #[prost(float, optional, tag = "2")]
    pub float_value: Option<f32>,
    #[prost(double, optional, tag = "3")]
    pub double_value: Option<f64>,
    #[prost(int64, optional, tag = "4")]
    pub int_value: Option<i64>,
    #[prost(uint64, optional, tag = "5")]
    pub uint_value: Option<u64>,
    #[prost(sint64, optional, tag = "6")]
    pub sint_value: Option<i64>,
    #[prost(bool, optional, tag = "7")]
    pub bool_value: Option<bool>,
}

/// `Tile.GeomType`: which kind of geometry a feature's stream draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum GeomType {
    Unknown = 0,
    Point = 1,
    LineString = 2,
    Polygon = 3,
}

/// The protobuf bytes of `tile`: what a tile read answers with.
///
/// The codec seam of this crate, so a caller encodes a tile without naming
/// `prost` itself.
pub fn encode_tile(tile: &Tile) -> Vec<u8> {
    ::prost::Message::encode_to_vec(tile)
}

/// The tile `bytes` carry, or the reason they are not one.
///
/// The decoding half of [`encode_tile`], for a caller — a test, a renderer —
/// reading a tile back.
pub fn decode_tile(bytes: &[u8]) -> Result<Tile, ::prost::DecodeError> {
    <Tile as ::prost::Message>::decode(bytes)
}

/// The geometry commands the specification defines (§4.3.3).
const CMD_MOVE_TO: u32 = 1;
const CMD_LINE_TO: u32 = 2;
const CMD_CLOSE_PATH: u32 = 7;

/// Builds one feature's command-and-parameter geometry stream.
///
/// Coordinates are layer-extent integers, and every command's parameters are
/// deltas from the cursor the previous command left, zig-zag encoded — §4.3.2 of
/// the specification.
#[derive(Debug, Default)]
pub struct GeometryWriter {
    stream: Vec<u32>,
    cursor_x: i32,
    cursor_y: i32,
}

impl GeometryWriter {
    /// A writer whose cursor sits at the tile's own origin, as §4.3.2 starts it.
    pub fn new() -> Self {
        Self::default()
    }

    /// `MoveTo` one extent coordinate: the start of a point, a line or a ring.
    pub fn move_to(&mut self, x: i32, y: i32) {
        self.stream.push(command(CMD_MOVE_TO, 1));
        self.push_delta(x, y);
    }

    /// `LineTo` each of `points`, in order. A no-op for an empty slice, which
    /// keeps a zero-count command — which the specification forbids — out of the
    /// stream.
    pub fn line_to(&mut self, points: &[(i32, i32)]) {
        if points.is_empty() {
            return;
        }
        self.stream
            .push(command(CMD_LINE_TO, u32::try_from(points.len()).unwrap_or(0)));
        for &(x, y) in points {
            self.push_delta(x, y);
        }
    }

    /// `ClosePath`, which takes no parameters and ends a polygon's ring.
    pub fn close_path(&mut self) {
        self.stream.push(command(CMD_CLOSE_PATH, 1));
    }

    /// The finished stream.
    pub fn finish(self) -> Vec<u32> {
        self.stream
    }

    fn push_delta(&mut self, x: i32, y: i32) {
        let dx = x.wrapping_sub(self.cursor_x);
        let dy = y.wrapping_sub(self.cursor_y);
        self.stream.push(zigzag(dx));
        self.stream.push(zigzag(dy));
        self.cursor_x = x;
        self.cursor_y = y;
    }
}

/// A command integer: the command id in the low three bits, the repeat count
/// above them.
///
/// The count is capped at the 29 bits the layout leaves for it, so a count that
/// cannot be represented truncates rather than wrapping into the command id and
/// producing a stream no reader can follow.
fn command(id: u32, count: u32) -> u32 {
    const MAX_COUNT: u32 = (1 << 29) - 1;
    (id & 0x7) | (count.min(MAX_COUNT) << 3)
}

/// Zig-zag encoding, so a small negative delta stays a small varint.
fn zigzag(value: i32) -> u32 {
    ((value << 1) ^ (value >> 31)) as u32
}

/// An attribute value a feature carries, in the three forms these tiles use.
#[derive(Debug, Clone, PartialEq)]
pub enum TagValue {
    Str(String),
    Int(i64),
    Double(f64),
}

/// Accumulates one layer, interning attribute keys and values the way the
/// specification requires: a feature stores INDICES into the layer's tables, not
/// the strings themselves, so a key repeated across features is written once.
#[derive(Debug)]
pub struct LayerBuilder {
    name: String,
    extent: u32,
    features: Vec<Feature>,
    keys: Vec<String>,
    values: Vec<Value>,
}

impl LayerBuilder {
    /// An empty layer named `name`, whose geometry is measured on an `extent`
    /// grid.
    pub fn new(name: &str, extent: u32) -> Self {
        Self {
            name: name.to_string(),
            extent,
            features: Vec::new(),
            keys: Vec::new(),
            values: Vec::new(),
        }
    }

    /// Append one feature with `attributes`, interning each key and value.
    pub fn add_feature(
        &mut self,
        id: u64,
        geom_type: GeomType,
        geometry: Vec<u32>,
        attributes: &[(&str, TagValue)],
    ) {
        let mut tags = Vec::with_capacity(attributes.len() * 2);
        for (key, value) in attributes {
            tags.push(self.intern_key(key));
            tags.push(self.intern_value(value));
        }
        self.features.push(Feature {
            id,
            tags,
            r#type: geom_type as i32,
            geometry,
        });
    }

    /// Whether anything has been added: a layer with no features belongs out of
    /// the tile rather than in it encoded empty.
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// The finished layer.
    pub fn finish(self) -> Layer {
        Layer {
            // Specification 2.x layers declare version 2.
            version: 2,
            name: self.name,
            features: self.features,
            keys: self.keys,
            values: self.values,
            extent: self.extent,
        }
    }

    fn intern_key(&mut self, key: &str) -> u32 {
        if let Some(index) = self.keys.iter().position(|existing| existing == key) {
            return index as u32;
        }
        self.keys.push(key.to_string());
        (self.keys.len() - 1) as u32
    }

    fn intern_value(&mut self, value: &TagValue) -> u32 {
        let mut encoded = Value::default();
        match value {
            TagValue::Str(text) => encoded.string_value = Some(text.clone()),
            TagValue::Int(number) => encoded.int_value = Some(*number),
            TagValue::Double(number) => encoded.double_value = Some(*number),
        }
        if let Some(index) = self.values.iter().position(|existing| *existing == encoded) {
            return index as u32;
        }
        self.values.push(encoded);
        (self.values.len() - 1) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a feature's geometry is a command-and-parameter stream whose
    /// parameters are zig-zag encoded deltas from the cursor (§4.3.2), and a
    /// layer's attributes are interned once however many features share them
    /// (§4.4). The table spans what the encoding generalises over: a point, a
    /// line of several vertices and a closed ring, each with a repeated key and
    /// a repeated value.
    #[test]
    fn a_tile_round_trips_through_the_codec() {
        let extent = 4096;
        let mut builder = LayerBuilder::new("water", extent);
        assert!(builder.is_empty(), "a fresh layer carries no feature");

        let mut point = GeometryWriter::new();
        point.move_to(2048, 2048);
        builder.add_feature(
            1,
            GeomType::Point,
            point.finish(),
            &[("kind", TagValue::Str("lake".to_string())), ("z", TagValue::Int(4))],
        );

        let mut line = GeometryWriter::new();
        line.move_to(0, 10);
        line.line_to(&[(100, 10), (100, 210)]);
        builder.add_feature(
            2,
            GeomType::LineString,
            line.finish(),
            // The same key and the same value as the point above: both are
            // interned once.
            &[("kind", TagValue::Str("lake".to_string())), ("z", TagValue::Int(9))],
        );

        let mut ring = GeometryWriter::new();
        ring.move_to(10, 10);
        ring.line_to(&[(20, 10), (20, 20)]);
        ring.close_path();
        builder.add_feature(3, GeomType::Polygon, ring.finish(), &[]);

        assert!(!builder.is_empty(), "three features were added");
        let encoded = encode_tile(&Tile {
            layers: vec![builder.finish()],
        });
        let decoded = decode_tile(&encoded).expect("the codec's own bytes decode");
        let [layer] = decoded.layers.as_slice() else {
            panic!("expected one layer, got {:?}", decoded.layers);
        };
        assert_eq!(
            (layer.version, layer.extent, layer.name.as_str()),
            (2, extent, "water"),
            "case \"layer header\": expected specification 2 at extent {extent}, got {layer:?}"
        );
        assert_eq!(
            layer.keys,
            vec!["kind".to_string(), "z".to_string()],
            "case \"interning\": a repeated key is written once, got {:?}",
            layer.keys
        );
        assert_eq!(
            layer.values.len(),
            3,
            "case \"interning\": the repeated value is written once, got {:?}",
            layer.values
        );

        // MoveTo(1) then the zig-zagged delta from the origin: 2048 -> 4096.
        let point_geometry = &layer.features[0].geometry;
        assert_eq!(
            point_geometry,
            &vec![9_u32, 4096, 4096],
            "case \"point\": expected one MoveTo and its deltas, got {point_geometry:?}"
        );
        // MoveTo(1), its deltas, then LineTo(2) and two deltas, the second
        // relative to the first.
        let line_geometry = &layer.features[1].geometry;
        assert_eq!(
            line_geometry,
            &vec![9_u32, 0, 20, 18, 200, 0, 0, 400],
            "case \"line\": expected MoveTo then a two-point LineTo, got {line_geometry:?}"
        );
        // ... and a ring ends with ClosePath, which takes no parameters.
        let ring_geometry = &layer.features[2].geometry;
        assert_eq!(
            ring_geometry.last(),
            Some(&15_u32),
            "case \"ring\": expected a trailing ClosePath, got {ring_geometry:?}"
        );
        assert_eq!(
            layer.features[2].tags,
            Vec::<u32>::new(),
            "case \"ring\": a feature with no attributes carries no tags"
        );
    }

    /// RULE: a `LineTo` of no points is left out of the stream — the
    /// specification forbids a zero-count command — and a feature's geometry
    /// type survives the round trip.
    #[test]
    fn an_empty_line_writes_no_command() {
        let mut writer = GeometryWriter::new();
        writer.move_to(1, 1);
        writer.line_to(&[]);
        let stream = writer.finish();
        assert_eq!(
            stream,
            vec![9_u32, 2, 2],
            "expected only the MoveTo command, got {stream:?}"
        );
    }
}
