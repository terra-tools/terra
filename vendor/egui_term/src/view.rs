use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Column;
use alacritty_terminal::index::Point as TerminalGridPoint;
use alacritty_terminal::term::cell;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use egui::epaint::RectShape;
use egui::Modifiers;
use egui::MouseWheelUnit;
use egui::Shape;
use egui::Widget;
use egui::{Align2, Painter, Pos2, Rect, Response, Stroke, Vec2};
use egui::{CornerRadius, Key};
use egui::{Id, PointerButton};

use crate::backend::BackendCommand;
use crate::backend::TerminalBackend;
use crate::emoji;
use crate::backend::{LinkAction, MouseButton, SelectionType};
use crate::bidi::{self, BidiBase};
use crate::bindings::Binding;
use crate::bindings::{BindingAction, BindingsLayout, InputKind};
use crate::font::TerminalFont;
use crate::theme::TerminalTheme;
use crate::types::Size;

const EGUI_TERM_WIDGET_ID_PREFIX: &str = "egui_term::instance::";
/// terra patch: width (px) of the vertical cursor beam.
const CURSOR_BEAM_WIDTH: f32 = 1.5;
/// Cursor blink cadence: visible for one interval, hidden for the next.
const CURSOR_BLINK_INTERVAL: f64 = 0.55;

#[derive(Debug, Clone)]
enum InputAction {
    BackendCall(BackendCommand),
    WriteToClipboard(String),
    Ignore,
}

#[derive(Clone, Default)]
pub struct TerminalViewState {
    is_dragged: bool,
    /// terra patch: a press that went to the program as a mouse report, so
    /// its release must go there too. See `process_left_button`.
    is_reported_press: bool,
    scroll_pixels: f32,
    current_mouse_position_on_grid: TerminalGridPoint,
}

pub struct TerminalView<'a> {
    widget_id: Id,
    has_focus: bool,
    size: Vec2,
    backend: &'a mut TerminalBackend,
    font: TerminalFont,
    theme: TerminalTheme,
    bindings_layout: BindingsLayout,
    /// terra patch: whether to reorder right-to-left text for display.
    bidi: bool,
    /// terra patch: the paragraph direction rows resolve against.
    bidi_base: BidiBase,
}

impl Widget for TerminalView<'_> {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        let (layout, painter) =
            ui.allocate_painter(self.size, egui::Sense::click());

        let widget_id = self.widget_id;
        let mut state = ui.memory(|m| {
            m.data
                .get_temp::<TerminalViewState>(widget_id)
                .unwrap_or_default()
        });

        self.focus(&layout)
            .resize(&layout)
            .process_input(&layout, &mut state)
            .show(&mut state, &layout, &painter);

        ui.memory_mut(|m| m.data.insert_temp(widget_id, state));
        layout
    }
}

impl<'a> TerminalView<'a> {
    pub fn new(ui: &mut egui::Ui, backend: &'a mut TerminalBackend) -> Self {
        let widget_id = ui.make_persistent_id(format!(
            "{}{}",
            EGUI_TERM_WIDGET_ID_PREFIX,
            backend.id()
        ));

        Self {
            widget_id,
            has_focus: false,
            size: ui.available_size(),
            backend,
            font: TerminalFont::default(),
            theme: TerminalTheme::default(),
            bindings_layout: BindingsLayout::new(),
            bidi: true,
            bidi_base: BidiBase::default(),
        }
    }

    #[inline]
    pub fn set_theme(mut self, theme: TerminalTheme) -> Self {
        self.theme = theme;
        self
    }

    #[inline]
    pub fn set_font(mut self, font: TerminalFont) -> Self {
        self.font = font;
        self
    }

    #[inline]
    pub fn set_focus(mut self, has_focus: bool) -> Self {
        self.has_focus = has_focus;
        self
    }

    /// terra patch: enable UAX #9 BiDi reordering for this frame.
    ///
    /// Applied to the backend before `sync`, so a toggle takes effect on the
    /// very next paint — both for what is drawn and for where clicks land.
    #[inline]
    pub fn set_bidi(mut self, bidi: bool) -> Self {
        self.bidi = bidi;
        self
    }

    /// terra patch: the paragraph direction BiDi resolves each row against.
    #[inline]
    pub fn set_bidi_base(mut self, base: BidiBase) -> Self {
        self.bidi_base = base;
        self
    }

    #[inline]
    pub fn set_size(mut self, size: Vec2) -> Self {
        self.size = size;
        self
    }

    #[inline]
    pub fn add_bindings(
        mut self,
        bindings: Vec<(Binding<InputKind>, BindingAction)>,
    ) -> Self {
        self.bindings_layout.add_bindings(bindings);
        self
    }

    fn focus(self, layout: &Response) -> Self {
        if self.has_focus {
            layout.request_focus();
        } else {
            layout.surrender_focus();
        }

        self
    }

    fn resize(self, layout: &Response) -> Self {
        self.backend.process_command(BackendCommand::Resize(
            Size::from(layout.rect.size()),
            self.font.font_measure(&layout.ctx),
        ));

        self
    }

    fn process_input(
        self,
        layout: &Response,
        state: &mut TerminalViewState,
    ) -> Self {
        // terra patch: the pointer decides where a *mouse* event lands, never
        // where a keystroke does. Upstream gated the whole function on
        // `contains_pointer`, so the focused terminal dropped everything typed
        // while the pointer sat anywhere else — over the tab bar right after
        // clicking a tab, or outside the window entirely. See `accepts`.
        let hovered = layout.contains_pointer();
        let focused = layout.has_focus();
        // Nothing to route: this view neither holds the keyboard nor sits
        // under the pointer.
        if !focused && !hovered {
            return self;
        }

        let modifiers = layout.ctx.input(|i| i.modifiers);
        let events = layout.ctx.input(|i| i.events.clone());
        for event in events {
            if !accepts(&event, hovered, focused) {
                continue;
            }
            let mut input_actions = vec![];

            match event {
                egui::Event::Text(_)
                | egui::Event::Key { .. }
                | egui::Event::Copy
                | egui::Event::Paste(_) => {
                    input_actions.push(process_keyboard_event(
                        event,
                        self.backend,
                        &self.bindings_layout,
                        modifiers,
                    ))
                }
                egui::Event::MouseWheel { unit, delta, .. } => {
                    // terra patch (hover scroll): an unfocused view never
                    // takes `PointerMoved`, so its cached grid position is
                    // whatever the pointer last did *while it was focused* —
                    // usually cell (0, 0). A wheel report has to name the cell
                    // the pointer is over now, so recompute it here.
                    if !focused {
                        if let Some(pos) = layout.ctx.pointer_latest_pos() {
                            track_grid_position(
                                state,
                                layout,
                                self.backend,
                                pos,
                            );
                        }
                    }
                    input_actions = process_mouse_wheel(
                        state,
                        self.backend,
                        self.font.font_type().size,
                        unit,
                        delta,
                        &modifiers,
                    )
                }
                egui::Event::PointerButton {
                    button,
                    pressed,
                    modifiers,
                    pos,
                    ..
                } => input_actions.push(process_button_click(
                    state,
                    layout,
                    self.backend,
                    &self.bindings_layout,
                    button,
                    pos,
                    &modifiers,
                    pressed,
                )),
                egui::Event::PointerMoved(pos) => {
                    input_actions = process_mouse_move(
                        state,
                        layout,
                        self.backend,
                        pos,
                        &modifiers,
                    )
                }
                _ => {}
            };

            for action in input_actions {
                match action {
                    InputAction::BackendCall(cmd) => {
                        self.backend.process_command(cmd);
                    }
                    InputAction::WriteToClipboard(data) => {
                        layout.ctx.copy_text(data);
                    }
                    InputAction::Ignore => {}
                }
            }
        }

        self
    }

    fn show(
        self,
        state: &mut TerminalViewState,
        layout: &Response,
        painter: &Painter,
    ) {
        // terra patch (BiDi): settle the mode before the snapshot, so the
        // maps in `content` describe exactly what this frame paints.
        // terra patch: the program learns about focus only if we tell it.
        // Both conditions matter — the window can be focused while the
        // palette has taken the keyboard.
        let window_focused = layout.ctx.input(|i| i.viewport().focused.unwrap_or(true));
        self.backend.set_focused(self.has_focus && window_focused);
        // terra patch: hand the backend the palette to answer colour queries
        // from. Once per tab — the table is 259 entries and the theme does
        // not change under us.
        // Normally already filled in when the tab was opened; this is the
        // backstop for a backend created without a theme to hand.
        if self.backend.wants_colors() {
            self.backend.set_reported_colors(self.theme.reported_colors());
        }
        self.backend.set_bidi(self.bidi);
        self.backend.set_bidi_base(self.bidi_base);
        let content = self.backend.sync();
        let layout_min = layout.rect.min;
        let layout_max = layout.rect.max;
        let cell_height = content.terminal_size.cell_height as f32;
        let cell_width = content.terminal_size.cell_width as f32;
        let global_bg =
            self.theme.get_color(Color::Named(NamedColor::Background));

        let mut shapes = vec![Shape::Rect(RectShape::filled(
            Rect::from_min_max(layout_min, layout_max),
            CornerRadius::ZERO,
            global_bg,
        ))];

        for indexed in content.grid.display_iter() {
            let flags = indexed.cell.flags;
            let is_wide_char_spacer =
                flags.contains(cell::Flags::WIDE_CHAR_SPACER);
            if is_wide_char_spacer {
                continue;
            }

            let is_wide_char = flags.contains(cell::Flags::WIDE_CHAR);
            let is_inverse = flags.contains(cell::Flags::INVERSE);
            // terra patch: DIM only. Upstream used
            // `intersects(DIM | DIM_BOLD)`, and DIM_BOLD == DIM|BOLD, so
            // plain BOLD cells were drawn at 70% alpha (dim + transparent).
            let is_dim = flags.contains(cell::Flags::DIM);
            let is_selected = content
                .selectable_range
                .is_some_and(|r| r.contains(indexed.point));
            let is_hovered_hyperling =
                content.hovered_hyperlink.as_ref().is_some_and(|r| {
                    r.contains(&indexed.point)
                        && r.contains(&state.current_mouse_position_on_grid)
                });

            let line_num =
                indexed.point.line.0 + content.grid.display_offset() as i32;
            let y = layout_min.y + (cell_height * line_num as f32);

            // terra patch (BiDi): the cell knows its *logical* column; the
            // row's map says where that lands visually. This is the only
            // place a column becomes a pixel, so it is the only place the
            // reordering has to be applied. `content.bidi` is empty when
            // BiDi is off, and `RowMap` is the identity for any row without
            // right-to-left text, so the common case returns what it was
            // given.
            let logical_col = indexed.point.column.0;
            let row_map = content.bidi.get(line_num.max(0) as usize);
            let visual_col = match row_map {
                // A wide char spans two columns; anchor the pair at its
                // leftmost visual column so the double-width glyph never
                // overhangs the run it belongs to.
                Some(map) if is_wide_char => {
                    map.visual_span_start(logical_col, 2)
                }
                Some(map) => map.visual_of(logical_col),
                None => logical_col,
            };
            let x = layout_min.x + (cell_width * visual_col as f32);

            let mut fg = self.theme.get_color(indexed.fg);
            let mut bg = self.theme.get_color(indexed.bg);
            let cell_width = if is_wide_char {
                cell_width * 2.0
            } else {
                cell_width
            };

            if is_dim {
                fg = fg.linear_multiply(0.7);
            }

            if is_inverse {
                std::mem::swap(&mut fg, &mut bg);
            }

            // terra patch: selection is an opaque overlay (Ghostty-style)
            // rather than an fg/bg swap (alacritty-style INVERSE). Applied
            // after the INVERSE swap so selected INVERSE cells keep their
            // swapped foreground unless the theme overrides it.
            if is_selected {
                bg = self.theme.selection_background();
                if let Some(selection_fg) = self.theme.selection_foreground() {
                    fg = selection_fg;
                }
            }

            // A selected cell always paints its background, even when it
            // matches the global background.
            if is_selected || global_bg != bg {
                shapes.push(Shape::Rect(RectShape::filled(
                    Rect::from_min_size(
                        Pos2::new(x, y),
                        // + 1.0 is to fill grid border
                        Vec2::new(cell_width + 1., cell_height + 1.),
                    ),
                    CornerRadius::ZERO,
                    bg,
                )));
            }

            // Handle hovered hyperlink underline
            if is_hovered_hyperling {
                let underline_height = y + cell_height;
                shapes.push(Shape::LineSegment {
                    points: [
                        Pos2::new(x, underline_height),
                        Pos2::new(x + cell_width, underline_height),
                    ],
                    stroke: Stroke::new(cell_height * 0.15, fg),
                });
            }

            // Draw text content
            if indexed.c != ' ' && indexed.c != '\t' {
                // terra patch (BiDi): rule L4 mirrors brackets whose own
                // resolved level is odd. Purely a paint-time substitution —
                // the cell, the clipboard and any URL match keep the
                // original character.
                let glyph = row_map.map_or(indexed.c, |map| {
                    map.display_char(logical_col, indexed.c)
                });
                // terra patch: U+23FA ⏺ exists in emoji faces only, drawn as
                // a record *button* (square around a dot). TUIs use it as a
                // status bullet and tint it via ANSI, so draw the plain
                // geometric circle the text cascade carries instead.
                // Paint-time only: cell, clipboard and capture keep U+23FA.
                let glyph = if glyph == '\u{23FA}' {
                    '\u{25CF}'
                } else {
                    glyph
                };
                // terra patch (#19): emoji paint as colour bitmaps from the
                // system emoji font, composited as textured quads — epaint's
                // glyph path is outlines-only and cannot carry colour. A
                // character the font has no art for falls through to text.
                let vs16 = indexed
                    .zerowidth()
                    .is_some_and(|z| z.contains(&'\u{FE0F}'));
                if emoji::wants_color(glyph, vs16) {
                    let side = cell_width.min(cell_height);
                    let px = (side
                        * layout.ctx.pixels_per_point())
                    .round() as u32;
                    if let Some(tex) =
                        emoji::texture(&layout.ctx, glyph, px)
                    {
                        shapes.push(Shape::image(
                            tex.id(),
                            Rect::from_center_size(
                                Pos2::new(
                                    x + cell_width / 2.0,
                                    y + cell_height / 2.0,
                                ),
                                Vec2::splat(side),
                            ),
                            Rect::from_min_max(
                                Pos2::ZERO,
                                Pos2::new(1.0, 1.0),
                            ),
                            egui::Color32::WHITE,
                        ));
                        continue;
                    }
                }
                shapes.push(painter.fonts_mut(|c| {
                    // terra patch: center the glyph vertically inside the
                    // (possibly line-height-inflated) cell.
                    let glyph_h = c.row_height(&self.font.font_type());
                    let pad_y = ((cell_height - glyph_h) / 2.0).max(0.0);
                    Shape::text(
                        c,
                        Pos2 {
                            x: x + (cell_width / 2.0),
                            y: y + pad_y,
                        },
                        Align2::CENTER_TOP,
                        glyph,
                        self.font.font_type(),
                        fg,
                    )
                }));
            }
        }

        // terra patch: a thin vertical beam at the insertion point in the
        // theme's cursor color, instead of upstream's full-cell filled block.
        // The glyph keeps its own color (no fg/bg swap), so the character
        // under the cursor stays readable.
        //
        // terra patch (BiDi): painted once, after the grid, rather than from
        // inside the cell loop. Keying it off "some iterated cell's point
        // equals the cursor point" dropped the beam altogether whenever that
        // cell was never iterated — reachably so when the cursor is parked on
        // the spacer column of a double-width character, which the loop skips
        // outright. Painting it last also stops a later cell's background
        // rect from covering a beam that the reordering put in an earlier
        // visual column.
        let cursor_point = content.grid.cursor.point;
        let cursor_row =
            cursor_point.line.0 + content.grid.display_offset() as i32;
        let columns = content.grid.columns();
        // The cursor lives in the active area, so scrolling back far enough
        // takes it off screen; then there is nothing to draw.
        if cursor_row >= 0
            && (cursor_row as usize) < content.grid.screen_lines()
            && columns > 0
        {
            // Blink: visible one interval, hidden the next; keep repainting
            // so the phase advances. An unfocused terminal shows a steady
            // beam instead of blinking.
            let time = layout.ctx.input(|i| i.time);
            let visible = !layout.has_focus()
                || (time / CURSOR_BLINK_INTERVAL) as u64 % 2 == 0;
            layout.ctx.request_repaint_after(
                std::time::Duration::from_secs_f64(
                    CURSOR_BLINK_INTERVAL - (time % CURSOR_BLINK_INTERVAL),
                ),
            );
            if visible {
                let row = &content.grid[cursor_point.line];
                // A cursor parked on a spacer belongs to the double-width
                // character that owns the pair, one column to its left.
                let mut col = cursor_point.column.0.min(columns - 1);
                if col > 0
                    && row[Column(col)]
                        .flags
                        .contains(cell::Flags::WIDE_CHAR_SPACER)
                {
                    col -= 1;
                }
                let is_wide =
                    row[Column(col)].flags.contains(cell::Flags::WIDE_CHAR);
                // One past the row's last occupied column, counting a wide
                // char's spacer as occupied — the same content length the
                // BiDi pass measured for this row.
                let content_end = (0..columns)
                    .rposition(|column| {
                        let grid_cell = &row[Column(column)];
                        grid_cell.c != ' '
                            || grid_cell
                                .flags
                                .contains(cell::Flags::WIDE_CHAR_SPACER)
                    })
                    .map_or(0, |last| last + 1);
                let beam = bidi::beam_position(
                    content.bidi.get(cursor_row as usize),
                    col,
                    is_wide,
                    content_end,
                );
                let mut beam_x = layout_min.x + cell_width * beam.offset;
                if beam.side == bidi::BeamSide::Right {
                    // Pull the beam back inside the cell whose right edge it
                    // marks, so it never bleeds into the next glyph.
                    beam_x -= CURSOR_BEAM_WIDTH;
                }
                shapes.push(Shape::Rect(RectShape::filled(
                    Rect::from_min_size(
                        Pos2::new(
                            beam_x,
                            layout_min.y + cell_height * cursor_row as f32,
                        ),
                        Vec2::new(CURSOR_BEAM_WIDTH, cell_height),
                    ),
                    CornerRadius::default(),
                    self.theme.cursor_color(),
                )));
            }
        }

        painter.extend(shapes);
    }
}

/// terra patch: whether this terminal acts on `event` this frame.
///
/// The input kinds are addressed differently, and conflating them is what made
/// selecting a tab need a second click: a keystroke belongs to whatever holds
/// keyboard focus, wherever the pointer happens to be resting, while a mouse
/// event belongs to whatever is under the pointer. Upstream required
/// `contains_pointer` for both, so everything typed with the pointer parked
/// over the tab bar — exactly where it lands after clicking a tab — went
/// nowhere.
///
/// The **wheel** is the one event that needs neither: it is routed by hover
/// alone, so with several panes on screen the one under the cursor scrolls
/// without being clicked first (iTerm2 / Ghostty behaviour). Keyboard focus
/// does not move — typing, paste and IME keep going where they went. Clicks
/// and pointer motion still need both, so click-to-focus and selection are
/// exactly what they were: a drag starts only in the pane that already holds
/// focus.
fn accepts(event: &egui::Event, hovered: bool, focused: bool) -> bool {
    match event {
        egui::Event::Text(_)
        | egui::Event::Key { .. }
        | egui::Event::Copy
        | egui::Event::Paste(_) => focused,
        egui::Event::MouseWheel { .. } => hovered,
        egui::Event::PointerButton { .. } | egui::Event::PointerMoved(_) => {
            hovered && focused
        }
        // Everything else is ignored by the match below anyway.
        _ => false,
    }
}

/// terra patch: whether this `Copy`/`Paste` event is really the terminal's
/// own `^C` / `^V` wearing a clipboard costume.
///
/// `egui_winit` synthesises `Event::Copy`/`Event::Paste` from
/// `modifiers.command + C/V` and *swallows* the `Event::Key`, and on every
/// platform but macOS `command` is Ctrl. Handing those to the clipboard would
/// cost a Linux or Windows user Ctrl+C — the interrupt every terminal is
/// expected to deliver — and the literal `^V` that `bind -v`, `showkey` and
/// quoted-insert want. So the bare Ctrl spelling stays a passthrough, which is
/// also the convention gnome-terminal, konsole and xterm follow: **Ctrl+Shift+C
/// / Ctrl+Shift+V** are the clipboard, plain Ctrl+C / Ctrl+V are bytes.
///
/// What *is* fixed here is that the old test asked for `COMMAND | SHIFT` and
/// sent `^V` for everything else, so Windows' Shift+Insert and Ctrl+Insert and
/// the dedicated `Key::Copy`/`Key::Paste` media keys — none of which carry the
/// command modifier — pasted a `^V` instead of the clipboard. Only the exact
/// "command held, shift not" spelling is a passthrough now.
///
/// On macOS the clipboard modifier is ⌘ and Ctrl+C/Ctrl+V never come through
/// here at all, so a paste is always a paste.
fn clipboard_key_is_passthrough(modifiers: Modifiers) -> bool {
    if cfg!(any(target_os = "ios", target_os = "macos")) {
        false
    } else {
        modifiers.command && !modifiers.shift
    }
}

/// terra patch: the bytes a paste puts on the PTY.
///
/// Two things upstream did not do, and the reason "paste is broken in terra"
/// was a real report:
///
/// * **Bracketed paste (DECSET 2004).** A program that set the mode asked to be
///   told where a paste begins and ends, so it can take the text as data rather
///   than as typing. Without the `ESC[200~` … `ESC[201~` wrapper a shell runs
///   every pasted line, and codex, claude and syntax-highlighting shells — all
///   of which gate on the marker — see a burst of keystrokes.
/// * **Line endings.** A paste is not a key sequence; the byte the Enter key
///   produces is CR, so `\r\n` and a lone `\n` both become `\r`. Alacritty does
///   this for unbracketed pastes and terra does it in both modes, as iTerm2 and
///   xterm do: a payload full of LFs inside the brackets makes readline's
///   bracketed-paste handler and tmux's buffer disagree about line count.
///
/// The payload is sanitised the way xterm sanitises it: the two markers cannot
/// appear *inside* the brackets, or a crafted paste (a snippet off a web page)
/// could close the bracket early and have its remainder executed as typing.
/// Stripping runs to a fixpoint because one removal can splice a fresh marker
/// out of the halves either side of it (`ESC[20` + `ESC[201~` + `1~`).
///
/// Alacritty instead deletes every `\x1b` and `\x03` from a bracketed payload.
/// That is blunter — it silently eats the SGR colours in a snippet copied out
/// of another terminal — and the narrower rule closes the same hole.
fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if !bracketed {
        return text.into_bytes();
    }
    let mut text = text;
    while text.contains(PASTE_START) || text.contains(PASTE_END) {
        text = text.replace(PASTE_START, "").replace(PASTE_END, "");
    }
    let mut out =
        Vec::with_capacity(text.len() + PASTE_START.len() + PASTE_END.len());
    out.extend_from_slice(PASTE_START.as_bytes());
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(PASTE_END.as_bytes());
    out
}

/// terra patch: the bracketed-paste delimiters (DECSET 2004).
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

fn process_keyboard_event(
    event: egui::Event,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    modifiers: Modifiers,
) -> InputAction {
    match event {
        egui::Event::Text(text) => {
            process_text_event(&text, modifiers, backend, bindings_layout)
        }
        egui::Event::Paste(text) => {
            // terra patch: a bare Ctrl+V is the terminal's literal ^V on the
            // platforms where Ctrl *is* the command modifier — see
            // `clipboard_key_is_passthrough`.
            if clipboard_key_is_passthrough(modifiers) {
                InputAction::BackendCall(BackendCommand::Write(vec![0x16]))
            } else {
                let bracketed = backend
                    .last_content()
                    .terminal_mode
                    .contains(TermMode::BRACKETED_PASTE);
                InputAction::BackendCall(BackendCommand::Write(paste_bytes(
                    &text, bracketed,
                )))
            }
        }
        egui::Event::Copy => {
            // terra patch: the same rule, so Ctrl+C still interrupts.
            if clipboard_key_is_passthrough(modifiers) {
                InputAction::BackendCall(BackendCommand::Write(vec![0x03]))
            } else {
                InputAction::WriteToClipboard(backend.selectable_content())
            }
        }
        egui::Event::Key {
            key,
            pressed,
            modifiers,
            ..
        } => process_keyboard_key(
            backend,
            bindings_layout,
            key,
            modifiers,
            pressed,
        ),
        _ => InputAction::Ignore,
    }
}

fn process_text_event(
    text: &str,
    modifiers: Modifiers,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
) -> InputAction {
    if let Some(key) = Key::from_name(text) {
        if bindings_layout.get_action(
            InputKind::KeyCode(key),
            modifiers,
            backend.last_content().terminal_mode,
        ) == BindingAction::Ignore
        {
            InputAction::BackendCall(BackendCommand::Write(
                text.as_bytes().to_vec(),
            ))
        } else {
            InputAction::Ignore
        }
    } else {
        InputAction::BackendCall(BackendCommand::Write(
            text.as_bytes().to_vec(),
        ))
    }
}

fn process_keyboard_key(
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    key: Key,
    modifiers: Modifiers,
    pressed: bool,
) -> InputAction {
    if !pressed {
        return InputAction::Ignore;
    }

    let terminal_mode = backend.last_content().terminal_mode;
    let binding_action = bindings_layout.get_action(
        InputKind::KeyCode(key),
        modifiers,
        terminal_mode,
    );

    match binding_action {
        BindingAction::Char(c) => {
            let mut buf = [0, 0, 0, 0];
            let str = c.encode_utf8(&mut buf);
            InputAction::BackendCall(BackendCommand::Write(
                str.as_bytes().to_vec(),
            ))
        }
        BindingAction::Esc(seq) => InputAction::BackendCall(
            BackendCommand::Write(seq.as_bytes().to_vec()),
        ),
        _ => InputAction::Ignore,
    }
}

fn process_mouse_wheel(
    state: &mut TerminalViewState,
    backend: &TerminalBackend,
    font_size: f32,
    unit: MouseWheelUnit,
    delta: Vec2,
    modifiers: &Modifiers,
) -> Vec<InputAction> {
    // Positive = up, matching `BackendCommand::Scroll`.
    let lines: i32 = match unit {
        MouseWheelUnit::Line => {
            (delta.y.signum() * delta.y.abs().ceil()) as i32
        }
        MouseWheelUnit::Point => {
            state.scroll_pixels -= delta.y;
            let lines = (state.scroll_pixels / font_size).trunc();
            state.scroll_pixels %= font_size;
            -lines as i32
        }
        MouseWheelUnit::Page => 0,
    };
    if lines == 0 {
        return Vec::new();
    }

    // terra patch (#21): a program that turned on mouse tracking gets the
    // wheel as mouse reports, one per line, at the pointer's cell — it asked
    // for the mouse, and alternate-scroll arrows would reach it as stray
    // keystrokes. Shift bypasses reporting, as in every terminal.
    let terminal_mode = backend.last_content().terminal_mode;
    if terminal_mode.intersects(TermMode::MOUSE_MODE) && !modifiers.shift {
        let button = if lines > 0 {
            MouseButton::ScrollUp
        } else {
            MouseButton::ScrollDown
        };
        return (0..lines.abs())
            .map(|_| {
                InputAction::BackendCall(BackendCommand::MouseReport(
                    button.clone(),
                    *modifiers,
                    state.current_mouse_position_on_grid,
                    true,
                ))
            })
            .collect();
    }

    vec![InputAction::BackendCall(BackendCommand::Scroll(lines))]
}

fn process_button_click(
    state: &mut TerminalViewState,
    layout: &Response,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    button: PointerButton,
    position: Pos2,
    modifiers: &Modifiers,
    pressed: bool,
) -> InputAction {
    match button {
        PointerButton::Primary => process_left_button(
            state,
            layout,
            backend,
            bindings_layout,
            position,
            modifiers,
            pressed,
        ),
        _ => InputAction::Ignore,
    }
}

fn process_left_button(
    state: &mut TerminalViewState,
    layout: &Response,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    position: Pos2,
    modifiers: &Modifiers,
    pressed: bool,
) -> InputAction {
    // terra patch: who owns a click is decided **once, at press time**, and
    // the release follows the press. Deciding it again on release reads the
    // modifiers as they are then, which need not be how they were: let go of
    // Shift before the mouse button and the program that never saw the press
    // gets an orphan release (it believes the button is still down), while
    // the selection started by that press is never finished — `is_dragged`
    // stays set and the next pointer move keeps extending it.
    if pressed {
        let terminal_mode = backend.last_content().terminal_mode;
        state.is_reported_press = terminal_mode
            .intersects(TermMode::MOUSE_MODE)
            && !selection_override(modifiers);
        if state.is_reported_press {
            return InputAction::BackendCall(BackendCommand::MouseReport(
                MouseButton::LeftButton,
                *modifiers,
                state.current_mouse_position_on_grid,
                true,
            ));
        }
        process_left_button_pressed(state, layout, position)
    } else if state.is_reported_press {
        state.is_reported_press = false;
        InputAction::BackendCall(BackendCommand::MouseReport(
            MouseButton::LeftButton,
            *modifiers,
            state.current_mouse_position_on_grid,
            false,
        ))
    } else {
        process_left_button_released(
            state,
            layout,
            backend,
            bindings_layout,
            position,
            modifiers,
        )
    }
}

/// terra patch: whether a mouse event should drive terra's own selection
/// rather than being reported to the program.
///
/// A program that turns mouse reporting on — Claude Code, vim, tmux —
/// swallows every click, so without an escape hatch the user can never
/// select or copy anything on screen. Every terminal provides one: Shift is
/// xterm's convention and Option is the macOS one, so honour both.
///
/// Deliberately not "any modifier": Ctrl is a modifier programs legitimately
/// want reported with the click, and Cmd already means follow-the-link here.
fn selection_override(modifiers: &Modifiers) -> bool {
    modifiers.shift || modifiers.alt
}

fn process_left_button_pressed(
    state: &mut TerminalViewState,
    layout: &Response,
    position: Pos2,
) -> InputAction {
    state.is_dragged = true;
    InputAction::BackendCall(build_start_select_command(layout, position))
}

fn process_left_button_released(
    state: &mut TerminalViewState,
    layout: &Response,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    position: Pos2,
    modifiers: &Modifiers,
) -> InputAction {
    state.is_dragged = false;
    if layout.double_clicked() || layout.triple_clicked() {
        InputAction::BackendCall(build_start_select_command(layout, position))
    } else {
        let terminal_content = backend.last_content();
        let binding_action = bindings_layout.get_action(
            InputKind::Mouse(PointerButton::Primary),
            *modifiers,
            terminal_content.terminal_mode,
        );

        if binding_action == BindingAction::LinkOpen {
            InputAction::BackendCall(BackendCommand::ProcessLink(
                LinkAction::Open,
                state.current_mouse_position_on_grid,
            ))
        } else {
            InputAction::Ignore
        }
    }
}

fn build_start_select_command(
    layout: &Response,
    cursor_position: Pos2,
) -> BackendCommand {
    let selection_type = if layout.double_clicked() {
        SelectionType::Semantic
    } else if layout.triple_clicked() {
        SelectionType::Lines
    } else {
        SelectionType::Simple
    };

    BackendCommand::SelectStart(
        selection_type,
        cursor_position.x - layout.rect.min.x,
        cursor_position.y - layout.rect.min.y,
    )
}

/// terra patch: remember which cell the pointer is over, and hand back the
/// pixel offset inside the view that says so.
///
/// Split out of `process_mouse_move` because the wheel needs the same answer
/// on a view that gets no pointer events at all — an unfocused pane the
/// cursor is merely hovering (see `accepts`).
fn track_grid_position(
    state: &mut TerminalViewState,
    layout: &Response,
    backend: &TerminalBackend,
    position: Pos2,
) -> (f32, f32) {
    let cursor_x = position.x - layout.rect.min.x;
    let cursor_y = position.y - layout.rect.min.y;
    state.current_mouse_position_on_grid = backend.selection_point(
        cursor_x,
        cursor_y,
        backend.last_content().grid.display_offset(),
    );
    (cursor_x, cursor_y)
}

fn process_mouse_move(
    state: &mut TerminalViewState,
    layout: &Response,
    backend: &TerminalBackend,
    position: Pos2,
    modifiers: &Modifiers,
) -> Vec<InputAction> {
    let (cursor_x, cursor_y) =
        track_grid_position(state, layout, backend, position);
    let terminal_content = backend.last_content();

    let mut actions = vec![];
    // terra patch: the drag belongs to whoever the press gave it to.
    // `is_dragged` is set only by a press that took the selection path, so a
    // Shift-drag keeps extending the selection even if Shift is released
    // mid-drag; a reported press keeps reporting motion even if Shift is
    // pressed mid-drag, so the program's press/motion/release stay coherent.
    if state.is_dragged {
        actions.push(InputAction::BackendCall(BackendCommand::SelectUpdate(
            cursor_x, cursor_y,
        )));
    } else if state.is_reported_press
        // terra patch: motion during a held reported press belongs to the
        // program under *button-event* tracking (DECSET 1002, what tmux sets)
        // as well as any-motion tracking (1003). Upstream only checked 1003,
        // so a drag reached tmux as press…silence…release — two clicks, never
        // a drag, and tmux's mouse selection could not start.
        && terminal_content
            .terminal_mode
            .intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
    {
        actions.push(InputAction::BackendCall(BackendCommand::MouseReport(
            MouseButton::LeftMove,
            *modifiers,
            state.current_mouse_position_on_grid,
            true,
        )));
    }

    // Handle link hover if applicable
    if modifiers.command_only() {
        actions.push(InputAction::BackendCall(BackendCommand::ProcessLink(
            LinkAction::Hover,
            state.current_mouse_position_on_grid,
        )));
    }

    actions
}

#[cfg(test)]
mod accepts_tests {
    use super::accepts;
    use egui::{Modifiers, Pos2};

    fn key() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    fn click() -> egui::Event {
        egui::Event::PointerButton {
            pos: Pos2::ZERO,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }
    }

    fn wheel() -> egui::Event {
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::Vec2::ZERO,
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        }
    }

    /// The one that matters: a focused terminal is typed into no matter where
    /// the pointer is resting — over the tab bar just after a tab was picked,
    /// or off the window entirely.
    #[test]
    fn keystrokes_reach_a_focused_terminal_wherever_the_pointer_is() {
        for hovered in [true, false] {
            assert!(accepts(&key(), hovered, true));
            assert!(accepts(&egui::Event::Text("a".into()), hovered, true));
            assert!(accepts(&egui::Event::Copy, hovered, true));
            assert!(accepts(&egui::Event::Paste("a".into()), hovered, true));
        }
    }

    /// …and nowhere else. Hovering an unfocused pane must not make it a
    /// second keyboard sink: typing belongs to focus alone.
    #[test]
    fn keystrokes_never_reach_an_unfocused_terminal() {
        for hovered in [true, false] {
            assert!(!accepts(&key(), hovered, false));
            assert!(!accepts(&egui::Event::Text("a".into()), hovered, false));
            assert!(!accepts(&egui::Event::Copy, hovered, false));
            assert!(!accepts(&egui::Event::Paste("a".into()), hovered, false));
        }
    }

    /// Clicks and pointer motion still need both: the pointer over the grid
    /// *and* the view holding focus. That is what keeps click-to-focus and
    /// drag-selection exactly as they were.
    #[test]
    fn mouse_events_are_only_taken_while_the_pointer_is_over_the_grid() {
        for event in [click(), egui::Event::PointerMoved(Pos2::ZERO)] {
            assert!(accepts(&event, true, true));
            assert!(!accepts(&event, false, true));
            assert!(!accepts(&event, true, false));
            assert!(!accepts(&event, false, false));
        }
    }

    /// The wheel is routed by hover alone, so the pane under the cursor
    /// scrolls without being clicked first — and the focused pane does not
    /// also scroll when the pointer is somewhere else.
    #[test]
    fn the_wheel_follows_the_pointer_not_the_focus() {
        assert!(accepts(&wheel(), true, false));
        assert!(accepts(&wheel(), true, true));
        assert!(!accepts(&wheel(), false, true));
        assert!(!accepts(&wheel(), false, false));
    }
}

#[cfg(test)]
mod selection_override_tests {
    use super::selection_override;
    use egui::Modifiers;

    /// With no modifier the program owns the mouse — that is the whole point
    /// of it having enabled reporting.
    #[test]
    fn a_bare_click_still_goes_to_the_program() {
        assert!(!selection_override(&Modifiers::NONE));
    }

    /// Shift is xterm's bypass, Option is the macOS one. Both must work, or
    /// text on screen is unreachable while any TUI is running.
    #[test]
    fn shift_or_option_hands_the_mouse_back_to_the_terminal() {
        assert!(selection_override(&Modifiers::SHIFT));
        assert!(selection_override(&Modifiers::ALT));
        assert!(selection_override(&Modifiers::SHIFT.plus(Modifiers::ALT)));
    }

    /// Ctrl is a modifier programs want reported alongside the click, and
    /// Cmd already means follow-the-link, so neither may steal the event.
    #[test]
    fn ctrl_and_command_are_left_to_their_existing_meanings() {
        assert!(!selection_override(&Modifiers::CTRL));
        assert!(!selection_override(&Modifiers::COMMAND));
    }
}

#[cfg(test)]
mod paste_tests {
    use super::{paste_bytes, PASTE_END, PASTE_START};

    fn s(text: &str, bracketed: bool) -> String {
        String::from_utf8(paste_bytes(text, bracketed)).unwrap()
    }

    /// The mode the report was filed against: DECSET 2004 means "tell me where
    /// the paste starts and ends".
    #[test]
    fn a_bracketed_paste_is_wrapped() {
        assert_eq!(s("hello", true), format!("{PASTE_START}hello{PASTE_END}"));
    }

    /// Without the mode there is nothing to wrap in — the program cannot tell
    /// a paste from typing, and must not be told it can.
    #[test]
    fn an_unbracketed_paste_is_bare() {
        assert_eq!(s("hello", false), "hello");
    }

    /// A paste is data, and the byte Enter produces is CR. Both spellings of a
    /// line break collapse to it, in both modes.
    #[test]
    fn newlines_become_carriage_returns() {
        assert_eq!(s("a\nb\r\nc", false), "a\rb\rc");
        assert_eq!(s("a\nb\r\nc", true), format!("{PASTE_START}a\rb\rc{PASTE_END}"));
        // A CR that was already a CR is left alone rather than doubled.
        assert_eq!(s("a\rb", false), "a\rb");
    }

    /// The whole point of the sanitiser: a payload carrying the terminator
    /// must not be able to close the bracket early and have its tail run as
    /// keystrokes.
    #[test]
    fn an_embedded_terminator_cannot_close_the_bracket_early() {
        let out = s(&format!("safe{PASTE_END}rm -rf /"), true);
        assert_eq!(out, format!("{PASTE_START}saferm -rf /{PASTE_END}"));
        assert_eq!(
            out.matches(PASTE_END).count(),
            1,
            "more than one terminator survived: {out:?}"
        );
        // The opener is just as bad: a second one nests, and programs that
        // count them lose track of where the paste began.
        let out = s(&format!("a{PASTE_START}b"), true);
        assert_eq!(out.matches(PASTE_START).count(), 1, "{out:?}");
    }

    /// Stripping once is not enough: deleting a marker splices its neighbours
    /// together, and the halves can spell a fresh one.
    #[test]
    fn stripping_runs_to_a_fixpoint() {
        // "\x1b[20" + "\x1b[201~" + "1~" collapses into a live terminator.
        let out = s("\x1b[20\x1b[201~1~tail", true);
        assert_eq!(out, format!("{PASTE_START}tail{PASTE_END}"));
    }
}

#[cfg(test)]
mod clipboard_key_tests {
    use super::clipboard_key_is_passthrough;
    use egui::Modifiers;

    /// On macOS the clipboard modifier is ⌘, so Ctrl+C/Ctrl+V never arrive as
    /// `Copy`/`Paste` at all and ⌘V must always paste.
    #[test]
    #[cfg(any(target_os = "ios", target_os = "macos"))]
    fn the_mac_always_uses_the_clipboard() {
        assert!(!clipboard_key_is_passthrough(Modifiers::COMMAND));
        assert!(!clipboard_key_is_passthrough(
            Modifiers::COMMAND | Modifiers::SHIFT
        ));
    }

    /// Elsewhere `command` is Ctrl: a bare Ctrl+C has to stay the interrupt,
    /// and Ctrl+Shift+C is the clipboard, as in every X11 terminal.
    #[test]
    #[cfg(not(any(target_os = "ios", target_os = "macos")))]
    fn ctrl_passes_through_and_ctrl_shift_copies() {
        assert!(clipboard_key_is_passthrough(Modifiers::COMMAND));
        assert!(!clipboard_key_is_passthrough(
            Modifiers::COMMAND | Modifiers::SHIFT
        ));
        // Shift+Insert, Ctrl+Insert and the media keys carry no command
        // modifier — the old test sent them a ^V.
        assert!(!clipboard_key_is_passthrough(Modifiers::SHIFT));
        assert!(!clipboard_key_is_passthrough(Modifiers::NONE));
    }
}
