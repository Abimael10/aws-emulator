// SPDX-License-Identifier: CC0-1.0

//! The RULES drawn on a page: the ruling lines of a table, a cell border, an underline.
//!
//! A rule is a long thin straight run of ink, and it is page STRUCTURE rather than text: no
//! face draws a character as a stroke many times longer than it is thick, and what a rule
//! does on a page is divide it — the ruling lines of a table are where one cell of a row
//! ends and the next begins, and the borders of that table are where it ends. So the ink of
//! a rule is struck out of the page the characters are read from ([`Ink::without`]): a
//! ruling line is never reported as the stem character it resembles, it never joins the
//! marks of the row it bounds into one, and it never stands in the frame, the pitch or the
//! word separation the characters beside it are measured against. The rules that stand
//! BETWEEN two marks of a line are what tells that row's cells apart ([`Rule::divides`]).
//!
//! A rule is told from a character by two measurements of the page's own ink:
//!
//! * how far out of square the run is ([`RULE_ASPECT`]) — the one property of a ruling line
//!   that does not depend on the size the page was drawn at;
//! * how much further it runs than the page's own CHARACTERS are tall ([`RULE_REACH`]) — a
//!   face's narrowest stem is as tall as its own letters, while a ruling line runs the
//!   height of a whole table row and the width of a whole table.
//!
//! The second is what a page's own text measures, so it is read off the page rather than
//! fixed in advance: the candidates are struck first and the characters of what is left are
//! measured ([`characters_of`]), which is the page's text with its rules out of it however
//! the page was ruled. A page whose marks are ALL as far out of square as a rule measures no
//! characters at all, and nothing on it is read as a rule: there is no text there for a rule
//! to be structure around.
//!
//! Finding the candidates is two scans of the page — down its columns for the rules that run
//! down it, across its rows for the ones that run across it. Nothing here is echoed from the
//! request, keyed to a digest or randomised: a rule is ink that was found on the page.

use super::{GlyphBox, Ink};

/// How many times longer than it is thick a run of ink must be to be a RULE rather than a
/// character's own stroke.
///
/// No face draws a character this far out of square: the narrowest stem a face sets is a
/// fraction of its own capital height rather than a fifteenth of it, while a ruling line
/// runs the height of a table row or the width of a table at the thickness of a hairline.
const RULE_ASPECT: usize = 15;

/// How many times further than the page's own CHARACTERS are tall a rule must run.
///
/// A bare stem — the capital i, the small l, the figure one — is as tall as the line's own
/// letters and as thin as a hairline, so the aspect alone would read it as a rule, and so
/// would the crossbar of a `T` set in a face whose strokes are a pixel wide. What a drawn
/// rule does and a character cannot is run clear past the page's own text: the ruling line
/// of a table spans a whole row of it, text and the space around it, and the rule that
/// borders a table spans every column of it.
const RULE_REACH: f32 = 2.0;

/// The least a rule may measure along its own length, in pixels: shorter ink than this is
/// speckle or one mark of a page drawn very small, not a ruling line. NOT an AWS limit — it
/// is also what bounds the scans below, since no shorter run of ink is considered at all.
const MIN_RULE_LENGTH: usize = 12;

/// How far the two ends of one rule may wander, as a fraction of its own length, from one of
/// its columns (or rows) to the next: a hairline drawn by a rasteriser, or scanned, ends a
/// pixel or two further along in one column than in the one beside it, while two runs whose
/// ends stand further apart than this are two pieces of ink rather than one rule.
const RULE_END_DRIFT: usize = 8;

/// The most runs of ink one column (or row) of a page may hold for a rule to be looked for
/// in it at all. NOT an AWS limit: a column holding this many long runs carries a pattern —
/// a dither, a hatching — rather than a ruling line, and this is what keeps the scans linear
/// in the page's pixels whatever the page holds.
const MAX_RULE_RUNS: usize = 64;

/// The most marks the page's own character height is measured over. NOT an AWS limit: the
/// median of this many of a page's marks is its character height, and a page holding more of
/// them is measured over the ones its first lines carry rather than over all of them, so the
/// measurement costs a fixed amount of memory whatever the page holds.
const MAX_MEASURED_MARKS: usize = 4_096;

/// The most rules one page may be read as carrying. NOT an AWS limit: every mark of every
/// line is weighed against the rules that cross its line, so this is what keeps that cost a
/// fixed multiple of the page's marks instead of the product of two counts the caller
/// chooses. A page drawn with more long thin runs of ink than this carries a pattern — a
/// hatching, a dither of hairlines — rather than the ruling of a table, and the rest of its
/// runs are left as the ink they stand as: a page of ruled tables holds a few dozen rules.
const MAX_RULES: usize = 256;

/// A rule drawn on a page: where its ink sits, and which way it runs.
#[derive(Debug, Clone, Copy)]
pub(super) struct Rule {
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    /// Whether the rule runs DOWN the page — a column divider or a side border — rather than
    /// across it.
    down: bool,
}

impl Rule {
    /// Whether this rule is drawn DOWN the page and crosses a line of the rows `top..=bottom`,
    /// which is what lets it bound a cell of that line at all.
    ///
    /// A ruling line is drawn the full height of the row it bounds, while ink that merely
    /// reaches into a line from the one above it bounds nothing of this one; and a rule drawn
    /// ACROSS the page bounds no cell of a line — it is a border or an underline, which
    /// separates that line from what is above or below it.
    pub(super) fn crosses(&self, top: usize, bottom: usize) -> bool {
        if !self.down || self.y0 > bottom || self.y1 < top {
            return false;
        }
        let crossed = self.y1.min(bottom) + 1 - self.y0.max(top);
        2 * crossed >= bottom + 1 - top
    }

    /// Whether this rule stands between the marks ending at column `before` and starting at
    /// column `after`.
    ///
    /// That is what makes it a CELL boundary: a rule drawn between two marks of one line puts
    /// them in two cells of that row, however wide or narrow the gap between them is, and two
    /// rules with no mark between them bound a cell nothing was drawn in.
    pub(super) fn divides(&self, before: usize, after: usize) -> bool {
        self.x0 > before && self.x1 < after
    }

    /// The column this rule's own ink begins at, which the rules of a page are held in order
    /// of.
    pub(super) fn left(&self) -> usize {
        self.x0
    }

    /// The column this rule's own ink ends at.
    pub(super) fn right(&self) -> usize {
        self.x1
    }

    /// How far this rule runs, in pixels.
    fn length(&self) -> usize {
        let (long, _) = span(self.x1 + 1 - self.x0, self.y1 + 1 - self.y0);
        long
    }
}

/// The longer and the shorter of two sides.
fn span(width: usize, height: usize) -> (usize, usize) {
    (width.max(height), width.min(height))
}

/// How far out of square `mark` is: how many times longer its longer side runs than its
/// shorter one.
fn out_of_square(mark: &GlyphBox) -> usize {
    let (long, short) = span(mark.width(), mark.height());
    long / short.max(1)
}

impl Ink {
    /// The rules drawn on this page, in column order.
    ///
    /// Two scans for the candidates — the runs of ink down each column of the page, joined
    /// across neighbouring columns into the bars they make up, and the same over its rows —
    /// and then the page's own character height, measured with those candidates struck, to
    /// tell the rules among them from the characters' own strokes. At most [`MAX_RULES`] of
    /// them are kept, and they are ordered by the column their ink begins at so that the
    /// marks of a line can be weighed against them by a search rather than a scan.
    pub(super) fn rules(&self) -> Vec<Rule> {
        let mut candidates = Vec::new();
        bars(
            self.width,
            self.height,
            &|a, b| self.at(a, b),
            &mut |a0, a1, b0, b1| {
                candidates.push(Rule {
                    x0: a0,
                    x1: a1,
                    y0: b0,
                    y1: b1,
                    down: true,
                });
            },
        );
        bars(
            self.height,
            self.width,
            &|a, b| self.at(b, a),
            &mut |a0, a1, b0, b1| {
                candidates.push(Rule {
                    x0: b0,
                    x1: b1,
                    y0: a0,
                    y1: a1,
                    down: false,
                });
            },
        );
        if candidates.is_empty() {
            return candidates;
        }
        // What the page's CHARACTERS measure, with every candidate out of the way: a page
        // whose text is ruled, boxed or underlined cannot be measured as it stands, since a
        // rule joins the marks of the lines it runs through into one.
        let Some(characters) = self
            .without(&candidates)
            .as_ref()
            .and_then(characters_of)
        else {
            return Vec::new();
        };
        candidates.retain(|rule| rule.length() as f32 >= RULE_REACH * characters);
        // The longest first, so a page carrying more runs of ink than one holds rules keeps
        // the ones that are most clearly its ruling; then in column order, which is the order
        // the marks of a line are weighed against them in.
        candidates.sort_by_key(|rule| std::cmp::Reverse(rule.length()));
        candidates.truncate(MAX_RULES);
        candidates.sort_by_key(|rule| (rule.x0, rule.y0));
        candidates
    }

    /// This page with `rules` struck out of it, or `None` when there are none.
    ///
    /// What is left is the page's TEXT: the segmentation, the naming and the measurement all
    /// read that. The page's own CONTENT is measured off the page as it stands, so a rule is
    /// still part of what was found on the page it is drawn on.
    pub(super) fn without(&self, rules: &[Rule]) -> Option<Self> {
        if rules.is_empty() {
            return None;
        }
        let mut ink = self.ink.clone();
        for rule in rules {
            for y in rule.y0..=rule.y1.min(self.height.saturating_sub(1)) {
                for x in rule.x0..=rule.x1.min(self.width.saturating_sub(1)) {
                    ink[y * self.width + x] = 0;
                }
            }
        }
        Some(Self {
            width: self.width,
            height: self.height,
            ink,
        })
    }
}

/// How tall the CHARACTERS of `ink` are: the median height of the marks the lines of ink on
/// it fall into, leaving out the marks that are themselves as far out of square as a rule.
///
/// `None` for a page that carries no such mark, which is a page with no text on it to
/// measure.
fn characters_of(ink: &Ink) -> Option<f32> {
    let mut heights: Vec<f32> = Vec::new();
    for (top, bottom) in ink.bands() {
        if heights.len() >= MAX_MEASURED_MARKS {
            break;
        }
        for mark in ink.glyphs_between(top, bottom) {
            if heights.len() >= MAX_MEASURED_MARKS {
                break;
            }
            if out_of_square(&mark) < RULE_ASPECT {
                heights.push(mark.height() as f32);
            }
        }
    }
    super::quantile(&heights, 0.5).filter(|height| *height >= 1.0)
}

/// One bar of ink being accumulated across the page's columns (or rows): how far it reaches
/// ACROSS the scans, and how far it runs ALONG them.
#[derive(Debug, Clone, Copy)]
struct Bar {
    a0: usize,
    a1: usize,
    b0: usize,
    b1: usize,
}

/// Find the bars of ink `span` scans of `depth` pixels hold, and hand each one that is far
/// enough out of square to be a rule to `take` as `(a0, a1, b0, b1)` — its extent across the
/// scans, and along them.
///
/// `inked(a, b)` is the pixel `b` of scan `a`, so one implementation serves both ways a rule
/// can run: the caller passes the page's columns for the rules that run down it and its rows
/// for the ones that run across it.
fn bars(
    span: usize,
    depth: usize,
    inked: &impl Fn(usize, usize) -> bool,
    take: &mut impl FnMut(usize, usize, usize, usize),
) {
    let mut open: Vec<Bar> = Vec::new();
    for a in 0..span {
        let runs = runs_of(a, depth, inked);
        let mut still: Vec<Bar> = Vec::with_capacity(runs.len());
        let mut used = vec![false; runs.len()];
        for bar in open.drain(..) {
            let joined = runs
                .iter()
                .enumerate()
                .find(|(index, (b0, b1))| !used[*index] && joins(&bar, *b0, *b1));
            match joined {
                Some((index, (b0, b1))) => {
                    used[index] = true;
                    still.push(Bar {
                        a0: bar.a0,
                        a1: a,
                        b0: bar.b0.min(*b0),
                        b1: bar.b1.max(*b1),
                    });
                }
                None => settle(&bar, take),
            }
        }
        for (_, (b0, b1)) in runs.iter().enumerate().filter(|(index, _)| !used[*index]) {
            still.push(Bar {
                a0: a,
                a1: a,
                b0: *b0,
                b1: *b1,
            });
        }
        open = still;
    }
    for bar in &open {
        settle(bar, take);
    }
}

/// Hand `bar` to `take` when it is far enough out of square to be a rule.
fn settle(bar: &Bar, take: &mut impl FnMut(usize, usize, usize, usize)) {
    let length = bar.b1 + 1 - bar.b0;
    let thickness = bar.a1 + 1 - bar.a0;
    if length >= MIN_RULE_LENGTH && length >= RULE_ASPECT * thickness {
        take(bar.a0, bar.a1, bar.b0, bar.b1);
    }
}

/// The runs of ink scan `a` holds that are long enough to be part of a rule, in order.
///
/// A scan holding more than [`MAX_RULE_RUNS`] of them carries a pattern rather than a ruling
/// line, and contributes none.
fn runs_of(a: usize, depth: usize, inked: &impl Fn(usize, usize) -> bool) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut b = 0usize;
    while b < depth {
        if !inked(a, b) {
            b += 1;
            continue;
        }
        let from = b;
        while b < depth && inked(a, b) {
            b += 1;
        }
        if b - from >= MIN_RULE_LENGTH {
            if out.len() >= MAX_RULE_RUNS {
                return Vec::new();
            }
            out.push((from, b - 1));
        }
    }
    out
}

/// Whether the run `b0..=b1` of this scan is part of the same bar as `bar`: both of its ends
/// stand within [`RULE_END_DRIFT`] of the bar's own.
fn joins(bar: &Bar, b0: usize, b1: usize) -> bool {
    let drift = ((bar.b1 + 1 - bar.b0) / RULE_END_DRIFT).max(1);
    bar.b0.abs_diff(b0) <= drift && bar.b1.abs_diff(b1) <= drift
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page of `width` by `height` pixels with no ink on it.
    fn page(width: usize, height: usize) -> Ink {
        Ink {
            width,
            height,
            ink: vec![0u8; width * height],
        }
    }

    /// The rectangle a case of the tables below inks: `x0..=x1` by `y0..=y1`, in pixels.
    type Drawn = (usize, usize, usize, usize);

    /// Ink the rectangle `x0..=x1` by `y0..=y1` as solidly as a stroke.
    fn draw(page: &mut Ink, (x0, x1, y0, y1): Drawn) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                page.ink[y * page.width + x] = u8::MAX;
            }
        }
    }

    /// Three marks the size of a line's own characters, which is what a rule's reach is
    /// measured against.
    fn characters(page: &mut Ink) {
        for mark in 0..3usize {
            draw(page, (10 + mark * 12, 18 + mark * 12, 10, 19));
        }
    }

    /// Ink is a rule when it is both far out of square and runs clear past the page's own
    /// characters; everything else on the page is text.
    ///
    /// The rows differ along what the rule generalises over: which way the ink runs, how
    /// thick and how long it is, and the size the same shape is drawn at.
    #[test]
    fn ink_running_clear_past_the_pages_characters_is_a_rule() {
        // (case, the ink drawn beside the characters, whether it is a rule)
        let cases: Vec<(&str, Drawn, bool)> = vec![
            ("a hairline drawn down a table's rows", (60, 60, 4, 60), true),
            ("a two-pixel rule drawn down them", (60, 61, 4, 60), true),
            ("a rule drawn across the page", (4, 100, 40, 41), true),
            ("a bare stem as tall as the characters", (60, 60, 10, 19), false),
            ("the crossbar of a letter", (60, 72, 10, 10), false),
            ("a short run no longer than it is thick", (60, 69, 30, 39), false),
            ("a block of ink", (60, 100, 30, 70), false),
        ];
        for (case, ink, is_rule) in cases {
            let mut drawn = page(140, 140);
            characters(&mut drawn);
            draw(&mut drawn, ink);
            let found = drawn.rules();
            assert_eq!(
                !found.is_empty(),
                is_rule,
                "case {case:?}: ink of {}x{} pixels must{} be read as a rule, got {found:?}",
                ink.1 + 1 - ink.0,
                ink.3 + 1 - ink.2,
                if is_rule { "" } else { " not" }
            );
        }
    }

    /// The same page drawn four times the size reads the same way: what tells a rule from a
    /// character is measured off the page's own ink.
    #[test]
    fn the_same_page_drawn_larger_reads_the_same_way() {
        let scale = |page: &Ink, by: usize| {
            let mut large = super::super::Ink {
                width: page.width * by,
                height: page.height * by,
                ink: vec![0u8; page.width * by * page.height * by],
            };
            for y in 0..large.height {
                for x in 0..large.width {
                    large.ink[y * large.width + x] = page.ink[(y / by) * page.width + (x / by)];
                }
            }
            large
        };
        let mut small = page(140, 140);
        characters(&mut small);
        draw(&mut small, (60, 60, 4, 60));
        let large = scale(&small, 4);
        assert_eq!(
            small.rules().len(),
            large.rules().len(),
            "a page drawn larger must be read the same way, got {:?} against {:?}",
            small.rules(),
            large.rules()
        );
    }

    /// A page whose every mark is as far out of square as a rule measures no characters, so
    /// nothing on it is read as a rule: there is no text there for a rule to be structure
    /// around.
    #[test]
    fn a_page_of_nothing_but_thin_marks_carries_no_rule() {
        let mut drawn = page(140, 140);
        for mark in 0..3usize {
            draw(&mut drawn, (10 + mark * 12, 10 + mark * 12, 10, 40));
        }
        assert!(
            drawn.rules().is_empty(),
            "a page of nothing but thin marks carries no rule, got {:?}",
            drawn.rules()
        );
    }

    /// A rule divides the marks standing either side of it on a line it crosses, and nothing
    /// else: a rule drawn across the page is a border, which bounds no cell of a line.
    #[test]
    fn a_rule_down_the_page_divides_the_marks_either_side_of_it() {
        let mut drawn = page(140, 140);
        characters(&mut drawn);
        draw(&mut drawn, (60, 61, 4, 60));
        draw(&mut drawn, (4, 100, 80, 81));
        let found = drawn.rules();
        assert_eq!(
            found.len(),
            2,
            "the page draws a rule down it and one across it, got {found:?}"
        );
        let down = found.iter().find(|rule| rule.down).expect("the divider");
        let across = found.iter().find(|rule| !rule.down).expect("the border");
        // (case, the marks either side, the line's own rows, whether the rule divides them)
        let cases: Vec<(&str, usize, usize, usize, usize, bool)> = vec![
            ("two marks either side of it", 40, 70, 10, 19, true),
            ("two marks that both stand left of it", 20, 40, 10, 19, false),
            ("two marks that both stand right of it", 70, 90, 10, 19, false),
            ("a line the rule does not reach", 40, 70, 100, 120, false),
            ("a line the rule crosses a sliver of", 40, 70, 56, 100, false),
        ];
        for (case, before, after, top, bottom, divides) in cases {
            assert_eq!(
                down.crosses(top, bottom) && down.divides(before, after),
                divides,
                "case {case:?}: the rule {down:?} must{} divide the marks ending at {before} \
                 and starting at {after} on the rows {top}..={bottom}",
                if divides { "" } else { " not" }
            );
            assert!(
                !across.crosses(top, bottom),
                "case {case:?}: a rule drawn across the page bounds no cell of a line"
            );
        }
    }

    /// The page the rules are struck from carries the same ink everywhere else: striking is
    /// what keeps a ruling line out of the recogniser, not a redrawing of the page.
    #[test]
    fn striking_a_rule_leaves_the_rest_of_the_page_as_it_was() {
        let mut drawn = page(140, 140);
        characters(&mut drawn);
        draw(&mut drawn, (60, 61, 4, 60));
        let rules = drawn.rules();
        let struck = drawn.without(&rules).expect("the page carries a rule");
        assert!(
            !struck.at(60, 40) && !struck.at(61, 40),
            "the rule's own ink must be struck"
        );
        assert!(
            struck.at(10, 10) && struck.at(18, 19),
            "the text beside the rule must be left as it was drawn"
        );
        assert!(
            drawn.at(60, 40),
            "the page as it stands keeps its rule, since the page's content includes it"
        );
        assert!(
            drawn.without(&[]).is_none(),
            "a page with no rule on it is not copied at all"
        );
    }
}
