// SPDX-License-Identifier: CC0-1.0

//! How the slot a turn elicits is ELICITED: the two members of a dialog action a
//! client configures the elicitation with, over every encoding of a turn and
//! every door that writes a session.
//!
//! `DialogAction$slotElicitationStyle` "configures the slot to use
//! spell-by-letter or spell-by-word style" and `DialogAction$subSlotToElicit` is
//! "the name of the constituent sub slot of the composite slot specified in
//! slotToElicit that should be elicited from the user": neither says which step
//! the bot takes next — they say how the slot named by `slotToElicit` is asked
//! for. So while the bot's own decision replaces the STEP a request asked for,
//! it carries the configuration the dialog state holds for the slot it goes on
//! eliciting.
//!
//! The rules under test, each over a TABLE of inputs:
//!
//! 1. A turn whose decision is to elicit the slot the dialog state names reports
//!    that state's `slotElicitationStyle` and `subSlotToElicit`, and a later
//!    `GetSession` reports them too — for every value of the modelled
//!    `StyleType` enum and for a sub slot of any depth.
//! 2. Nothing is invented for an elicitation nothing configured: a turn that
//!    elicits another slot, or that elicits nothing at all, reports neither
//!    member.
//! 3. The configuration lives on the SESSION: it stands over the later turns of
//!    the conversation that go on eliciting the same slot, carrying no state of
//!    their own, and is gone once that elicitation ends.
//! 4. The two encodings of a turn agree — `RecognizeUtterance` carries the state
//!    on its gzipped header rather than in a body — and so does the sibling
//!    `PutSession` door.

use bytes::Bytes;
use serde_json::{json, Value};

use crate::harness::{gzip_base64, Emulator};
use crate::recognize_text_nlu::{pizza_bot, say_with_state, Bot, ORDER_PIZZA, PIZZA_SIZE};

/// `TopResolution`, one member of the modelled `SlotValueResolutionStrategy`
/// enum (`smithy_query(query="shape",shape="SlotValueResolutionStrategy")` on
/// the Lex Models V2 model: OriginalValue | TopResolution | Concatenation): the
/// strategy that resolves a word the slot type does not declare to nothing, so
/// an answer the bot cannot resolve leaves the slot open and the elicitation
/// running.
const TOP_RESOLUTION: &str = "TopResolution";

/// Every value the modelled `StyleType` enum admits
/// (`smithy_query(query="shape",shape="StyleType")`: Default, SpellByLetter,
/// SpellByWord) — the whole catalog, so none of them is the only one that works.
const STYLES: [&str; 3] = ["Default", "SpellByLetter", "SpellByWord"];

/// A word the pizza bot's slot type does not declare, so an answer carrying it
/// resolves to nothing and the slot stays open.
const UNRESOLVABLE_ANSWER: &str = "medium";

/// An utterance of the bot's that names no slot value, so the turn it opens
/// elicits the required slot.
const OPENING_UTTERANCE: &str = "I want to order a pizza";

/// An utterance that fills the required slot outright, so the turn it takes
/// closes the dialog.
const FILLING_UTTERANCE: &str = "I want a large pizza";

/// The `type` of a dialog action that elicits a slot, and of one that ends the
/// dialog: two members of the modelled `DialogActionType` enum
/// (`smithy_query(query="shape",shape="DialogActionType")`: Close,
/// ConfirmIntent, Delegate, ElicitIntent, ElicitSlot, None).
const ELICIT_SLOT: &str = "ElicitSlot";
/// See [`ELICIT_SLOT`].
const CLOSE: &str = "Close";
/// See [`ELICIT_SLOT`]: the step that asks the bot to decide the next one.
const DELEGATE: &str = "Delegate";

/// A `sessionState` seeding the dialog on the pizza intent, with the dialog
/// action `action` describes.
fn seed(action: Value) -> Value {
    json!({"dialogAction": action, "intent": {"name": ORDER_PIZZA}})
}

/// The dialog action a turn or a session read reports.
fn dialog_action(body: &Value, pointer: &str) -> Value {
    body.pointer(pointer)
        .cloned()
        .unwrap_or_else(|| panic!("the state must report a dialogAction, got {body:?}"))
}

/// The dialog action `GetSession` reports for that conversation.
async fn read_dialog_action(emulator: &Emulator, bot: &Bot, session_id: &str) -> Value {
    let reply = emulator.send("GET", &bot.session(session_id), None).await;
    assert_eq!(
        reply.status,
        200,
        "GetSession {session_id:?}: expected HTTP 200, got {} {:?}",
        reply.status,
        String::from_utf8_lossy(&reply.body)
    );
    dialog_action(&reply.json(), "/sessionState/dialogAction")
}

/// Rule 1: the elicitation of the slot the dialog state names keeps that state's
/// own `slotElicitationStyle` and `subSlotToElicit`, on the turn and on the
/// session it leaves behind.
#[tokio::test]
async fn an_elicitation_keeps_the_settings_the_caller_configured_for_that_slot() {
    let emulator = Emulator::new();
    let bot = pizza_bot(
        &emulator,
        "StyledPizzaBot",
        Some(TOP_RESOLUTION),
        true,
        None,
    )
    .await;

    // (case, the elicitation settings the request carries beside the slot)
    let mut rows: Vec<(String, Value)> = vec![
        ("no elicitation settings at all".to_string(), json!({})),
        (
            "a sub slot of the slot being elicited, and no style".to_string(),
            json!({"subSlotToElicit": {"name": "Crust"}}),
        ),
        (
            "a sub slot of a sub slot, nested as the model allows".to_string(),
            json!({
                "subSlotToElicit": {
                    "name": "Crust",
                    "subSlotToElicit": {"name": "Thickness"},
                },
            }),
        ),
    ];
    // Every value of the modelled enum, alone and beside a sub slot.
    for style in STYLES {
        rows.push((
            format!("the {style:?} style"),
            json!({"slotElicitationStyle": style}),
        ));
        rows.push((
            format!("the {style:?} style and a sub slot"),
            json!({
                "slotElicitationStyle": style,
                "subSlotToElicit": {"name": "Crust"},
            }),
        ));
    }

    for (index, (case, settings)) in rows.iter().enumerate() {
        let session = format!("styled-{index}");
        let mut action = json!({"type": ELICIT_SLOT, "slotToElicit": PIZZA_SIZE});
        for (member, value) in settings.as_object().expect("the settings are an object") {
            action[member.as_str()] = value.clone();
        }
        // What the turn must report: this utterance leaves the required slot
        // open, so the bot's own decision is to elicit the very slot the request
        // configured — the step it asked for and the settings it carried.
        let expected = action.clone();

        let turn = say_with_state(
            &emulator,
            &bot,
            &session,
            OPENING_UTTERANCE,
            Some(seed(action)),
        )
        .await;
        let actual = dialog_action(&turn, "/sessionState/dialogAction");
        assert_eq!(
            actual, expected,
            "case {case:?}: the turn must report the elicitation it was configured for, \
             got {actual:?}"
        );
        let stored = read_dialog_action(&emulator, &bot, &session).await;
        assert_eq!(
            stored, expected,
            "case {case:?}: GetSession must report the same elicitation, got {stored:?}"
        );
    }
}

/// Rule 1, continued: the settings travel with the SLOT the dialog state names,
/// so a request that asks the bot to decide the step (`Delegate`) still has the
/// elicitation it configured for that slot honoured when the bot elicits it.
#[tokio::test]
async fn a_delegated_step_keeps_the_settings_configured_for_the_slot() {
    let emulator = Emulator::new();
    let bot = pizza_bot(
        &emulator,
        "DelegatedPizzaBot",
        Some(TOP_RESOLUTION),
        true,
        None,
    )
    .await;

    for (index, style) in STYLES.iter().enumerate() {
        let session = format!("delegated-{index}");
        let turn = say_with_state(
            &emulator,
            &bot,
            &session,
            OPENING_UTTERANCE,
            Some(seed(json!({
                "type": DELEGATE,
                "slotToElicit": PIZZA_SIZE,
                "slotElicitationStyle": style,
            }))),
        )
        .await;
        let expected = json!({
            "type": ELICIT_SLOT,
            "slotToElicit": PIZZA_SIZE,
            "slotElicitationStyle": style,
        });
        let actual = dialog_action(&turn, "/sessionState/dialogAction");
        assert_eq!(
            actual, expected,
            "case {style:?} delegated: the step is the bot's and the style the caller's, \
             got {actual:?}"
        );
    }
}

/// Rule 2: nothing is invented for an elicitation nothing configured — another
/// slot's elicitation, and a dialog that elicits nothing at all, report neither
/// member.
#[tokio::test]
async fn an_unconfigured_step_reports_no_elicitation_settings() {
    let emulator = Emulator::new();
    let bot = pizza_bot(
        &emulator,
        "PlainPizzaBot",
        Some(TOP_RESOLUTION),
        true,
        None,
    )
    .await;

    // (case, the dialog action the request carries, the utterance, the dialog
    // action the turn must report)
    let rows: Vec<(&str, Value, &str, Value)> = vec![
        (
            "settings configured for a slot the turn does not elicit",
            json!({
                "type": ELICIT_SLOT,
                "slotToElicit": "Crust",
                "slotElicitationStyle": "SpellByWord",
                "subSlotToElicit": {"name": "Thickness"},
            }),
            OPENING_UTTERANCE,
            json!({"type": ELICIT_SLOT, "slotToElicit": PIZZA_SIZE}),
        ),
        (
            "settings on a turn that elicits nothing, because it filled the slot",
            json!({
                "type": ELICIT_SLOT,
                "slotToElicit": PIZZA_SIZE,
                "slotElicitationStyle": "SpellByLetter",
                "subSlotToElicit": {"name": "Crust"},
            }),
            FILLING_UTTERANCE,
            json!({"type": CLOSE}),
        ),
        (
            "settings naming no slot at all",
            json!({"type": DELEGATE, "slotElicitationStyle": "SpellByLetter"}),
            OPENING_UTTERANCE,
            json!({"type": ELICIT_SLOT, "slotToElicit": PIZZA_SIZE}),
        ),
    ];

    for (index, (case, action, text, expected)) in rows.iter().enumerate() {
        let session = format!("plain-{index}");
        let turn = say_with_state(
            &emulator,
            &bot,
            &session,
            text,
            Some(seed(action.clone())),
        )
        .await;
        let actual = dialog_action(&turn, "/sessionState/dialogAction");
        assert_eq!(
            &actual, expected,
            "case {case:?}: the turn must report only the step it decided on, got {actual:?}"
        );
        let stored = read_dialog_action(&emulator, &bot, &session).await;
        assert_eq!(
            &stored, expected,
            "case {case:?}: GetSession must report the same dialog action, got {stored:?}"
        );
    }
}

/// Rule 3: the configuration is the session's, so it stands over the later turns
/// that go on eliciting the same slot — each carrying no dialog state of its own
/// — and is gone once the slot is filled and the elicitation ends.
#[tokio::test]
async fn the_configuration_stands_while_the_same_slot_is_elicited() {
    let emulator = Emulator::new();
    let bot = pizza_bot(
        &emulator,
        "SpellingPizzaBot",
        Some(TOP_RESOLUTION),
        true,
        None,
    )
    .await;

    for (index, style) in STYLES.iter().enumerate() {
        let session = format!("spelling-{index}");
        let expected = json!({
            "type": ELICIT_SLOT,
            "slotToElicit": PIZZA_SIZE,
            "slotElicitationStyle": style,
        });

        let opened = say_with_state(
            &emulator,
            &bot,
            &session,
            OPENING_UTTERANCE,
            Some(seed(json!({
                "type": ELICIT_SLOT,
                "slotToElicit": PIZZA_SIZE,
                "slotElicitationStyle": style,
            }))),
        )
        .await;
        assert_eq!(
            dialog_action(&opened, "/sessionState/dialogAction"),
            expected,
            "case {style:?}: the turn that was configured, got {opened:?}"
        );

        // Two more answers the slot type resolves to nothing: the slot stays
        // open, so the same elicitation runs on, and neither request carries a
        // dialog state of its own.
        for attempt in 1..=2 {
            let again =
                say_with_state(&emulator, &bot, &session, UNRESOLVABLE_ANSWER, None).await;
            let actual = dialog_action(&again, "/sessionState/dialogAction");
            assert_eq!(
                actual, expected,
                "case {style:?} attempt {attempt}: the elicitation runs on with the style the \
                 session holds, got {actual:?}"
            );
            let stored = read_dialog_action(&emulator, &bot, &session).await;
            assert_eq!(
                stored, expected,
                "case {style:?} attempt {attempt}: GetSession must report the same, got \
                 {stored:?}"
            );
        }

        // The answer that fills the slot ends the elicitation, and with it the
        // configuration of the slot that was being elicited.
        let closed = say_with_state(&emulator, &bot, &session, "large", None).await;
        let actual = dialog_action(&closed, "/sessionState/dialogAction");
        assert_eq!(
            actual,
            json!({"type": CLOSE}),
            "case {style:?}: the filled slot closes the dialog, got {actual:?}"
        );
        let stored = read_dialog_action(&emulator, &bot, &session).await;
        assert_eq!(
            stored,
            json!({"type": CLOSE}),
            "case {style:?}: GetSession must report the closed dialog, got {stored:?}"
        );
    }
}

/// Rule 4: the other encoding of the same turn, and the sibling write door,
/// answer the same — `RecognizeUtterance` carries the dialog state on its
/// gzipped header, and `PutSession` seeds it in a body.
#[tokio::test]
async fn every_door_reports_the_same_elicitation_settings() {
    /// The `requestContentType` a text input stream carries ("Text format —
    /// `text/plain; charset=utf-8`").
    const TEXT_REQUEST_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

    let emulator = Emulator::new();
    let bot = pizza_bot(
        &emulator,
        "EveryDoorPizzaBot",
        Some(TOP_RESOLUTION),
        true,
        None,
    )
    .await;

    for (index, style) in STYLES.iter().enumerate() {
        let configured = json!({
            "type": ELICIT_SLOT,
            "slotToElicit": PIZZA_SIZE,
            "slotElicitationStyle": style,
            "subSlotToElicit": {"name": "Crust"},
        });
        let expected = configured.clone();

        // --- RecognizeUtterance: the state travels on the gzipped header ------
        let spoken = format!("spoken-{index}");
        let state = seed(configured.clone());
        let reply = emulator
            .send_raw(
                "POST",
                &format!("{}/utterance", bot.session(&spoken)),
                Bytes::from(OPENING_UTTERANCE),
                TEXT_REQUEST_CONTENT_TYPE,
                &[(
                    "x-amz-lex-session-state",
                    gzip_base64(&state.to_string()).as_str(),
                )],
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {style:?}: RecognizeUtterance must succeed, got {} {:?}",
            reply.status,
            String::from_utf8_lossy(&reply.body)
        );
        let reported = reply
            .lex_header("x-amz-lex-session-state")
            .unwrap_or_else(|| panic!("case {style:?}: the turn must report its session state"));
        let actual = dialog_action(&reported, "/dialogAction");
        assert_eq!(
            actual, expected,
            "case {style:?}: RecognizeUtterance must keep the configured elicitation, got \
             {actual:?}"
        );
        let stored = read_dialog_action(&emulator, &bot, &spoken).await;
        assert_eq!(
            stored, expected,
            "case {style:?}: GetSession must report what the utterance left, got {stored:?}"
        );

        // --- PutSession: the sibling door that seeds a dialog outright --------
        let seeded = format!("seeded-{index}");
        let put = emulator
            .send(
                "POST",
                &bot.session(&seeded),
                Some(json!({"sessionState": seed(configured.clone())})),
            )
            .await;
        assert_eq!(
            put.status,
            200,
            "case {style:?}: PutSession must succeed, got {} {:?}",
            put.status,
            String::from_utf8_lossy(&put.body)
        );
        let stored = read_dialog_action(&emulator, &bot, &seeded).await;
        assert_eq!(
            stored, expected,
            "case {style:?}: the sibling door must report the same elicitation, got {stored:?}"
        );
    }
}
