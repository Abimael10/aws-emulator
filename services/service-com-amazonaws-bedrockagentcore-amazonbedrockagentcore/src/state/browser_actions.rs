// SPDX-License-Identifier: CC0-1.0

//! The SURFACE a browser session's operating-system-level actions are performed against,
//! and the input state those actions leave on it.
//!
//! `InvokeBrowser` performs one OS-level action on a live browser session: a pointer
//! action, a keyboard action, or a screenshot. Every one of them is performed against the
//! session's own viewport ([`BrowserSurface`]) — the visible area the start asked for,
//! which is "the visible area of the web content" the session renders. So a screenshot is
//! encoded at exactly those dimensions, and a point outside them is not on this session's
//! screen at all: nothing can be delivered there, which the action reports as a FAILED
//! result rather than as a refusal of the request.
//!
//! What a pointer or keyboard action leaves behind is the session's
//! [`BrowserInputState`]: where its pointer stands, how far the wheel has scrolled it and
//! how much has been sent through its keyboard. That pointer is what a screenshot of the
//! surface shows, because these are OS-level screenshots and the screen's pointer is part
//! of the screen.
//!
//! Nothing in this emulator navigates a page — a page is driven over the session's
//! automation stream, the WebSocket channel the start materialises — so the surface
//! carries NO page content: it renders as the blank viewport a browser shows before
//! anything is loaded, and no content is synthesised onto it.

use std::time::SystemTime;

use crate::ops::types::ViewPort;

use super::BrowserSessionRecord;

/// `BrowserActionStatus` for an action the session performed (source: smithy_query
/// query="shape" shape="BrowserActionStatus", values SUCCESS | FAILED).
pub const BROWSER_ACTION_SUCCESS: &str = "SUCCESS";

/// `BrowserActionStatus` for an action the session could not perform, from the same enum.
/// It travels on a 200 beside the `error` sentence that says why: the action failed, the
/// request that asked for it did not.
pub const BROWSER_ACTION_FAILED: &str = "FAILED";

/// The width of the surface a session renders and dispatches on when its start asked for
/// no viewport.
///
/// AWS documents that "if not specified, Amazon Bedrock AgentCore uses a default viewport
/// size" without publishing that size, so this is an EMULATOR surface — this process's own
/// choice, in the same spirit as the ceilings this plane declares — and not a value a read
/// of such a session reports as its viewport. It lies inside `ViewPort`'s modelled ranges
/// (width 320..3840, height 240..2160).
pub const DEFAULT_SURFACE_WIDTH: u32 = 1280;

/// The height of that same surface, chosen for the same reason.
pub const DEFAULT_SURFACE_HEIGHT: u32 = 720;

/// The widest surface this emulator renders, and the tallest: `ViewPort`'s own published
/// maxima (source: smithy_query query="shape" shape="ViewPort", width 320..3840, height
/// 240..2160). A stored viewport is already inside them, since the request boundary refuses
/// one that is not; holding them here as well is what bounds the image ONE screenshot
/// encodes whatever a session carries.
pub const MAX_SURFACE_WIDTH: u32 = 3_840;

/// The tallest, from the same member's range.
pub const MAX_SURFACE_HEIGHT: u32 = 2_160;

/// How many samples one rendered pixel carries: the surface is encoded as 8-bit RGB, so
/// three.
const SAMPLES_PER_PIXEL: usize = 3;

/// The sample value every channel of an unpainted pixel carries: the blank white viewport
/// a browser shows before a page is loaded.
const SURFACE_SAMPLE: u8 = 0xff;

/// The sample value every channel of a pointer pixel carries.
const POINTER_SAMPLE: u8 = 0x00;

/// How many pixels on a side the pointer drawn into a screenshot spans, its tip at the
/// point the session's last pointer action left it.
const POINTER_MARK_PIXELS: usize = 12;

/// How many clicks a click action that names no `clickCount` delivers: one, which is what
/// a pointer click is. The member is optional and modelled 1..10, so a request that omits
/// it asks for a single click.
const SINGLE_CLICK: i32 = 1;

/// How many presses a key action that names no `presses` delivers, for the same reason:
/// the member is optional and modelled 1..100.
const SINGLE_PRESS: i32 = 1;

/// The screen one browser session's OS-level actions happen on: its viewport, or the
/// emulator's own surface when the session was started without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserSurface {
    /// The horizontal dimension of the visible area, in pixels.
    pub width: u32,
    /// The vertical dimension of the visible area, in pixels.
    pub height: u32,
}

impl BrowserSurface {
    /// The surface a session with `view_port` renders and dispatches on.
    #[must_use]
    pub fn of(view_port: Option<&ViewPort>) -> Self {
        match view_port {
            // `ViewPort`'s members are `@required` and modelled positive, so a stored
            // viewport is the surface; a value outside that range is no dimension at all
            // and leaves the emulator's own surface standing, and neither dimension is
            // rendered past the maximum the member publishes.
            Some(view_port) => Self {
                width: u32::try_from(view_port.width)
                    .unwrap_or(DEFAULT_SURFACE_WIDTH)
                    .min(MAX_SURFACE_WIDTH),
                height: u32::try_from(view_port.height)
                    .unwrap_or(DEFAULT_SURFACE_HEIGHT)
                    .min(MAX_SURFACE_HEIGHT),
            },
            None => Self {
                width: DEFAULT_SURFACE_WIDTH,
                height: DEFAULT_SURFACE_HEIGHT,
            },
        }
    }

    /// Whether the point `(x, y)` lies on this surface. The origin is its top-left corner,
    /// so a negative coordinate, or one at or past a dimension, is off the screen.
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        u32::try_from(x).is_ok_and(|x| x < self.width)
            && u32::try_from(y).is_ok_and(|y| y < self.height)
    }

    /// This surface as a PNG image, exactly [`Self::width`] by [`Self::height`] pixels,
    /// with the session's `pointer` drawn where its last pointer action left it — or
    /// `None` when the image could not be encoded, which the action reports as a failed
    /// screenshot.
    #[must_use]
    pub fn render_png(&self, pointer: Option<(i32, i32)>) -> Option<Vec<u8>> {
        let width = usize::try_from(self.width).ok()?;
        let height = usize::try_from(self.height).ok()?;
        let samples_len = width.checked_mul(height)?.checked_mul(SAMPLES_PER_PIXEL)?;
        let mut samples = vec![SURFACE_SAMPLE; samples_len];
        if let Some((x, y)) = pointer {
            draw_pointer(&mut samples, width, height, x, y);
        }

        let mut rendered = Vec::new();
        {
            let mut encoder = ::png::Encoder::new(&mut rendered, self.width, self.height);
            encoder.set_color(::png::ColorType::Rgb);
            encoder.set_depth(::png::BitDepth::Eight);
            let mut writer = encoder.write_header().ok()?;
            writer.write_image_data(&samples).ok()?;
            writer.finish().ok()?;
        }
        Some(rendered)
    }
}

/// Draw the screen pointer with its tip at `(x, y)`, clipped to the surface: a filled
/// triangle, which is the shape of the arrow an operating system draws for it.
fn draw_pointer(samples: &mut [u8], width: usize, height: usize, x: i32, y: i32) {
    let (Ok(tip_x), Ok(tip_y)) = (usize::try_from(x), usize::try_from(y)) else {
        return;
    };
    for row in 0..POINTER_MARK_PIXELS {
        for column in 0..(POINTER_MARK_PIXELS - row) {
            let pixel_x = tip_x + column;
            let pixel_y = tip_y + row;
            if pixel_x >= width || pixel_y >= height {
                continue;
            }
            let offset = (pixel_y * width + pixel_x) * SAMPLES_PER_PIXEL;
            if let Some(pixel) = samples.get_mut(offset..offset + SAMPLES_PER_PIXEL) {
                pixel.fill(POINTER_SAMPLE);
            }
        }
    }
}

/// What the OS-level actions performed on one browser session have left on it.
///
/// Every field is written only by an action this session actually performed, so nothing
/// here is reported for work that was refused.
#[derive(Debug, Clone, Default)]
pub struct BrowserInputState {
    /// Where this session's pointer stands: the point its last delivered pointer action
    /// left it, or `None` while no pointer action has been performed, in which case the
    /// screen carries no pointer.
    pub pointer: Option<(i32, i32)>,
    /// How far the wheel has scrolled this session horizontally, as the sum of the deltas
    /// its scroll actions delivered.
    pub scroll_x: i32,
    /// The same vertically.
    pub scroll_y: i32,
    /// The mouse button the last click or drag used, as the request named it, or `None`
    /// while no action has named one. `MouseButton` declares no default, so no button is
    /// recorded for an action that named none.
    pub button: Option<String>,
    /// How many pointer clicks this session has received.
    pub clicks: u64,
    /// How many characters have been typed into this session.
    pub typed_characters: u64,
    /// How many key presses — single keys and the keys of a shortcut — have been sent to
    /// it.
    pub key_presses: u64,
    /// How many OS-level actions this session has performed.
    pub actions_performed: u64,
}

impl BrowserSessionRecord {
    /// The screen this session's actions happen on.
    #[must_use]
    pub fn surface(&self) -> BrowserSurface {
        BrowserSurface::of(self.view_port.as_ref())
    }

    /// Move the pointer to `(x, y)`, or say why that point is not on this session's
    /// screen.
    pub fn deliver_pointer_move(&mut self, x: i32, y: i32, now: SystemTime) -> Result<(), String> {
        self.place_pointer(x, y)?;
        self.record_action(now);
        Ok(())
    }

    /// Click `button` `click_count` times at `(x, y)`, leaving the pointer there.
    pub fn deliver_click(
        &mut self,
        x: i32,
        y: i32,
        button: Option<&str>,
        click_count: Option<i32>,
        now: SystemTime,
    ) -> Result<(), String> {
        self.place_pointer(x, y)?;
        let clicks = u64::try_from(click_count.unwrap_or(SINGLE_CLICK)).unwrap_or(0);
        self.input.clicks = self.input.clicks.saturating_add(clicks);
        self.press_button(button);
        self.record_action(now);
        Ok(())
    }

    /// Drag from `start` to `end` with `button` held, leaving the pointer at `end`. A drag
    /// is one gesture over the screen, so BOTH of its points have to be on it.
    pub fn deliver_drag(
        &mut self,
        start: (i32, i32),
        end: (i32, i32),
        button: Option<&str>,
        now: SystemTime,
    ) -> Result<(), String> {
        self.place_pointer(start.0, start.1)?;
        self.place_pointer(end.0, end.1)?;
        self.press_button(button);
        self.record_action(now);
        Ok(())
    }

    /// Deliver a wheel turn of `delta_x` by `delta_y` at `(x, y)`, which moves the
    /// pointer there and scrolls the session by those deltas.
    pub fn deliver_scroll(
        &mut self,
        x: i32,
        y: i32,
        delta_x: i32,
        delta_y: i32,
        now: SystemTime,
    ) -> Result<(), String> {
        self.place_pointer(x, y)?;
        self.input.scroll_x = self.input.scroll_x.saturating_add(delta_x);
        self.input.scroll_y = self.input.scroll_y.saturating_add(delta_y);
        self.record_action(now);
        Ok(())
    }

    /// Type `text` into this session. Text of no characters sends no keystroke and is no
    /// failure: `KeyTypeArguments$text` is modelled with a maximum length and no minimum.
    pub fn deliver_typed_text(&mut self, text: &str, now: SystemTime) {
        let characters = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
        self.input.typed_characters = self.input.typed_characters.saturating_add(characters);
        self.record_action(now);
    }

    /// Press `key` `presses` times, or say that nothing names a key to press.
    pub fn deliver_key_press(
        &mut self,
        key: &str,
        presses: Option<i32>,
        now: SystemTime,
    ) -> Result<(), String> {
        if key.trim().is_empty() {
            return Err(BLANK_KEY.to_string());
        }
        let presses = u64::try_from(presses.unwrap_or(SINGLE_PRESS)).unwrap_or(0);
        self.input.key_presses = self.input.key_presses.saturating_add(presses);
        self.record_action(now);
        Ok(())
    }

    /// Hold `keys` together as one shortcut, or say that one of them names no key. The
    /// list's length is modelled (1..5), so what is left to check is each key itself.
    pub fn deliver_key_shortcut(&mut self, keys: &[String], now: SystemTime) -> Result<(), String> {
        if keys.iter().any(|key| key.trim().is_empty()) {
            return Err(BLANK_KEY.to_string());
        }
        let pressed = u64::try_from(keys.len()).unwrap_or(0);
        self.input.key_presses = self.input.key_presses.saturating_add(pressed);
        self.record_action(now);
        Ok(())
    }

    /// This session's screen as a PNG image. A screenshot REPORTS the surface: it changes
    /// nothing about the session, so a later caller sees exactly what it would have seen
    /// had nothing been screenshotted.
    #[must_use]
    pub fn screenshot_png(&self) -> Option<Vec<u8>> {
        self.surface().render_png(self.input.pointer)
    }

    /// Put the pointer at `(x, y)`, or say why that point is not on this session's
    /// screen: the ONE bounds check every pointer action goes through.
    fn place_pointer(&mut self, x: i32, y: i32) -> Result<(), String> {
        let surface = self.surface();
        if !surface.contains(x, y) {
            return Err(point_off_surface(x, y, surface));
        }
        self.input.pointer = Some((x, y));
        Ok(())
    }

    /// Record the button a click or a drag used, leaving the last one standing when this
    /// action named none.
    fn press_button(&mut self, button: Option<&str>) {
        if let Some(button) = button {
            self.input.button = Some(button.to_string());
        }
    }

    /// Record that this session performed one OS-level action at `now`, which is the
    /// instant every later read reports as the last one that changed it.
    fn record_action(&mut self, now: SystemTime) {
        self.input.actions_performed = self.input.actions_performed.saturating_add(1);
        self.last_updated_at = now;
    }
}

/// How a pointer action that lands off the session's screen says so: the point it asked
/// for and the screen it was asked for on, both of which the caller has to reconcile.
fn point_off_surface(x: i32, y: i32, surface: BrowserSurface) -> String {
    format!(
        "the point ({x}, {y}) is outside this session's viewport of {}x{} pixels",
        surface.width, surface.height
    )
}

/// How a keyboard action with a blank key says so.
const BLANK_KEY: &str = "a blank key names no key to send";
