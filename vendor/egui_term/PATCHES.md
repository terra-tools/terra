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
