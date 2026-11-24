// SPDX-License-Identifier: CC0-1.0

//! The reading behind a SQL client's own `set` command, reached through the crate's PUBLIC
//! surface.
//!
//! THE RULE this suite covers: `set` is a command of the CLIENT rather than a statement for the
//! metastore, so the text of one is read as a session-configuration command — an assignment, a
//! report of one property, or a report of every property the session holds — and a `SET` form
//! that names something other than a configuration property is left to the SQL driver. The
//! end-to-end behaviour that rule produces is `crate::step_sql_session_configuration`'s, and the
//! forms a Hive CLI writes are `state::step_session_config`'s own unit test; what this adds is
//! that the reading is reachable from OUTSIDE the crate at all, over the rows
//! that unit test does not carry: a value quoted the way the client takes one, a name carrying
//! digits, whitespace around every part, and the `SET` forms of the SQL grammar that name no
//! property.

use service_com_amazonaws_emr_elasticmapreduce::state::{session_setting, SessionSetting};

/// Rows the sibling unit test does not carry: how a command line can SPELL the same command,
/// and the `SET` forms that are no `set` command of the client.
#[test]
fn a_set_commands_form_is_read_off_its_text() {
    let rows: [(&str, &str, Option<SessionSetting>); 8] = [
        (
            "a quoted value, as a command line writes one",
            "SET hive.exec.scratchdir='/mnt/tmp';",
            Some(SessionSetting::Assign {
                key: "hive.exec.scratchdir".to_string(),
                value: "'/mnt/tmp'".to_string(),
            }),
        ),
        (
            "a value carrying a separator of its own",
            "set hive.aux.jars.path=/a.jar,/b.jar",
            Some(SessionSetting::Assign {
                key: "hive.aux.jars.path".to_string(),
                value: "/a.jar,/b.jar".to_string(),
            }),
        ),
        (
            "whitespace around every part",
            "   SET   hive.exec.parallel   ;   ",
            Some(SessionSetting::Print("hive.exec.parallel".to_string())),
        ),
        (
            "a property name carrying digits",
            "SET mapreduce.map.memory.mb;",
            Some(SessionSetting::Print("mapreduce.map.memory.mb".to_string())),
        ),
        (
            "the listing, with the separator and nothing else",
            "SET ;",
            Some(SessionSetting::PrintAll),
        ),
        (
            "the listing the client's verbose option asks for",
            "set -v",
            Some(SessionSetting::PrintAll),
        ),
        // The SQL grammar's own `SET` forms name no configuration property, so the driver
        // answers for them rather than the client.
        ("a transaction characteristic", "SET TIME ZONE 'UTC';", None),
        ("a role", "SET ROLE admin", None),
    ];
    for (case, text, expected) in rows {
        let actual = session_setting(text);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}
