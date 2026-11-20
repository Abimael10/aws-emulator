// SPDX-License-Identifier: CC0-1.0

//! What a SINGLE-ITEM read reports: `GetItem`, and the per-element reads of
//! `BatchGetItem` and `TransactGetItems`.
//!
//! Those three differ from `Query` and `Scan` in having no page, no index and no filter:
//! one key addresses one item, so all that is left of what the read REPORTS is the
//! projection the request carried. The projection itself is read by the one reader every
//! read of this service reads it with ([`super::requested_projection`]) and applied by
//! the one that applies it ([`super::project_paths`]); this file is only where the two
//! meet for a single item, so the three handlers state that once rather than three times.
//!
//! A file of its own beside the item store rather than inside it, for the reason the
//! other read modules are: this is about what one REQUEST reports, not about what the
//! store holds. It is re-exported up to `crate::state`, so every caller names it
//! `crate::state::…`.

use super::{project_paths, DocumentPath, Item};

/// The `Item` a single-item read reports: what `projection` names of the item the key
/// addressed, or the whole item when the request named no projection — "If no attribute
/// names are specified, then all attributes are returned"
/// (`Get$ProjectionExpression`).
///
/// Whether there is an item at the key is the ONLY thing that decides whether there is
/// anything to report: "If there is no matching item, GetItem does not return any data and
/// there will be no Item element in the response" (`GetItem`), and correspondingly "if a
/// requested item could not be retrieved, the corresponding ItemResponse object is Null"
/// (`TransactGetItems$Responses`). What the projection names decides only the CONTENTS of
/// what is reported, so an item that was found but whose projection names no location of it
/// is reported as the map with nothing in it that it projected to — "if the requested item
/// has no projected attributes, the corresponding ItemResponse object is an empty Map", the
/// map of projected name-value pairs that `ItemResponse` carries being the empty one.
///
/// The same holds for a read that reports its items as a LIST — `Query`, `Scan` and the
/// per-table list of a batched read: an element that projected nothing is still an element.
pub fn reported_item(item: Option<Item>, projection: Option<&[DocumentPath]>) -> Option<Item> {
    match (item, projection) {
        (Some(item), Some(paths)) => Some(project_paths(&item, paths)),
        (item, _) => item,
    }
}
