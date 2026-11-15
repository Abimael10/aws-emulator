// SPDX-License-Identifier: CC0-1.0

//! The per-message ceiling a WEBSOCKET api serves a client's message under is applied to the
//! payload the message's FRAMES ASSEMBLE TO, driven over a real connection against the address
//! the api announces.
//!
//! The rule this table is a slice of:
//!
//! > A client's message on a deployed WEBSOCKET stage is admitted on the size of the payload
//! > its frames assemble to (RFC 6455 §5.4), not on the size or the number of the frames that
//! > carried it: at or under the api's per-message ceiling it is dispatched to the route its
//! > `RouteSelectionExpression` selects and answered by that route's integration, and past that
//! > ceiling it selects no route and reaches no integration — the gateway answers it with a
//! > CLOSE frame carrying RFC 6455 §7.4.1 status code `1009` and the fixed sentence that class
//! > is worded with, and the connection then ends. The refusal is of that MESSAGE: a later
//! > connection of the same api is served as ever.
//!
//! So the framing is what the rows span: the same ceiling is approached and crossed with
//! fragments of 1 KiB, with fragments of 16 KiB, and with fragments of uneven size — a cycle
//! that includes one frame at exactly the published frame quota and one of a single byte — and
//! each row's assembled total sits under the ceiling, one byte under it, exactly at it, one
//! byte past it, one fragment past it, or a quarter again past it. Every frame of every row is
//! itself inside the published frame quota, so a gateway that only bounded single frames would
//! serve every one of them, and a refusal names the message's class rather than a frame's
//! quota.
//!
//! The neighbouring suites are not restated here: the refusal of one FRAME past the frame quota
//! and of a data frame whose opcode is not text are `websocket_message_refusals.rs` (whose api,
//! whose reading of an answer and whose close texts this suite reuses), the bytes an assembled
//! message is handed to the integration as are `websocket_message_reassembly.rs`, and the
//! handshake's own statuses are `websocket_transport_data_plane.rs`.

use crate::api_lifecycle::emulator;
use crate::websocket_message_refusals::{
    answer, closed, connect, deployed_api, ended, fragments_of, Answer, CLOSE_MESSAGE_TOO_BIG,
    FRAGMENT_BYTES, MESSAGE_CEILING_BYTES, SERVED_REPLY,
};

/// One fragment size the rows cross the ceiling with, in bytes: well under the published frame
/// quota, so a message of a given size arrives as many frames.
const SMALL_FRAGMENT_BYTES: usize = 1024;

/// The other fragment size the rows cross the ceiling with, in bytes: half the published frame
/// quota.
const MID_FRAGMENT_BYTES: usize = 16 * 1024;

/// `count` fragments of `size` bytes each.
fn even(count: usize, size: usize) -> Vec<usize> {
    vec![size; count]
}

/// Fragments of UNEVEN sizes assembling to exactly `total` bytes: the sizes below are cycled
/// through, each capped at what is left, so no fragment passes the published frame quota and
/// the sizes a message arrives in vary from one frame to the next.
fn uneven_to(total: usize) -> Vec<usize> {
    let cycle = [1usize, FRAGMENT_BYTES, MID_FRAGMENT_BYTES, 7];
    let mut sizes = Vec::new();
    let mut left = total;
    let mut at = 0usize;
    while left > 0 {
        let take = cycle[at % cycle.len()].min(left);
        sizes.push(take);
        left -= take;
        at += 1;
    }
    sizes
}

/// A client's message is admitted on its ASSEMBLED size against the per-message ceiling,
/// whatever framing carried it: served at or under it, refused with CLOSE 1009 past it.
#[tokio::test]
async fn the_per_message_ceiling_is_applied_to_the_assembled_payload() {
    /// One row: the byte sizes of the fragments the client writes, and what it must read back.
    struct Row {
        case: &'static str,
        sizes: Vec<usize>,
        expected: Answer,
    }

    let served = Answer::Reply(SERVED_REPLY.to_string());
    let too_big = closed(CLOSE_MESSAGE_TOO_BIG);
    let rows = vec![
        Row {
            case: "seven fragments of 16 KiB, under the ceiling",
            sizes: even(7, MID_FRAGMENT_BYTES),
            expected: Answer::Reply(SERVED_REPLY.to_string()),
        },
        Row {
            case: "fragments of 16 KiB assembling to exactly the ceiling",
            sizes: even(MESSAGE_CEILING_BYTES / MID_FRAGMENT_BYTES, MID_FRAGMENT_BYTES),
            expected: Answer::Reply(SERVED_REPLY.to_string()),
        },
        Row {
            case: "one fragment of 16 KiB more than the ceiling",
            sizes: even(
                MESSAGE_CEILING_BYTES / MID_FRAGMENT_BYTES + 1,
                MID_FRAGMENT_BYTES,
            ),
            expected: closed(CLOSE_MESSAGE_TOO_BIG),
        },
        Row {
            case: "ten fragments of 16 KiB, a quarter again past the ceiling",
            sizes: even(10, MID_FRAGMENT_BYTES),
            expected: closed(CLOSE_MESSAGE_TOO_BIG),
        },
        Row {
            // Follows a refused row on purpose: the refusal above was of that message, so this
            // connection of the same api is served.
            case: "fragments of 1 KiB assembling to exactly the ceiling",
            sizes: even(
                MESSAGE_CEILING_BYTES / SMALL_FRAGMENT_BYTES,
                SMALL_FRAGMENT_BYTES,
            ),
            expected: Answer::Reply(SERVED_REPLY.to_string()),
        },
        Row {
            case: "one fragment of 1 KiB more than the ceiling",
            sizes: even(
                MESSAGE_CEILING_BYTES / SMALL_FRAGMENT_BYTES + 1,
                SMALL_FRAGMENT_BYTES,
            ),
            expected: closed(CLOSE_MESSAGE_TOO_BIG),
        },
        Row {
            case: "uneven fragments assembling to one byte under the ceiling",
            sizes: uneven_to(MESSAGE_CEILING_BYTES - 1),
            expected: Answer::Reply(SERVED_REPLY.to_string()),
        },
        Row {
            case: "uneven fragments assembling to one byte past the ceiling",
            sizes: uneven_to(MESSAGE_CEILING_BYTES + 1),
            expected: closed(CLOSE_MESSAGE_TOO_BIG),
        },
    ];

    let emulator = emulator();
    let api = deployed_api(&emulator, "the assembled per-message ceiling", "framing-api").await;

    for row in rows {
        let case = row.case;
        let total: usize = row.sizes.iter().sum();
        // The row is only about the MESSAGE's size: every frame of it stays inside the frame
        // quota, which has a refusal and a close text of its own.
        let largest = row.sizes.iter().copied().max().unwrap_or_default();
        assert!(
            largest <= FRAGMENT_BYTES,
            "case {case:?}: every frame must stay inside the frame quota of {FRAGMENT_BYTES} \
             bytes, got {largest:?}"
        );
        assert_eq!(
            row.expected == served,
            total <= MESSAGE_CEILING_BYTES,
            "case {case:?}: a row of {total} bytes against the ceiling of \
             {MESSAGE_CEILING_BYTES} expects {:?}",
            row.expected
        );
        // A refusal ends the connection, so every row drives one of its own.
        let mut socket = connect(case, &api).await;
        let answered = answer(case, &mut socket, fragments_of(&row.sizes)).await;
        assert_eq!(
            answered, row.expected,
            "case {case:?}: a message of {total} bytes written as {} fragments expects {:?}, \
             got {answered:?}",
            row.sizes.len(),
            row.expected
        );
        if answered == too_big {
            ended(case, &mut socket).await;
        }
    }
}
