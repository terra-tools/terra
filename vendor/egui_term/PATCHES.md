# terra patches to vendored egui_term (upstream rev 31bbc7ab8503)

1. view.rs `is_dim`: upstream `flags.intersects(DIM | DIM_BOLD)` also matched
   plain BOLD (DIM_BOLD == DIM|BOLD in alacritty), rendering all bold text at
   70% alpha via `linear_multiply(0.7)`. Patched to `flags.contains(DIM)`.
   Worth upstreaming to https://github.com/Harzu/egui_term.

2. font.rs/view.rs: added `FontSettings.line_height` (cell-height multiplier,
   like Ghostty's `adjust-cell-height`); glyphs are vertically centered in the
   inflated cell. terra uses 1.3 to match the user's Ghostty config.

3. theme.rs/view.rs: selection colors. Upstream rendered selected cells by
   swapping fg/bg (alacritty INVERSE style), which turns selected text into a
   glaring white block. Added `ColorPalette::selection_background` /
   `selection_foreground` (+ `TerminalTheme::selection_background()` /
   `selection_foreground()`), and view.rs now paints selected cells as a
   Ghostty-style opaque overlay: `bg = selection_background`, and
   `fg = selection_foreground` when the theme sets one (else the cell keeps
   its own fg). `is_inverse` still swaps, and is applied *before* the
   selection override. Selected cells always emit their background rect, even
   when it equals the global background. terra uses #3f638b / #ffffff from the
   "Apple System Colors" Ghostty theme.

   The same patch also replaces the cursor rendering: upstream filled the
   whole cursor cell with `content.cursor.fg` (a solid white block) and
   swapped the glyph's fg/bg in APP_CURSOR mode to keep it legible. Now the
   cursor is a 2px vertical beam at the cell's left edge, painted in the new
   `ColorPalette::cursor_color` (`TerminalTheme::cursor_color()`, default
   #98989d), and the glyph underneath is drawn in its own color with no swap.

4. bidi.rs (new) + backend/mod.rs + view.rs: UAX #9 BiDi Level 1 — right-to-
   left text is reordered for display. Upstream painted logical column `n` at
   `x = n * cell_width` unconditionally, so Hebrew and Arabic rendered
   mirrored.

   `bidi.rs` is a pure module (no egui, no alacritty): `map_row(&[char]) ->
   RowMap` computes one row's logical↔visual permutation using
   `unicode-bidi`'s `ParagraphBidiInfo`, applying rules L1 (trailing-
   whitespace reset), L2 (reordering) and L4 (bracket mirroring — which
   `unicode-bidi` documents as out of scope and leaves to the caller).

   The base direction is chosen per row by `BidiBase` (`[text] bidi_base`,
   default `auto`). Two refinements make autodetection safe in a terminal:

   * Detection and reordering run over the row's **non-blank prefix only**.
     A terminal row is padded to the full column count; let that padding into
     the paragraph and rule L2 drags it into the reversed run, pushing a
     four-letter Hebrew word out to column 76 of an 80-column row.
   * Under `auto`, the row's **leading chrome is held out of the paragraph**
     — the run before the first strong letter, i.e. `⏺` bullets, `❯`
     prompts, `│`/`⎿` box drawing, indentation. Rule P2 skips past those to
     the first strong character, so one Hebrew line would turn the whole row
     RTL and sweep the chrome to the right margin; a bulleted list would have
     its bullets on different sides depending on each item's language. The
     Unicode answer is for the application to wrap its text in U+2068 FSI.
     Terminal applications do not, so terra does it for them.

   `ltr` is available for anyone who wants the old provably-immobile
   behaviour, at the cost of stranding RTL sentence punctuation on the wrong
   side. Historical note on why `ltr` was the original default: A
   terminal row is an addressable cell array, not a paragraph: with
   autodetection a row's visual origin depends on its content, so typing one
   Hebrew character at a prompt teleports the whole row — prompt included —
   to the right margin, because the row's trailing blanks join the reversed
   run. Applications that want an RTL paragraph can still say so explicitly
   with RLM/RLE/RLI, which are honoured.

   The map is computed in `TerminalBackend::sync` (not in the view) and
   stored on `RenderableContent::bidi`, because the renderer and the
   hit-tester must agree exactly — `selection_point` cannot see anything the
   paint loop computes, and two derivations of one permutation would be free
   to drift. Rows with no RTL text take an identity fast path that allocates
   nothing (one integer compare per cell), which is every row of ordinary
   output.

   Consumers: `view.rs` maps the logical column to a visual one for `x`,
   anchors a wide char/spacer pair at its leftmost visual column, puts the
   cursor beam on the visual *right* edge inside an RTL run, and applies L4
   to the painted glyph only. `backend::selection_point` inverts the map so
   clicks land on the right cell, and `selection_side` flips Left/Right
   inside an RTL run — without that, every RTL selection is off by one at
   both ends. `selectable_content` is deliberately untouched: the clipboard
   stays logical and unmirrored.

   `TerminalView::set_bidi(bool)` drives it per frame (terra's `[text] bidi`
   config key, ⇧⌘B). Not covered: Arabic contextual shaping — Arabic is
   ordered correctly but renders as isolated letterforms — and combining
   marks, which upstream never drew.

5. view.rs `process_input`: upstream returned early unless
   `layout.has_focus() && layout.contains_pointer()`, so a terminal that held
   the keyboard still dropped every keystroke while the pointer rested
   anywhere else — over the tab bar right after a tab was clicked (issue #20),
   or off the window entirely. The two input kinds are now addressed
   separately by the new `accepts`: keyboard events (`Text`, `Key`, `Copy`,
   `Paste`) go to whatever has focus, mouse events (`PointerButton`,
   `PointerMoved`, `MouseWheel`) still only to what is under the pointer.
   Focus itself was never the problem — `set_focus(true)` already calls
   `Response::request_focus` every frame, and egui grants no focus on click,
   so the tab bar never took it. Worth upstreaming.

6. view.rs `process_mouse_wheel`: the wheel never consulted the terminal's
   mouse mode, so a program that enabled mouse tracking (claude code, htop)
   got alternate-scroll arrow keys — or nothing but a display scroll — instead
   of the wheel reports it asked for (issue #21). When
   `TermMode::MOUSE_MODE` is active and Shift is not held, each scrolled line
   now becomes a `MouseReport` (`ScrollUp`/`ScrollDown` at the pointer's
   cell), riding the same `BackendCommand::MouseReport` path as clicks, so
   SGR/normal/UTF-8 encodings all come out right. Shift-scroll keeps the old
   behaviour (scrollback on the primary screen, arrows on the alt screen), as
   in every terminal. Guarded by `terra-app/tests/mouse_reporting.rs`. Worth
   upstreaming.

7. emoji.rs (new) + view.rs + Cargo.toml (`skrifa`, `png`): colour emoji in
   the grid (issue #19). epaint's glyph path is outline-coverage-masks only —
   it never reads sbix/CBDT/COLR — so emoji cells are composited as textured
   quads instead: the system emoji font's sbix strikes are embedded PNGs,
   read with `skrifa`, decoded once per (char, pixel size) and cached as egui
   textures in the context. Pictograph planes always qualify; the legacy
   symbol blocks only behind a zero-width U+FE0F. Anything the font has no
   art for — including ZWJ sequences, flags and keycaps, which need shaping —
   falls through to the monochrome text path, as does every platform without
   `/System/Library/Fonts/Apple Color Emoji.ttc`. The tab bar and palette
   still go through epaint and stay monochrome.

8. view.rs: U+23FA ⏺ is drawn as U+25CF ● at paint time. The codepoint
   exists in emoji faces only, whose glyph is a record *button* (a square
   around a dot); TUIs use it as an ANSI-tinted status bullet, which the
   plain geometric circle serves and the button glyph does not. Cell,
   clipboard and capture keep the original character.

9. view.rs `process_left_button` / `process_mouse_move`: **Shift (or Option)
   bypasses mouse reporting**, so text can still be selected inside a program
   that grabbed the mouse. Upstream routed every click to the program the
   moment `TermMode::MOUSE_MODE` was set, which left the user unable to
   select — and therefore unable to copy — anything on screen while claude
   code, htop or vim was running. The new `selection_override` decides it:
   Shift is xterm's convention, Option the macOS one, and a press held under
   either takes the ordinary `SelectStart`/`SelectUpdate` path instead. It is
   deliberately not "any modifier" — Ctrl is one programs want reported with
   the click, and Cmd already means follow-the-link. Nothing else about the
   selection differs: double-click still selects a word, triple-click a line,
   and ⌘C copies through the same `selectable_content`.

   The decision is latched at **press** time (`is_reported_press`) and the
   release follows the press. Re-deciding on release reads the modifiers as
   they are then, which need not be how they were: let go of Shift before the
   mouse button and the program that never saw the press got an orphan
   release — it believes the button is still down — while terra's own drag
   never ended, so `is_dragged` stayed set and the next pointer move kept
   extending the selection. The same latch drives drag motion, which now
   also fixes reporting under `MOUSE_MOTION` (modes 1002/1003): a reported
   press left `is_dragged` false, so the motion branch was unreachable and a
   drag inside tmux or vim sent press and release with nothing in between.

   Scroll is untouched — Shift-scroll keeps the item 6 behaviour (scrollback
   on the primary screen, arrows on the alt screen). Guarded by
   `terra-app/tests/shift_selection.rs`. Worth upstreaming.

10. backend/tap.rs (new) + backend/mod.rs + backend/settings.rs + Cargo.toml
    (`polling`): **an optional tee on the child→terminal byte stream**.
    `BackendSettings.output_tap` takes an `Arc<dyn Fn(&[u8]) + Send + Sync>`
    which is called with every chunk read from the PTY, on the reader thread,
    before the parser sees it. terra keeps a bounded per-tab ring of those
    bytes so `terra transcript` can read back what a full-screen program
    painted — output that exists nowhere else once the screen is cleared
    (`terra-app/src/transcript.rs`).

    The tee is a `TappedPty<P>` wrapper around alacritty's `Pty`, which works
    because `EventLoop` is generic over `EventedPty`; no alacritty fork is
    involved. The one subtlety is `EventedReadWrite::reader(&mut self) -> &mut
    Self::Reader`: the wrapper has to hand back a reader it owns, while the
    bytes come from one the wrapped PTY owns. `Tap` therefore holds a raw
    pointer to the inner reader, re-stored on every `reader()` call — sound
    because the returned `&mut Tap` borrows the whole `TappedPty`, so the
    inner reader cannot move or drop while the only route to `Tap::read` is
    alive. See the SAFETY note on `Tap::src`.

    With no tap installed the wrapper is a pure delegation (one pointer store
    and one `Option` check per read), so it is applied unconditionally and
    `[tabs] transcript_kb = 0` costs nothing.

11. view.rs `process_keyboard_event` (+ the new `paste_bytes` /
    `clipboard_key_is_passthrough`): **bracketed paste, and a Ctrl+V that
    pastes**. Upstream wrote the clipboard text to the PTY raw, whatever the
    terminal's mode. A program that had set DECSET 2004 (`ESC[?2004h`) asked to
    be told where a paste begins and ends, so it can take the text as *data*
    rather than as typing — and every shell, editor and agent that gates on the
    marker (readline, zsh, vim, codex, claude) treated a paste as a burst of
    keystrokes instead: a multi-line snippet ran line by line. This is the
    reported "I can't paste into terra".

    `paste_bytes(text, bracketed)` is the whole decision, as a pure function:

    * `BRACKETED_PASTE` set → `ESC[200~` + payload + `ESC[201~`.
    * Line endings are normalised to `\r` in **both** modes. A paste is not a
      key sequence, and the byte the Enter key produces is CR; alacritty does
      this for unbracketed pastes, and iTerm2 and xterm do it inside the
      brackets too, where a payload full of LFs makes readline's paste handler
      and tmux's buffer disagree about how many lines arrived.
    * The payload is sanitised the way xterm sanitises it: neither marker may
      appear *inside* the brackets, or a crafted snippet could close the bracket
      early and have its remainder run as typing. Stripping loops to a fixpoint,
      because removing one marker can splice a fresh one out of its neighbours
      (`ESC[20` + `ESC[201~` + `1~`). Alacritty instead deletes every `\x1b` and
      `\x03` from a bracketed payload, which is blunter — it eats the SGR
      colours in text copied out of another terminal — for the same protection.

    The other half is the non-macOS branch, whose logic was inverted. `egui_winit`
    turns `modifiers.command + C/V` into `Event::Copy`/`Event::Paste` and swallows
    the `Event::Key`, and off macOS `command` **is** Ctrl. Upstream's fix was to
    write `^V` (and `^C`) unless `COMMAND | SHIFT` was held. Keeping a passthrough
    is right — a Linux terminal where Ctrl+C copies instead of interrupting is
    unusable, and Ctrl+Shift+C / Ctrl+Shift+V is what gnome-terminal, konsole and
    xterm do — but "everything that is not Ctrl+Shift" was too wide: Windows'
    Shift+Insert and Ctrl+Insert, and the dedicated `Key::Copy`/`Key::Paste` media
    keys, carry no command modifier and so pasted a `^V`. `clipboard_key_is_passthrough`
    now names the exact spelling — command held, shift not — and everything else
    reaches the clipboard. **On macOS it is always false**: ⌘ is the clipboard
    modifier, Ctrl+C/Ctrl+V never arrive as `Copy`/`Paste` at all, and a paste is
    always a paste. Guarded by `view.rs`'s `paste_tests` / `clipboard_key_tests`
    and `terra-app/tests/issue_paste_repro.rs`. Worth upstreaming.

12. backend/mod.rs + lib.rs: **OSC 52, the clipboard *write* direction** — how a
    program hands text out to the terminal's clipboard, and the missing half of
    "a plain mouse drag inside tmux should copy, like Ghostty". Inside `ssh` →
    `tmux` (`mouse on`) a drag belongs to tmux: tmux paints the selection and, on
    release, emits `ESC]52;c;<base64>BEL` upstream. It is the only route from the
    far end of an ssh connection to this Mac's pasteboard, and terra dropped it,
    leaving a selection the user could see and could not paste.

    Almost no plumbing was needed: alacritty already raises
    `Event::ClipboardStore(ClipboardType, String)` with the payload decoded, and
    the pty event thread already forwards every alacritty event to the embedder
    as a `PtyEvent`. Three changes make it usable:

    * `ClipboardType` is re-exported (`lib.rs`), since the event carries it and
      the embedder has to match on it. terra-app maps both `Clipboard` and
      `Selection` onto the one macOS pasteboard via `ctx.copy_text`, which is the
      same route ⌘C already takes.
    * `ClipboardStore` joins `Exit` and `Title` in the set of events that request
      a repaint even while the tab is not the one on screen — the store is acted
      on by the UI thread, which only runs during a frame. Measured caveat: a
      *fully occluded* window is parked by AppKit and a requested repaint does
      not unpark it, so a copy made there lands on the next frame the window
      draws. The gesture this exists for — a drag the user is watching — always
      has one.
    * `Event::ClipboardLoad` — the *read* direction, `ESC]52;c;?` — is answered
      with an **empty string** and nothing else. A program asking to read the
      clipboard is asking terra to hand whatever the user last copied (a
      password, a token) to code running on the far end of an ssh connection.
      alacritty's `term::Config` already defaults to `Osc52::OnlyCopy`, which
      denies the load before it becomes an event at all; the arm is belt and
      braces against that default ever moving.

    Guarded by `terra-app/tests/osc52_clipboard.rs`, which drives both the bare
    sequence and a real tmux server. Measured finding worth recording: tmux 3.5a
    needs **no configuration** for the copy-out direction — `set-clipboard`
    already defaults to `external`, and tmux's built-in
    `terminal-features[0] xterm*:clipboard` grants the `Ms` capability to terra's
    `TERM=xterm-256color` without consulting terminfo (macOS's system
    `xterm-256color` entry has no `Ms`). `set -s set-clipboard on` governs the
    opposite direction — whether tmux *accepts* OSC 52 from programs inside it.

13. view.rs `process_mouse_move`: **motion reports under button-event tracking
    (DECSET 1002)**. Upstream only forwarded drag motion when the program had
    asked for *any-motion* tracking (1003), but the mode every mouse-aware TUI
    actually sets — tmux, vim, htop — is 1002, motion-while-a-button-is-held.
    Terra therefore delivered a drag as press…silence…release: two clicks at
    different cells. tmux cannot start its mouse selection from that, which is
    why a plain drag inside `tmux` (`mouse on`) selected nothing while the same
    drag in Ghostty selected and copied (item 12 carries the copy half). The
    gate is now `MOUSE_DRAG | MOUSE_MOTION` — the press that starts the gesture
    is a *reported* press, so the button is held by construction and reporting
    under either mode is correct. `MouseButton::LeftMove` (32) already encoded
    the motion flag; it simply never fired.

14. view.rs `process_input` / `accepts` (+ the new `track_grid_position`):
    **the wheel is routed by hover, not by focus**. With the window split into
    panes only the focused one scrolled, because item 5 left the whole function
    behind an early `if !layout.has_focus() { return }`: the wheel over any
    other pane went nowhere, and the user had to click a pane before the mouse
    worked in it. iTerm2 and Ghostty both scroll whatever is under the cursor
    and leave the keyboard where it is, which is what a wheel is *for* — it
    names its target by pointing at it.

    `accepts` now takes `focused` as well as `hovered`, and the three kinds
    separate cleanly:

    * keyboard (`Text`, `Key`, `Copy`, `Paste`) → **focus alone**, wherever the
      pointer rests (item 5's fix, unchanged).
    * `MouseWheel` → **hover alone**. Only one view can contain the pointer, so
      exactly one pane acts on a given wheel event; the focused pane no longer
      gets a copy of a scroll aimed elsewhere.
    * `PointerButton` / `PointerMoved` → **hover *and* focus**, which is what
      they already effectively required. Click-to-focus (terra-app watches the
      press itself) and drag-selection therefore behave exactly as before: a
      drag still starts only in the pane that holds focus, and loosening the
      wheel does not silently loosen the selection with it.

    The early return survives as `if !focused && !hovered`, so a pane that is
    neither still costs nothing.

    One thing had to move for the alternate-screen half to be right. Wheel
    *reports* (item 6) are sent at `state.current_mouse_position_on_grid`,
    which only `process_mouse_move` maintains — and an unfocused view takes no
    pointer events, so its cached cell is wherever the pointer was when it last
    held focus, or the default (0, 0). The pixel→cell conversion is now
    `track_grid_position`, called from the `MouseWheel` arm too when the view
    is unfocused, so a program in mouse mode in a hovered pane is told the cell
    the pointer is actually over. Nothing else about reporting changes: the
    reports go to that pane's PTY and to no other.

    Nothing in terra-app had to move for this. `set_focus` still names the
    focused group only, and `scrollbar.rs` was already hover-driven
    (`rect_contains_pointer`) and reads its own group's backend, so the hovered
    pane's thumb is the one that reacts.

    terra went a step further *on top of* this patch — `main.rs::hover_focus`
    moves the keyboard to the pane the pointer moves into, so in the ordinary
    case the hovered pane is also the focused one. That is deliberately terra
    policy and not part of this patch: a `TerminalView` still learns about
    focus only through `set_focus`, and hover routing remains the layer
    underneath, the one that keeps the wheel working in the cases where focus
    is *not* allowed to follow — a modal is up, or a drag is in progress.
    Guarded by `terra-app/tests/hover_scroll.rs` and `accepts_tests`. Worth
    upstreaming.

15. backend/mod.rs: **the PTY env names the terminal — `TERM` and `COLORTERM`
    included**. The spawn already inserted `TERM_PROGRAM`/`_VERSION` so
    children stop believing they run in whatever launched terra; but `TERM`
    itself was still inherited. A terra started from a tmux shell handed every
    tab `TERM=screen`; started where no TERM exists (Finder, a CI runner) the
    child got none, and curses programs refuse a blank terminal — the CI
    symptom was tmux's "open terminal failed: terminal does not support
    clear" in the PTY harness suites. alacritty the app fixes this by calling
    `tty::setup_env()` at startup, which nothing in terra ever did. The env
    map now pins `TERM=xterm-256color` (what terra emulates and documents)
    and `COLORTERM=truecolor`.

16. view.rs `TerminalViewState` + the new `TerminalView::drag_autoscroll`:
    **a selection drag that runs past the edge scrolls the viewport**. Upstream
    extended a selection only from `PointerMoved`, and item 14's `accepts`
    routes pointer events by hover — correctly, since only one pane may own the
    mouse — so a drag that left the rect got no more events and the selection
    froze at the last visible row. Selecting more than one screenful was
    therefore impossible: the user had to release, wheel, and drag again from
    scratch — losing what they had, since a fresh press starts a fresh
    `Selection` — or give up. iTerm2 and Ghostty both scroll while the button is
    held and keep extending into what comes into view, which is what every text
    editor does and what the gesture means.

    The fix cannot be an event handler, because the missing events are the
    whole problem. `drag_autoscroll` runs once per frame between `process_input`
    and `show` and **polls** `i.pointer` — `latest_pos()` and `primary_down()`,
    plain input state that a headless test can drive with synthetic
    `PointerMoved` plus a held `PointerButton`. It acts only when
    `state.is_dragged`, the latch that `process_left_button_pressed` sets on
    terra's own selection path and never on a reported press (item 13), so
    mouse-mode drags, the wheel and the scrollbar are untouched.

    Per frame, in order: a button that is no longer down clears the latch; a
    pointer past the top or bottom edge emits `BackendCommand::ScrollViewport`
    towards it; and a pointer outside the rect at all emits `SelectUpdate` at the
    nearest position *inside* it. The clamp is on both axes — `visual_cell_at`
    converts with `as usize`, so a negative x would wrap to an enormous column,
    and clamping x also gives the natural "drag out to the side to take the
    whole line". Nothing tracks the selection across the scroll because nothing
    has to: the anchors are absolute grid lines and `update_selection` re-reads
    `display_offset` every call, so scroll-then-update extends into the newly
    revealed rows for free. Over-scrolling is free too — `Grid::scroll_display`
    clamps at both ends of the history.

    Two details that are easy to get wrong:

    * **Pacing is wall-clock, and it ramps.** `autoscroll_lines_per_tick` is
      zero for the first half cell past the edge, then eases in (quadratically)
      over four cells to a ceiling of 5 lines per 1/60 s tick — 300 lines/s,
      the same cap xterm.js uses. The dead zone matters because the view is
      inset a few pixels inside its pane, so without it a pointer resting in
      that gutter already scrolls; the ramp matters because the commonest
      gesture is nudging a hair past the last row to catch one more line, and a
      flat rate flings several screens past it before the hand reacts. The
      frame's `stable_dt` (capped at 100 ms, so a stalled frame cannot fling the
      viewport) buys a fractional number of ticks whose remainder carries in
      `state.autoscroll_lines`, so a 120 Hz display scrolls at the same speed as
      a 60 Hz one. Because repaints here are reactive and a pointer held still
      outside the rect produces no events whatsoever, the poll also asks for a
      repaint in 16 ms while it is scrolling — without that the drag stalls
      after one frame. At either end of the history it stops asking, so a drag
      parked there goes idle instead of spinning the repaint loop; likewise the
      `SelectUpdate` is skipped when neither the viewport nor the pointer moved.
    * **The alternate screen is excluded from the scrolling half.** Under
      `ALTERNATE_SCROLL | ALT_SCREEN` the backend's `scroll` types arrow keys
      into the PTY instead of moving the viewport, so an autoscroll inside vim
      would walk the user's cursor — and their file — around while they were
      merely selecting text. The `SelectUpdate` half still runs there. The gate
      is checked twice, deliberately: the view checks `ALT_SCREEN` alone against
      the previous frame's snapshot (the alternate grid has no scrollback, so
      the wider test costs nothing), and because a program can enter the
      alternate screen *between* frames, the command it sends is the new
      `BackendCommand::ScrollViewport`, which re-checks the live mode and moves
      the viewport or does nothing — it can never fall through to the arrow
      keys.

    Two releases the event path never saw are cleaned up by the same poll.
    A selection drag released *outside* the rect never reached
    `process_left_button_released`, so `is_dragged` stayed set and a later
    pointer move, no button held, resumed extending the abandoned selection;
    and a *reported* drag released outside left `is_reported_press` set and,
    worse, left the program believing the button was still down, having been
    told about the press but never the release — so the poll sends that release
    report itself. Only those latches are cleared here: the double/triple-click
    re-select in `process_left_button_released` belongs to a click that ended
    inside the view and stays there. The poll also sits out while the view is
    unfocused (a modal is up), matching what `process_input` routes, and falls
    back to the last known position when `PointerGone` nulls `latest_pos` with
    the button still down.

    Guarded by `crates/terra-app/tests/drag_autoscroll.rs` and the
    `autoscroll_rate_tests` unit tests in view.rs.

17. backend/mod.rs `TerminalBackend::selectable_content`: **a copy is built by
    the terminal, not by walking the visible cells**. Upstream assembled the
    clipboard string itself — `for indexed in grid.display_iter()`, push
    `indexed.c` wherever the selection range contained the point — and that
    one loop got four things wrong at once. No newline was ever emitted, so a
    multi-row copy pasted as a single run-on line that only *looked* wrapped
    wherever the receiving app happened to break it. Every row arrived padded
    with the blanks the grid blanks it out to, so each line dragged a tail of
    spaces behind it. The second cell of a double-width character
    (`WIDE_CHAR_SPACER`) was copied as a character, putting a space after
    every CJK glyph. And `display_iter` walks the *viewport*: anything the
    selection reached in scrollback was dropped without a trace — which item
    16 turned from a corner case into the common one, since a drag past the
    edge now scrolls and a selection is routinely mostly off screen by the
    time the button comes up.

    All four are already solved upstream of us. `Term::selection_to_string`
    indexes the grid by absolute `Point`, so history is included; it trims
    each row's trailing blanks; it withholds the row's newline when the row
    ends in `WRAPLINE`, so a soft-wrapped line pastes back as the one line
    the user typed rather than as fragments a shell would run separately; it
    skips both spacer flags and picks up the zero-width marks a cell carries;
    it collapses the cells a tab covered; and it takes the rectangular path
    for `SelectionType::Block` (a `line_to_string` per row over the same
    column range, newline-joined) which we could not have written ourselves
    anyway, `line_to_string` being private. So the function is now one line
    that locks the term and asks it. Reading the live `Term` rather than
    `last_content` also means a copy describes the selection as it is, not as
    of the last `sync`; the lock is safe from every caller — view.rs's
    `Event::Copy` arm and the app both call it outside `process_command`
    /`sync`, which are the only places the term lock is held.

    Guarded by `crates/terra-app/tests/copy_selection.rs`.

18. view.rs `TerminalView::set_pointer_exclusion` + the guard in
    `process_input`: **a widget drawn on top of the terminal keeps its own
    clicks**. egui's hit test only occludes widgets *across* layers, so terra's
    overlay scrollbar — painted into the terminal's own layer, flush to its
    right edge — left `contains_pointer` true for both of them: a press on the
    thumb also started a text selection underneath it, and once item 16's
    drag-autoscroll existed, hauling the thumb up past the top edge set the
    poll issuing its own scroll deltas behind the scrollbar's back, so the
    viewport ran away from the thumb under a bogus selection. The caller now
    passes the screen-space strip its overlay owns, and `process_input` drops
    `egui::Event::PointerButton` inside it — `None` while the overlay is
    hidden, or the rightmost pixels of every pane would stop being selectable
    text. Only buttons are filtered: motion still goes through, so a selection
    begun elsewhere keeps extending as the pointer crosses the strip, and
    blocking the press alone is enough to leave `is_dragged` false and the
    autoscroll asleep. terra wires it from `scrollbar::hit_area` gated on
    `ScrollbarState::interactive`, which is necessarily the *previous* frame's
    answer — the thumb has to be added after the view to win the hit test — and
    that lag is harmless because a pointer merely resting over the strip
    already reveals a hidden thumb a frame before any button goes down.

    Guarded by `crates/terra-app/tests/scrollbar_selection.rs`.

19. backend/mod.rs `process_link_action` / `open_link` (+ the new
    `hyperlink_at`, `RenderableContent::hovered_hyperlink_uri`, and `LinkAction`
    re-exported from lib.rs): **OSC 8 hyperlinks are hoverable and openable**.
    Upstream resolved a hover with the URL regex alone, so the only links terra
    could open were the ones whose target was spelled out on screen. `ESC ] 8 ;
    ; URI ST` exists precisely so it need not be: `ls --hyperlink`, `gh`, and
    cargo's diagnostics print a word and hide the URL behind it, and every one
    of those was dead text.

    alacritty's parser already stores the link per cell (`Cell::hyperlink()`,
    either terminator), so the hover now asks the cell first and falls back to
    the regex only when the cell carries nothing. The hovered run is the
    maximal neighbourhood of the point whose cells carry the same link id,
    walked with `GridIterator`'s `prev`/`next` — which crosses row boundaries
    on its own, so a link that soft-wraps underlines as one thing, and clamps
    to the viewport the way alacritty's `hyperlink_at` does. Identity is the
    id, not the URI, which keeps two adjacent links distinct and one wrapped
    link whole. The spacer half of a double-width glyph is written from the
    same cursor template as the glyph, so it inherits the link and hovering
    either half resolves — no special case needed.

    The resolved URI is stored beside the range rather than folded into it, so
    `view.rs`'s underline test stays a plain `range.contains(point)` and the
    regex path keeps reading its URL off the screen. `open_link` opens the
    stored URI when there is one.

    Also here: `open::that` was `unwrap_or_else(|_| panic!(...))`. A URI with a
    scheme the OS has no handler for took the whole terminal down, losing every
    other tab's session; it now logs to stderr (the crate pulls in no logging
    facade) and carries on.

    Guarded by `crates/terra-app/tests/osc8_hyperlinks.rs`.

20. view.rs `paste_bytes` + lib.rs: **the paste path is public**, so the
    embedder can put text on the PTY *as a paste* instead of writing bytes
    raw. terra's drag-and-drop needs exactly that: a file dragged out of
    Finder onto the window is typed into the focused tab as shell-quoted
    paths (`terra-app/src/drop.rs`), and a drop is a paste — a shell that set
    DECSET 2004 has to see `ESC[200~` … `ESC[201~` round it, or it takes the
    path as typing and a name with a newline in it runs. Re-exporting the
    function item 11 already wrote is the whole change: the alternative was a
    second copy of the bracketing, the marker-stripping and the CR
    normalisation living in the app, drifting from this one.

    Guarded by `crates/terra-app/tests/drop_paths.rs`.

## The cursor beam under BiDi

The beam marks an *insertion point*, not a cell, so under reordering it has to
be placed on the side new text grows from. `bidi::beam_position` is the whole
decision, as a pure function of the row's map, the cursor's column, whether
that column holds a double-width character, and where the row's content ends;
`view.rs` only multiplies its answer by the cell width. Three cases:

1. **The cursor's own cell reads right-to-left.** Text grows leftwards, so the
   insertion point is the cell's visual **right** edge — the side the previous
   character is on. For a double-width character the edge belongs to the pair
   (`visual_span_start + 2`), never the seam down the middle of the glyph.
2. **The cursor is past the row's content and the row ends in a right-to-left
   run.** This is the case that fires on every keystroke while typing Hebrew or
   Arabic, and it cannot be decided from the cursor's own cell: that cell is
   blank padding, which rule L1 resets to the base level, so it claims to read
   left-to-right and the beam lands to the *right* of the word — the end it
   started from. The next character joins the trailing run, and a run grows
   from its visual left end, so the beam goes at the **leftmost visual column
   of that run**. Taking the minimum over the whole run, rather than reading
   `visual_of(cursor - 1)`, is what makes this right when L2 has reordered
   something inside the run and what keeps the answer stable when the user
   typed a space and the cursor is no longer adjacent to the text.
3. **Anything else** — the cell's visual **left** edge, which is the ordinary
   left-to-right beam. With BiDi off the map is `None`, every row is this case,
   and the answer is the plain logical column: the default configuration is
   bit-for-bit what it was before any of this existed.

Two fixes came out of auditing that path:

* Deciding case 2 on "the character before the cursor is right-to-left" alone
  also fired for a cursor sitting on a blank *inside* the row, or on a Latin
  letter wedged between two Hebrew ones, and dragged the beam back to the run's
  left edge — the reported "cursor in the middle of the Hebrew". The
  `content_end` argument is what distinguishes a blank inside the line, which
  is a real cell with a real position, from padding past the end of it.
* The beam used to be painted from inside the cell loop, when an iterated
  cell's point happened to equal the cursor's. A cursor parked on the spacer
  column of a double-width character then got **no beam at all**, because the
  loop skips spacers outright. It is now painted once after the loop, from the
  cursor point itself, with a spacer resolved back to the character that owns
  the pair; the row is drawn only when it is inside the viewport, which is what
  the old coupling gave for free. Painting last also stops a later cell's
  background rect from covering a beam that the reordering put in an earlier
  visual column.

Known limitation: a row that *ends* in digits or Latin inside a right-to-left
paragraph (`שלום 123`) puts the beam at the cursor's own column, to the right
of the Hebrew, rather than after the digits where the next character will
appear. Case 2 only recognises a trailing right-to-left run.
