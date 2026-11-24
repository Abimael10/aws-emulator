// SPDX-License-Identifier: CC0-1.0

//! The `@aws-appsync/utils/dynamodb` module an APPSYNC_JS resolver over an `AMAZON_DYNAMODB`
//! data source imports: the helpers it BUILDS its request object with, and the `operations` an
//! update states its changes with.
//!
//! The module is DECLARED from the source here, in the same sandbox as the `util` helpers
//! beside it ([`super`]), so an `import` of it opens no path and reaches nothing of the host.
//! Every helper it exports is a thin wrapper over ONE host function
//! ([`DDB_HOST_FUNCTION`]), because the helpers themselves are the service's own reading of
//! them (`crate::service::graphql_api::dynamodb::helpers`): the request object a resolver's
//! code returns is built in the same place for this runtime as the one a Velocity template
//! writes out is read, so the two cannot disagree about what a call carries.
//!
//! What the module exports, by the names AppSync's own resolvers import:
//!
//! * `get`, `put`, `remove`, `update`, `query`, `scan` and `sync` — one request object each,
//!   from a payload stating the key it addresses, the item it writes, the changes it makes and
//!   the condition, filter or projection it holds over, with every plain value MARSHALLED as
//!   the attribute value DynamoDB holds it as;
//! * `operations` — what an update does to ONE attribute (add to it, append to or prepend to
//!   the list it holds, count it up or down, replace it, write one element of it, drop it),
//!   each a value the `update` helper reads back out of the update it is handed.

/// The specifier an `import` of this module names.
pub(super) const DYNAMODB_UTILS_MODULE: &str = "@aws-appsync/utils/dynamodb";

/// The name the host function every helper of this module calls is installed under.
pub(super) const DDB_HOST_FUNCTION: &str = "__appsyncDdb";

/// This module, as its own source.
///
/// A helper RAISES what the host refused the payload with, exactly as a `util` helper does, so
/// a request object that cannot be built is an error of the resolver's own code and the field
/// reports it.
pub(super) const DYNAMODB_UTILS_MODULE_SOURCE: &str = r#"
const hostDdb = __appsyncDdb;
const called = function (helper, args) {
    const answered = JSON.parse(hostDdb(helper, JSON.stringify(args)));
    if (answered.error !== undefined && answered.error !== null) {
        throw new Error(answered.error);
    }
    return answered.value;
};
const builder = function (helper) {
    return function (payload) {
        return called(helper, [payload === undefined ? null : payload]);
    };
};
export const get = builder("get");
export const put = builder("put");
export const remove = builder("remove");
export const update = builder("update");
export const query = builder("query");
export const scan = builder("scan");
export const sync = builder("sync");
// Each `operations.*` value STATES what an update does to one attribute, so it is a value the
// update it sits in carries rather than a call of its own: the host reads it back under the
// member it is marked with (`crate::service::graphql_api::dynamodb::helpers`) and writes the
// clause that performs it.
const operation = function (name) {
    return function () {
        return {
            __appsyncDdbOperation: name,
            arguments: Array.prototype.slice.call(arguments),
        };
    };
};
export const operations = {
    add: operation("add"),
    append: operation("append"),
    prepend: operation("prepend"),
    decrement: operation("decrement"),
    increment: operation("increment"),
    replace: operation("replace"),
    updateListItem: operation("updateListItem"),
    remove: operation("remove"),
};
export default { get, put, remove, update, query, scan, sync, operations };
"#;

#[cfg(test)]
mod tests {
    use crate::service::graphql_api::dynamodb::helpers;

    use super::*;

    /// RULE: every helper the service builds a request object for is EXPORTED by this module,
    /// and every operation an update is read for is one this module publishes — the two halves
    /// are written in different languages, so this is what holds them to one set of names.
    #[test]
    fn the_module_exports_every_helper_the_host_builds() {
        for helper in helpers::HELPERS {
            let exported = format!("export const {helper} = builder(\"{helper}\");");
            assert!(
                DYNAMODB_UTILS_MODULE_SOURCE.contains(&exported),
                "case the {helper:?} helper: expected the module to export it as {exported:?}"
            );
        }
        for member in [helpers::OPERATION_MARKER, helpers::OPERATION_ARGUMENTS] {
            assert!(
                DYNAMODB_UTILS_MODULE_SOURCE.contains(member),
                "case the {member:?} member of an update operation: expected the module to \
                 state it"
            );
        }
        // And the host function every helper calls is the one the sandbox installs under that
        // name, so renaming the constant cannot leave the module calling a name nothing binds.
        assert!(
            DYNAMODB_UTILS_MODULE_SOURCE.contains(DDB_HOST_FUNCTION),
            "case the host function of this module: expected the source to call \
             {DDB_HOST_FUNCTION:?}"
        );
    }
}
