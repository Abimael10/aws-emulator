// SPDX-License-Identifier: CC0-1.0

//! Mapbox Vector Tile encoding, as this crate reaches it.
//!
//! The format lives in the workspace's `vector-tile` crate, shared with the Maps
//! V2 plane beside this one, which serves MVT tiles of its own: one copy of a
//! wire format, so a tile either service answers with decodes the same way.
//! [`super::map_tiles`] builds a tile through it, and a suite reads one back
//! through [`decode_tile`].

pub use ::vector_tile::*;
