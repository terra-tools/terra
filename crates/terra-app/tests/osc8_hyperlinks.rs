//! OSC 8 — the escape that lets a program label a link.
//!
//! `ESC ] 8 ; params ; URI ST` opens a hyperlink; every cell written until the
//! matching empty `ESC ] 8 ; ; ST` carries it. The point is that the *label*
//! and the *target* differ: `ls --hyperlink`, `gh`, cargo's diagnostics and
//! most modern CLIs print a short word and hide a URL behind it. A terminal
//! that only ever hunts for URL-shaped text on screen (which is all terra did)
//! can never open those, because the URL is not on screen.
//!
//! Both terminators are in the wild: ST (`ESC \`) is what the spec says, BEL
//! (`\a`) is what half the emitters send. alacritty's parser takes either, so
//! both are pinned here.
//!
//! Everything is headless and at the egui_term boundary:
//! `BackendCommand::ProcessLink(LinkAction::Hover, point)` is exactly what the
//! view sends while the user holds Cmd, and the assertion is on the range the
//! backend resolved plus the URI it stored. Nothing here opens anything — the
//! test suite must not launch a browser.
#![cfg(unix)]

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use egui::{Modifiers, Pos2, Rect};
use egui_term::{
    BackendCommand, BackendSettings, LinkAction, PtyEvent, TerminalBackend, TerminalView,
};

const SCREEN: Rect = Rect {
    min: Pos2::ZERO,
    max: Pos2::new(800.0, 600.0),
};

fn frame(ctx: &egui::Context, backend: &mut TerminalBackend) {
    let input = egui::RawInput {
        screen_rect: Some(SCREEN),
        modifiers: Modifiers::NONE,
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |ui: &mut egui::Ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            let view = TerminalView::new(ui, backend)
                .set_focus(true)
                .set_size(ui.available_size());
            ui.add(view);
        });
    });
}

/// The visible grid as text, spacers dropped — for diagnostics and for
/// `wait_for`.
fn screen_text(backend: &mut TerminalBackend) -> String {
    backend
        .sync()
        .grid
        .display_iter()
        .filter(|c| !c.cell.flags.contains(Flags::WIDE_CHAR_SPACER))
        .map(|c| c.cell.c)
        .collect()
}

fn sh(ctx: &egui::Context, script: &str) -> (TerminalBackend, Receiver<(u64, PtyEvent)>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let backend = TerminalBackend::new(
        0,
        ctx.clone(),
        tx,
        BackendSettings {
            shell: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            ..Default::default()
        },
    )
    .expect("spawn pty");
    (backend, rx)
}

fn wait_for(
    ctx: &egui::Context,
    backend: &mut TerminalBackend,
    what: &str,
    mut pred: impl FnMut(&mut TerminalBackend) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        frame(ctx, backend);
        if pred(backend) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; the grid holds {:?}",
            screen_text(backend).trim_end()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Where `needle` sits on the visible grid, as the point of its first char.
///
/// Walks the rows by hand rather than searching `screen_text`, because a wide
/// char occupies two columns and a flat string index would drift past it.
fn find(backend: &mut TerminalBackend, needle: &str) -> Point {
    let content = backend.sync();
    let grid = &content.grid;
    let offset = grid.display_offset() as i32;
    let needle: Vec<char> = needle.chars().collect();
    for row_index in 0..grid.screen_lines() as i32 {
        let line = Line(row_index - offset);
        let row: Vec<char> = (0..grid.columns())
            .map(|c| grid[line][Column(c)].c)
            .collect();
        for start in 0..row.len() {
            let mut col = start;
            let mut matched = 0;
            while matched < needle.len() && col < row.len() {
                if row[col] != needle[matched] {
                    break;
                }
                // Step over the spacer half of a double-width glyph.
                let wide = grid[line][Column(col)].flags.contains(Flags::WIDE_CHAR);
                col += if wide { 2 } else { 1 };
                matched += 1;
            }
            if matched == needle.len() {
                return Point::new(line, Column(start));
            }
        }
    }
    panic!(
        "{needle:?} is not on screen; the grid holds {:?}",
        screen_text(backend).trim_end()
    );
}

/// The text the resolved range covers, so a range can be asserted as the words
/// a user would see underlined rather than as raw coordinates.
fn range_text(backend: &mut TerminalBackend) -> Option<String> {
    let content = backend.sync();
    let range = content.hovered_hyperlink.clone()?;
    let grid = &content.grid;
    let mut out = String::new();
    let mut point = *range.start();
    loop {
        let cell = &grid[point];
        if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            out.push(cell.c);
        }
        if point == *range.end() {
            break;
        }
        if point.column.0 + 1 >= grid.columns() {
            point = Point::new(point.line + 1, Column(0));
        } else {
            point.column += 1;
        }
    }
    Some(out)
}

/// Hover a cell and report what the backend resolved: the underlined text and
/// the URI that a Cmd-click would open.
fn hover(
    ctx: &egui::Context,
    backend: &mut TerminalBackend,
    point: Point,
) -> (Option<String>, Option<String>) {
    backend.process_command(BackendCommand::ProcessLink(LinkAction::Hover, point));
    frame(ctx, backend);
    let uri = backend.sync().hovered_hyperlink_uri.clone();
    (range_text(backend), uri)
}

/// One line carrying, in order: an ST-terminated OSC 8 link labelled `Docs`, a
/// plain word, a BEL-terminated one labelled `Bell`, and a bare URL for the
/// regex path to find. `cat` keeps the shell alive so the grid stays put.
const LINE: &str = concat!(
    r"printf 'READY\n'; ",
    r"printf '\033]8;;https://example.com/st\033\\Docs\033]8;;\033\\'; ",
    r"printf ' plain '; ",
    r"printf '\033]8;;https://example.com/bel\aBell\033]8;;\a'; ",
    r"printf ' https://example.com/bare\n'; ",
    r"cat",
);

fn ready(ctx: &egui::Context) -> (TerminalBackend, Receiver<(u64, PtyEvent)>) {
    let (mut backend, rx) = sh(ctx, LINE);
    wait_for(ctx, &mut backend, "the line to paint", |b| {
        let text = screen_text(b);
        text.contains("Docs") && text.contains("Bell") && text.contains("example.com/bare")
    });
    (backend, rx)
}

/// `ESC \`-terminated: the label is `Docs`, the target is not on screen at all.
#[test]
fn an_st_terminated_osc8_resolves_to_its_uri() {
    let ctx = egui::Context::default();
    let (mut backend, _rx) = ready(&ctx);

    // Inside the label, not at its edge — the run has to grow both ways.
    let mut point = find(&mut backend, "Docs");
    point.column += 2;

    let (text, uri) = hover(&ctx, &mut backend, point);
    println!("hovering inside Docs: {text:?} -> {uri:?}");
    assert_eq!(text.as_deref(), Some("Docs"), "the underline is the label");
    assert_eq!(uri.as_deref(), Some("https://example.com/st"));
}

/// `\a`-terminated: same story, the terminator half the world actually sends.
#[test]
fn a_bel_terminated_osc8_resolves_to_its_uri() {
    let ctx = egui::Context::default();
    let (mut backend, _rx) = ready(&ctx);

    let mut point = find(&mut backend, "Bell");
    point.column += 1;

    let (text, uri) = hover(&ctx, &mut backend, point);
    println!("hovering inside Bell: {text:?} -> {uri:?}");
    assert_eq!(text.as_deref(), Some("Bell"));
    assert_eq!(uri.as_deref(), Some("https://example.com/bel"));
}

/// The regex path is untouched: a bare URL still hovers, and reports no
/// out-of-band URI, so `open_link` reads it off the screen as it always has.
#[test]
fn a_bare_url_still_hovers_through_the_regex() {
    let ctx = egui::Context::default();
    let (mut backend, _rx) = ready(&ctx);

    let mut point = find(&mut backend, "https://example.com/bare");
    point.column += 10;

    let (text, uri) = hover(&ctx, &mut backend, point);
    println!("hovering the bare URL: {text:?} -> {uri:?}");
    assert_eq!(text.as_deref(), Some("https://example.com/bare"));
    assert_eq!(uri, None, "a regex match has no out-of-band target");
}

/// A word between the two links belongs to neither, and is not URL-shaped —
/// hovering it must leave nothing underlined.
#[test]
fn a_plain_word_is_not_a_link() {
    let ctx = egui::Context::default();
    let (mut backend, _rx) = ready(&ctx);

    let mut point = find(&mut backend, "plain");
    point.column += 2;

    let (text, uri) = hover(&ctx, &mut backend, point);
    println!("hovering plain: {text:?} -> {uri:?}");
    assert_eq!(text, None);
    assert_eq!(uri, None);
}

/// Two adjacent links with no gap must not merge into one run: the label
/// boundary is the link's identity, not whitespace.
#[test]
fn adjacent_links_stay_separate() {
    let ctx = egui::Context::default();
    let (mut backend, _rx) = sh(
        &ctx,
        concat!(
            r"printf 'READY\n'; ",
            r"printf '\033]8;;https://example.com/one\033\\AAA\033]8;;\033\\'; ",
            r"printf '\033]8;;https://example.com/two\033\\BBB\033]8;;\033\\\n'; ",
            r"cat",
        ),
    );
    wait_for(&ctx, &mut backend, "the two links to paint", |b| {
        screen_text(b).contains("AAABBB")
    });

    let a = find(&mut backend, "AAA");
    let b = Point::new(a.line, a.column + 4);

    let (text, uri) = hover(&ctx, &mut backend, a);
    assert_eq!(text.as_deref(), Some("AAA"), "{uri:?}");
    assert_eq!(uri.as_deref(), Some("https://example.com/one"));

    let (text, uri) = hover(&ctx, &mut backend, b);
    assert_eq!(text.as_deref(), Some("BBB"), "{uri:?}");
    assert_eq!(uri.as_deref(), Some("https://example.com/two"));
}

/// A double-width glyph writes two cells: the glyph and a `WIDE_CHAR_SPACER`.
/// Both come from the same cursor template, so both carry the link — and the
/// right half of a CJK character is as clickable as the left, which is where
/// the pointer lands about half the time.
#[test]
fn the_spacer_half_of_a_wide_glyph_resolves() {
    let ctx = egui::Context::default();
    let (mut backend, _rx) = sh(
        &ctx,
        concat!(
            r"printf 'READY\n'; ",
            r"printf '\033]8;;https://example.com/wide\033\\漢字\033]8;;\033\\\n'; ",
            r"cat",
        ),
    );
    wait_for(&ctx, &mut backend, "the wide label to paint", |b| {
        screen_text(b).contains('漢')
    });

    let glyph = find(&mut backend, "漢");
    assert!(
        backend.sync().grid[Point::new(glyph.line, glyph.column + 1)]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER),
        "the cell after 漢 is not a spacer; this test is checking the wrong cell"
    );

    let (text, uri) = hover(&ctx, &mut backend, Point::new(glyph.line, glyph.column + 1));
    println!("hovering the spacer half: {text:?} -> {uri:?}");
    assert_eq!(text.as_deref(), Some("漢字"));
    assert_eq!(uri.as_deref(), Some("https://example.com/wide"));
}
