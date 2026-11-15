// SPDX-License-Identifier: CC0-1.0

//! How this service's operations are served so that the error bodies they answer with are
//! the ones THIS service answers with.
//!
//! The one fact stated here is WHICH operations are served through that layer: every
//! operation the model declares, because the member an error body reports its text in
//! ([`super::error_message_member`]) is a property of this service's error bodies rather
//! than of any one operation. The member itself, the renaming and the whole
//! deserialize -> validate -> handle -> serialize walk stay where they already live.

use std::sync::Arc;

use service_common::submitted_collections::AnsweringAs;
use smithy_server_runtime::OperationRoutingTable;

/// Serve `$operation` with its error bodies reporting their text in the member this model
/// declares: one `crate::ops::<module>::<Operation>` per line, filed under its own modelled
/// name.
macro_rules! answering {
    ($table:expr, $engine:expr, $($module:ident :: $operation:ident),+ $(,)?) => {
        $(
            $table.add_operation(
                <crate::ops::$module::$operation<Eng> as ::smithy_server_runtime::Operation>::NAME,
                Box::new(AnsweringAs::new(
                    crate::ops::$module::$operation::new(::std::sync::Arc::clone($engine)),
                    super::error_message_member::with_modelled_message_member,
                )),
            );
        )+
    };
}

/// The same modelled operations the generated table holds, each wrapped so that the error
/// bodies it answers with are the ones this service answers with, and built from the SAME
/// shared engine, so a wrapped operation reads and writes the one store.
pub(super) fn stated_error_bodies<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
) -> OperationRoutingTable {
    let mut table = OperationRoutingTable::new();
    answering!(
        table,
        engine,
        describe_services::DescribeServices,
        get_attribute_values::GetAttributeValues,
        get_price_list_file_url::GetPriceListFileUrl,
        get_products::GetProducts,
        list_price_lists::ListPriceLists,
    );
    table
}
