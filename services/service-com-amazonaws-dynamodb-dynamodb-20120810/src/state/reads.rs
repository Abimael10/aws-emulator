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
//! What such a read reports of the item and whether the key held one at all are two
//! different facts: whether there is an item at the key decides whether anything is
//! reported, and the projection decides only the CONTENTS of what is reported — the map
//! with nothing in it, when it names no location of the item it was applied to.
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
/// there will be no Item element in the response" (`GetItem`). What the projection names
/// decides only the CONTENTS of what is reported, so an item that was found but whose
/// projection names no location of it is reported as the map with nothing in it that it
/// projected to.
///
/// The same holds for a read that reports its items as a LIST — `Query`, `Scan` and the
/// per-table list of a batched read: an element that projected nothing is still an element.
/// A read that reports an `ItemResponse` STRUCTURE instead states the two facts in one
/// object and so answers them differently ([`item_response_item`]).
pub fn reported_item(item: Option<Item>, projection: Option<&[DocumentPath]>) -> Option<Item> {
    match (item, projection) {
        (Some(item), Some(paths)) => Some(project_paths(&item, paths)),
        (item, _) => item,
    }
}

/// The `Item` member an `ItemResponse` of a TRANSACTIONAL read carries, given the projected
/// pairs that read came to (`None` where its key held no item).
///
/// An `ItemResponse` is "a Map of the name-value pairs that are the projected attributes of
/// the requested item", and what it states when there are no such pairs is the structure
/// itself with nothing in it: "if a requested item could not be retrieved, the
/// corresponding ItemResponse object is Null, or if the requested item has no projected
/// attributes, the corresponding ItemResponse object is an empty Map"
/// (`TransactGetItems$Responses`). Both facts are therefore answered the same way — the
/// empty `ItemResponse` — so the member is carried only where there is at least one
/// projected pair to report, and a caller testing for it reads "there are attributes here"
/// rather than "the item exists".
///
/// This is the one place that rule lives, for every read that reports its items as
/// `ItemResponse` structures: the transactional get and the PartiQL read transaction. It is
/// NOT how a read with an `Item` MEMBER of its own answers — "if there is no matching item,
/// GetItem does not return any data and there will be no Item element in the response"
/// (`GetItem`) decides that member on whether an item was found, which is
/// [`reported_item`].
pub fn item_response_item(projected: Option<Item>) -> Option<Item> {
    projected.filter(|item| !item.is_empty())
}
