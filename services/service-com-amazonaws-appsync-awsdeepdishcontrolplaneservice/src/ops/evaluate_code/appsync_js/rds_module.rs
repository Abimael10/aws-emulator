// SPDX-License-Identifier: CC0-1.0

//! The `@aws-appsync/utils/rds` module an APPSYNC_JS resolver over a relational database
//! imports: the statement builders it writes its request with, and the reading of the result
//! that request produced.
//!
//! The module is DECLARED from the source here, in the same sandbox as the `util` helpers
//! beside it ([`super`]), so an `import` of it opens no path and reaches nothing of the host.
//! Every helper it exports is a thin wrapper over ONE host function
//! ([`RDS_HOST_FUNCTION`]), because the helpers themselves are the service's own reading of
//! them (`crate::service::graphql_api::relational_database::utilities`): the SQL a statement
//! carries and the rows an answer holds are decided in one place for this runtime and for a
//! Velocity template, so the two cannot disagree.
//!
//! What the module exports, by the names AppSync's own resolvers import:
//!
//! * `select`, `insert`, `update` and `remove` — one statement each, from an object stating
//!   the table it is about, the columns it reports or assigns and the condition it holds
//!   over;
//! * `sql` — a tagged template, for a statement written as its own text, whose interpolated
//!   values are BOUND rather than written into it;
//! * `createMySQLStatement` and `createPgStatement` — the request object the data source is
//!   called with, rendering the statements in the dialect of the engine the addressed cluster
//!   runs;
//! * `toJsonObject` — the rows of the result the request produced, each as the object its
//!   columns name;
//! * `typeHint` — a value wrapped in the type it is to be sent to the database as.

/// The specifier an `import` of this module names.
pub(super) const RDS_UTILS_MODULE: &str = "@aws-appsync/utils/rds";

/// The name the host function every helper of this module calls is installed under.
pub(super) const RDS_HOST_FUNCTION: &str = "__appsyncRds";

/// This module, as its own source.
///
/// A helper RAISES what the host refused the call with, exactly as a `util` helper does, so a
/// statement that cannot be built is an error of the resolver's own code and the field reports
/// it.
pub(super) const RDS_UTILS_MODULE_SOURCE: &str = r#"
const hostRds = __appsyncRds;
const called = function (helper, args) {
    const answered = JSON.parse(hostRds(helper, JSON.stringify(args)));
    if (answered.error !== undefined && answered.error !== null) {
        throw new Error(answered.error);
    }
    return answered.value;
};
const builder = function (helper) {
    return function (input) {
        return called(helper, [input === undefined ? null : input]);
    };
};
export const select = builder("select");
export const insert = builder("insert");
export const update = builder("update");
export const remove = builder("remove");
// The tagged template: the engine hands the literal parts as the first argument and one
// further argument per interpolated value, in the order the template writes them.
export const sql = function (parts) {
    return called("sql", [
        Array.prototype.slice.call(parts),
        Array.prototype.slice.call(arguments, 1),
    ]);
};
const renderer = function (helper) {
    return function () {
        return called(helper, Array.prototype.slice.call(arguments));
    };
};
export const createMySQLStatement = renderer("createMySQLStatement");
export const createPgStatement = renderer("createPgStatement");
export const toJsonObject = function (result) {
    return called("toJsonObject", [result === undefined ? null : result]);
};
export const toJsonString = function (result) {
    return called("toJsonString", [result === undefined ? null : result]);
};
// A hinted value states the type it is sent to the database as, which the request carries in
// its own type hint map.
const hint = function (name) {
    return function (value) {
        return {__appsyncRdsTypeHint: name, value: value === undefined ? null : value};
    };
};
export const typeHint = {
    JSON: hint("JSON"),
    UUID: hint("UUID"),
    TIMESTAMP: hint("TIMESTAMP"),
    DATE: hint("DATE"),
    TIME: hint("TIME"),
    DECIMAL: hint("DECIMAL"),
};
export default {
    select,
    insert,
    update,
    remove,
    sql,
    createMySQLStatement,
    createPgStatement,
    toJsonObject,
    toJsonString,
    typeHint,
};
"#;
