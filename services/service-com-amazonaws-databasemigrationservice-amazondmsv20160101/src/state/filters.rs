// SPDX-License-Identifier: CC0-1.0

//! The `Filters` guard every DMS `Describe*` that takes a `FilterList` shares.
//!
//! `Filter$Name` is "the name of the filter as specified for a `Describe*` or similar
//! operation", and each report's own `Filters` member doc publishes the names it accepts
//! ("Valid filter names: ..."). A name outside that set selects nothing the operation
//! knows how to narrow by, so DMS REFUSES the request rather than ignoring the entry and
//! answering with resources the caller asked to exclude.
//!
//! One guard and one wording for every report that checks its filter names, so none of
//! them can drift from the others: a report differs only in the catalog of names it
//! passes in.

use crate::ops::types::Filter;

/// What DMS refuses a `Filters` entry whose `Name` is not one the report publishes with.
///
/// DMS answers an unrecognized filter name with ONE sentence: fixed text, then the name
/// the request carried, and nothing after it — no enumeration of the names the operation
/// does publish, and no mention of the operation. The only variable part is `name`, taken
/// verbatim from the request's own `Filter$Name`, so a caller that parses the refusal
/// reads back the value it sent.
pub fn unrecognized_filter_name_message(name: &str) -> String {
    format!("Unrecognized filter name: {name}")
}

/// Refuse the first `Filters` entry whose `Name` is not one of `documented`, in request
/// order, so the refusal names the first unrecognized name the caller sent.
///
/// The refusal is a parameter-value fault — HTTP 400 with the error code
/// `InvalidParameterValueException` — which this service's model declares for no
/// operation, so it travels the handler's non-modelled refusal path
/// ([`crate::scaffolded::Scaffolded::invalid_parameter_value`]) rather than being squeezed
/// into the generic validation channel: a client branching on the error code has to see
/// the code DMS answers for it.
///
/// Generic over the operation's modelled error enum, so each report calls it with its own
/// error channel and its own catalog of names.
pub fn check_filter_names<E>(
    filters: &[Filter],
    documented: &[&str],
) -> Result<(), crate::scaffolded::Scaffolded<E>> {
    match filters
        .iter()
        .find(|filter| !documented.contains(&filter.name.as_str()))
    {
        Some(filter) => Err(crate::scaffolded::Scaffolded::invalid_parameter_value(
            unrecognized_filter_name_message(&filter.name),
        )),
        None => Ok(()),
    }
}
