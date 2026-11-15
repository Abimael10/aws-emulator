// SPDX-License-Identifier: CC0-1.0

//! Mapbox Vector Tile encoding, as this crate reaches it.
//!
//! The format lives in the workspace's `vector-tile` crate, shared with the
//! Amazon Location Service maps plane, which serves MVT tiles of its own: one
//! copy of a wire format, so a tile either service answers with decodes the same
//! way. This module is the path this crate has always read it from, so the
//! modules and suites that name `state::vector_tile::…` keep resolving.

pub use ::vector_tile::*;
