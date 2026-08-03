pub mod settings;
// terra patch: the child->terminal byte tee (see tap.rs).
pub mod tap;

use crate::bidi::{self, BidiBase, RowMap};
use crate::types::Size;
use alacritty_terminal::event::{
    Event, EventListener, Notify, OnResize, WindowSize,
};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{
    Selection, SelectionRange, SelectionType as AlacrittySelectionType,
};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{
    self, cell::Cell, test::TermSize, viewport_to_point, Term, TermMode,
};
use alacritty_terminal::vte::ansi::Rgb;
use alacritty_terminal::{tty, Grid};
use egui::Modifiers;
use settings::BackendSettings;
use std::borrow::Cow;
use std::collections::HashMap;
use std::cmp::min;
use std::io::Result;
use std::ops::{Index, RangeInclusive};
use std::sync::mpsc::Sender;
use std::sync::{mpsc, Arc};

pub type TerminalMode = TermMode;
pub type PtyEvent = Event;
pub type SelectionType = AlacrittySelectionType;
/// terra patch: which pasteboard an OSC 52 store names. Re-exported because
/// `PtyEvent::ClipboardStore` carries it and the embedder has to match on it.
pub type ClipboardType = term::ClipboardType;

#[derive(Debug, Clone)]
pub enum BackendCommand {
    Write(Vec<u8>),
    Scroll(i32),
    Resize(Size, Size),
    SelectStart(SelectionType, f32, f32),
    SelectUpdate(f32, f32),
    ProcessLink(LinkAction, Point),
    MouseReport(MouseButton, Modifiers, Point, bool),
}

#[derive(Debug, Clone)]
pub enum MouseMode {
    Sgr,
    Normal(bool),
}

impl From<TermMode> for MouseMode {
    fn from(term_mode: TermMode) -> Self {
        if term_mode.contains(TermMode::SGR_MOUSE) {
            MouseMode::Sgr
        } else if term_mode.contains(TermMode::UTF8_MOUSE) {
            MouseMode::Normal(true)
        } else {
            MouseMode::Normal(false)
        }
    }
}

#[derive(Debug, Clone)]
pub enum MouseButton {
    LeftButton = 0,
    MiddleButton = 1,
    RightButton = 2,
    LeftMove = 32,
    MiddleMove = 33,
    RightMove = 34,
    NoneMove = 35,
    ScrollUp = 64,
    ScrollDown = 65,
    Other = 99,
}

#[derive(Debug, Clone)]
pub enum LinkAction {
    Clear,
    Hover,
    Open,
}

#[derive(Clone, Copy, Debug)]
pub struct TerminalSize {
    pub cell_width: u16,
    pub cell_height: u16,
    num_cols: u16,
    num_lines: u16,
    layout_size: Size,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cell_width: 1,
            cell_height: 1,
            num_cols: 80,
            num_lines: 50,
            layout_size: Size::default(),
        }
    }
}

impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        self.num_lines as usize
    }

    fn columns(&self) -> usize {
        self.num_cols as usize
    }

    fn last_column(&self) -> Column {
        Column(self.num_cols as usize - 1)
    }

    fn bottommost_line(&self) -> Line {
        Line(self.num_lines as i32 - 1)
    }
}

impl From<TerminalSize> for WindowSize {
    fn from(size: TerminalSize) -> Self {
        Self {
            num_lines: size.num_lines,
            num_cols: size.num_cols,
            cell_width: size.cell_width,
            cell_height: size.cell_height,
        }
    }
}

pub struct TerminalBackend {
    id: u64,
    pty_id: u32,
    /// terra patch: whether this tab is currently rendered (see set_visible).
    visible: Arc<std::sync::atomic::AtomicBool>,
    /// terra patch: colours reported for OSC 4/10/11/12 queries.
    reported_colors: Arc<FairMutex<Option<Vec<Rgb>>>>,
    url_regex: RegexSearch,
    term: Arc<FairMutex<Term<EventProxy>>>,
    size: TerminalSize,
    notifier: Notifier,
    last_content: RenderableContent,
    /// terra patch: the last focus state reported to the program, so a
    /// report is sent on change rather than every frame. `None` means the
    /// program is not asking (mode 1004 off), which also makes re-enabling
    /// the mode re-send the current state.
    reported_focus: Option<bool>,
    /// terra patch: whether to run the BiDi pass (see [`Self::set_bidi`]).
    bidi_enabled: bool,
    /// Paragraph direction the BiDi pass resolves each row against.
    bidi_base: BidiBase,
    /// Reused row buffer so the BiDi pass allocates nothing after frame one.
    bidi_scratch: Vec<char>,
}

impl TerminalBackend {
    pub fn new(
        id: u64,
        app_context: egui::Context,
        pty_event_proxy_sender: Sender<(u64, PtyEvent)>,
        settings: BackendSettings,
    ) -> Result<Self> {
        // terra patch: identify the terminal to the programs running in it.
        //
        // Without this the child inherits whatever launched terra — start it
        // from a VS Code terminal and every program inside believes it is
        // running in VS Code. Programs branch on these, so leaking the
        // launcher's identity makes terra behave like a different terminal
        // depending on how it was started, which is both wrong and
        // impossible to reproduce.
        let mut env: HashMap<String, String> = HashMap::new();
        env.insert("TERM_PROGRAM".into(), "terra".into());
        env.insert(
            "TERM_PROGRAM_VERSION".into(),
            env!("CARGO_PKG_VERSION").into(),
        );
        // The same argument as TERM_PROGRAM, but load-bearing: TERM must
        // describe the terminal the child is *in*, not the one terra was
        // launched from — inherit it and a terra started from a tmux shell
        // hands every tab TERM=screen; started where no TERM exists at all
        // (Finder, a CI runner) the child gets none and curses programs
        // refuse to run ("terminal does not support clear" from tmux).
        // alacritty the app solves this with `tty::setup_env`, which terra
        // never calls. xterm-256color is what terra actually emulates and
        // what its docs advertise.
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        let pty_config = tty::Options {
            shell: Some(tty::Shell::new(settings.shell, settings.args)),
            working_directory: settings.working_directory,
            env,
            ..tty::Options::default()
        };
        let config = term::Config::default();
        let terminal_size = TerminalSize::default();
        let pty = tty::new(&pty_config, terminal_size.into(), id)?;
        #[cfg(not(windows))]
        let pty_id = pty.child().id();
        #[cfg(windows)]
        let pty_id = pty
            .child_watcher()
            .pid()
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Failed to get child process ID",
            ))?
            .into();
        let (event_sender, event_receiver) = mpsc::channel();
        let event_proxy = EventProxy(event_sender);
        let mut term = Term::new(config, &terminal_size, event_proxy.clone());
        let initial_content = RenderableContent {
            grid: term.grid().clone(),
            selectable_range: None,
            terminal_mode: *term.mode(),
            terminal_size,
            cursor: term.grid_mut().cursor_cell().clone(),
            hovered_hyperlink: None,
            bidi: Vec::new(),
        };
        let term = Arc::new(FairMutex::new(term));
        // terra patch: tee the child's output before the parser eats it, so
        // terra can keep a transcript of a program that clears the screen.
        // Nothing is copied when no tap is installed.
        let pty = tap::TappedPty::new(pty, settings.output_tap);
        let pty_event_loop =
            EventLoop::new(term.clone(), event_proxy, pty, false, false)?;
        let notifier = Notifier(pty_event_loop.channel());
        let pty_notifier = Notifier(pty_event_loop.channel());
        let url_regex = RegexSearch::new(r#"(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file://|git://|ssh:|ftp://)[^\u{0000}-\u{001F}\u{007F}-\u{009F}<>"\s{-}\^⟨⟩`]+"#).unwrap();
        let _pty_event_loop_thread = pty_event_loop.spawn();
        // terra patch: a background tab's output must not repaint the app —
        // otherwise one busy tab (an animation, a compiler) makes every tab
        // switch janky. The flag is flipped by TerminalBackend::set_visible.
        let visible = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let visible_in_thread = visible.clone();
        // terra patch: the palette a program sees when it asks (OSC 4/10/11/12).
        // Filled in by the view, which is the only thing that knows the theme.
        let reported_colors: Arc<FairMutex<Option<Vec<Rgb>>>> = Arc::new(FairMutex::new(None));
        let colors_in_thread = reported_colors.clone();
        let _pty_event_subscription = std::thread::Builder::new()
            .name(format!("pty_event_subscription_{}", id))
            .spawn(move || loop {
                // A closed tab drops the Term (and with it every sender)
                // without an Event::Exit ever arriving here; recv() then
                // returns Err forever and `if let Ok` would spin this
                // thread at 100% CPU for the rest of the process's life.
                let Ok(event) = event_receiver.recv() else {
                    break;
                };
                pty_event_proxy_sender
                    .send((id, event.clone()))
                    .unwrap_or_else(|_| {
                        panic!("pty_event_subscription_{}: sending PtyEvent is failed", id)
                    });
                if visible_in_thread.load(std::sync::atomic::Ordering::Relaxed)
                    || matches!(
                        event,
                        Event::Exit
                            | Event::Title(_)
                            // terra patch: a clipboard write is acted on by
                            // the UI thread (egui owns the pasteboard), so an
                            // OSC 52 copy arriving in a tab that is not the
                            // one on screen must still ask for a frame.
                            // Measured caveat: a *fully occluded* window is
                            // parked by AppKit and a requested repaint does
                            // not unpark it, so that copy lands on the next
                            // frame the window draws. The gesture this exists
                            // for — a drag the user is watching — always has
                            // one.
                            | Event::ClipboardStore(..)
                    )
                {
                    app_context.clone().request_repaint();
                }
                match event {
                    Event::Exit => break,
                    Event::PtyWrite(pty) => pty_notifier.notify(pty.into_bytes()),
                    // terra patch: answer colour queries.
                    //
                    // Programs ask what the terminal's foreground and
                    // background actually are so they can derive a shade
                    // that contrasts with it — Codex computes its
                    // composer background this way. A terminal that
                    // stays silent gets no styling at all, which looks
                    // like a missing feature rather than an unanswered
                    // question. alacritty hands us the index and a
                    // formatter; all we owe it is the colour.
                    Event::ColorRequest(index, format) => {
                        let rgb = colors_in_thread
                            .lock()
                            .as_ref()
                            .and_then(|table| table.get(index).copied());
                        if let Some(rgb) = rgb {
                            pty_notifier.notify(format(rgb).into_bytes());
                        }
                    }
                    // terra patch: OSC 52 *read* (`ESC]52;c;?`). The write
                    // direction is forwarded to the embedder as a `PtyEvent`
                    // and put on the system clipboard; this direction hands a
                    // remote program whatever the user last copied — an ssh
                    // session on a machine they do not trust exfiltrating
                    // passwords by asking politely — so terra answers with an
                    // empty string and nothing else.
                    //
                    // alacritty's `term::Config` already defaults to
                    // `Osc52::OnlyCopy`, which denies the load before it ever
                    // becomes an event, so this arm is belt and braces against
                    // a future config that turns the read on.
                    Event::ClipboardLoad(_, format) => {
                        pty_notifier.notify(format("").into_bytes());
                    }
                    _ => {}
                }
            })?;

        Ok(Self {
            id,
            pty_id,
            url_regex,
            term: term.clone(),
            size: terminal_size,
            notifier,
            visible,
            reported_colors,
            last_content: initial_content,
            reported_focus: None,
            bidi_enabled: true,
            bidi_base: BidiBase::default(),
            bidi_scratch: Vec::new(),
        })
    }

    pub fn process_command(&mut self, cmd: BackendCommand) {
        let term = self.term.clone();
        let mut term = term.lock();
        match cmd {
            BackendCommand::Write(input) => {
                self.write(input);
                term.scroll_display(Scroll::Bottom);
            }
            BackendCommand::Scroll(delta) => {
                self.scroll(&mut term, delta);
            }
            BackendCommand::Resize(layout_size, font_size) => {
                self.resize(&mut term, layout_size, font_size);
            }
            BackendCommand::SelectStart(selection_type, x, y) => {
                self.start_selection(&mut term, selection_type, x, y);
            }
            BackendCommand::SelectUpdate(x, y) => {
                self.update_selection(&mut term, x, y);
            }
            BackendCommand::ProcessLink(link_action, point) => {
                self.process_link_action(&term, link_action, point);
            }
            BackendCommand::MouseReport(button, modifiers, point, pressed) => {
                self.process_mouse_report(button, modifiers, point, pressed);
            }
        };
    }

    /// Pixel position -> **logical** grid point.
    ///
    /// terra patch: the x pixel names a *visual* column, so the row's BiDi
    /// map has to convert it back before the point reaches alacritty, which
    /// only ever speaks logical coordinates. The map used is last frame's —
    /// deliberately, because a click describes pixels the user actually saw,
    /// not a layout that may have scrolled out from under it.
    pub fn selection_point(
        &self,
        x: f32,
        y: f32,
        display_offset: usize,
    ) -> Point {
        let (vline, vcol) = self.visual_cell_at(x, y);
        let col = self.row_map(vline).logical_of(vcol);
        let col = min(Column(col), Column(self.size.num_cols as usize - 1));
        viewport_to_point(display_offset, Point::new(vline, col))
    }

    /// Pixel position -> (visible row, **visual** column), both clamped.
    fn visual_cell_at(&self, x: f32, y: f32) -> (usize, usize) {
        let col = (x as usize) / (self.size.cell_width as usize);
        let col = min(col, self.size.num_cols as usize - 1);
        let line = (y as usize) / (self.size.cell_height as usize);
        let line = min(line, self.size.num_lines as usize - 1);
        (line, col)
    }

    pub fn selectable_content(&self) -> String {
        let content = self.last_content();
        let mut result = String::new();
        if let Some(range) = content.selectable_range {
            for indexed in content.grid.display_iter() {
                if range.contains(indexed.point) {
                    result.push(indexed.c);
                }
            }
        }
        result
    }

    pub fn sync(&mut self) -> &RenderableContent {
        let term = self.term.clone();
        let mut terminal = term.lock();
        let selectable_range = match &terminal.selection {
            Some(s) => s.to_range(&terminal),
            None => None,
        };

        let cursor = terminal.grid_mut().cursor_cell().clone();
        self.last_content.grid = terminal.grid().clone();
        self.last_content.selectable_range = selectable_range;
        self.last_content.cursor = cursor.clone();
        self.last_content.terminal_mode = *terminal.mode();
        self.last_content.terminal_size = self.size;
        drop(terminal);
        self.sync_bidi();
        self.last_content()
    }

    /// terra patch: recompute the visual order of every visible row.
    ///
    /// Runs against the grid snapshot `sync` just took, so the map and the
    /// cells it describes can never be a frame apart. Rows of ordinary
    /// output hit [`bidi::map_row`]'s identity fast path and allocate
    /// nothing, so the usual cost is one integer compare per cell — far less
    /// than the grid clone happening just above.
    fn sync_bidi(&mut self) {
        self.last_content.bidi.clear();
        if !self.bidi_enabled {
            return;
        }

        let display_offset = self.last_content.grid.display_offset() as i32;
        let cols = self.size.columns();
        let rows = self.size.screen_lines();
        let bottom = self.last_content.grid.bottommost_line();

        for vline in 0..rows {
            let line = Line(-display_offset + vline as i32);
            if line > bottom {
                break;
            }
            let row = &self.last_content.grid[line];
            self.bidi_scratch.clear();
            self.bidi_scratch.reserve(cols);
            let mut col = 0;
            while col < cols {
                let grid_cell = &row[Column(col)];
                self.bidi_scratch.push(grid_cell.c);
                // A wide char spans two columns. Push it twice rather than
                // pushing the spacer's blank: identical characters mean
                // identical Bidi classes, hence identical levels, hence a
                // pair L2 can never split across a run boundary.
                let wide =
                    grid_cell.flags.contains(term::cell::Flags::WIDE_CHAR);
                if wide && col + 1 < cols {
                    self.bidi_scratch.push(grid_cell.c);
                    col += 1;
                }
                col += 1;
            }
            let map = bidi::map_row(&self.bidi_scratch, self.bidi_base);
            self.last_content.bidi.push(map);
        }
    }

    /// terra patch: whether the colour table still needs filling in.
    ///
    /// The theme lives in the view, so the view supplies it — but only once,
    /// which keeps a 259-entry table off the per-frame path.
    pub fn wants_colors(&self) -> bool {
        self.reported_colors.lock().is_none()
    }

    /// terra patch: the palette to answer colour queries from.
    ///
    /// Indices follow alacritty's table: 0..=255 are the ANSI palette, then
    /// foreground, background and cursor.
    pub fn set_reported_colors(&self, colors: Vec<Rgb>) {
        *self.reported_colors.lock() = Some(colors);
    }

    /// terra patch: report window focus to the program (DECSET 1004).
    ///
    /// Programs that enable focus reporting — Codex does, and it is what
    /// draws its composer highlight — assume the terminal is *unfocused*
    /// until told otherwise. Never sending the report leaves them rendering
    /// a permanently-blurred UI, which looks like a missing background
    /// rather than a missing escape sequence.
    pub fn set_focused(&mut self, focused: bool) {
        let enabled = self.last_content.terminal_mode.contains(TermMode::FOCUS_IN_OUT);
        let Some(report) = focus_report(enabled, focused, self.reported_focus)
        else {
            if !enabled {
                self.reported_focus = None;
            }
            return;
        };
        self.reported_focus = Some(report);
        // CSI I on focus in, CSI O on focus out.
        let seq: &[u8] = if report { b"\x1b[I" } else { b"\x1b[O" };
        self.notifier.notify(seq.to_vec());
    }

    /// terra patch: turn BiDi reordering on or off for this tab.
    ///
    /// Disabling clears the maps, so every accessor falls back to the
    /// identity and both the renderer and the hit-tester return to logical
    /// order in the same frame.
    pub fn set_bidi(&mut self, enabled: bool) {
        if self.bidi_enabled != enabled {
            self.bidi_enabled = enabled;
            self.last_content.bidi.clear();
        }
    }

    /// terra patch: choose the paragraph direction rows resolve against.
    ///
    /// Clearing the maps on a change means the very next paint uses the new
    /// base, rather than one stale frame in the old one.
    pub fn set_bidi_base(&mut self, base: BidiBase) {
        if self.bidi_base != base {
            self.bidi_base = base;
            self.last_content.bidi.clear();
        }
    }

    /// The visual order of the visible row at `vline`, or the identity when
    /// BiDi is off or the row is past the end of the viewport.
    fn row_map(&self, vline: usize) -> RowMap {
        self.last_content
            .bidi
            .get(vline)
            .cloned()
            .unwrap_or(RowMap::Identity(self.size.columns()))
    }

    pub fn last_content(&self) -> &RenderableContent {
        &self.last_content
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// terra patch: mark whether this tab is on screen. Output from hidden
    /// tabs still updates the grid but no longer wakes the UI.
    pub fn set_visible(&self, visible: bool) {
        self.visible
            .store(visible, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn pty_id(&self) -> u32 {
        self.pty_id
    }

    fn process_link_action(
        &mut self,
        terminal: &Term<EventProxy>,
        link_action: LinkAction,
        point: Point,
    ) {
        match link_action {
            LinkAction::Hover => {
                self.last_content.hovered_hyperlink = self.regex_match_at(
                    terminal,
                    point,
                    &mut self.url_regex.clone(),
                );
            }
            LinkAction::Clear => {
                self.last_content.hovered_hyperlink = None;
            }
            LinkAction::Open => {
                self.open_link();
            }
        };
    }

    fn open_link(&self) {
        if let Some(range) = &self.last_content.hovered_hyperlink {
            let start = range.start();
            let end = range.end();

            let mut url = String::from(self.last_content.grid.index(*start).c);
            for indexed in self.last_content.grid.iter_from(*start) {
                url.push(indexed.c);
                if indexed.point == *end {
                    break;
                }
            }

            open::that(url).unwrap_or_else(|_| {
                panic!("link opening is failed");
            })
        }
    }

    fn process_mouse_report(
        &self,
        button: MouseButton,
        modifiers: Modifiers,
        point: Point,
        pressed: bool,
    ) {
        let mut mods = 0;
        if modifiers.contains(Modifiers::SHIFT) {
            mods += 4;
        }
        if modifiers.contains(Modifiers::ALT) {
            mods += 8;
        }
        if modifiers.contains(Modifiers::COMMAND) {
            mods += 16;
        }

        match MouseMode::from(self.last_content().terminal_mode) {
            MouseMode::Sgr => {
                self.sgr_mouse_report(point, button as u8 + mods, pressed)
            }
            MouseMode::Normal(is_utf8) => {
                if pressed {
                    self.normal_mouse_report(
                        point,
                        button as u8 + mods,
                        is_utf8,
                    )
                } else {
                    self.normal_mouse_report(point, 3 + mods, is_utf8)
                }
            }
        }
    }

    fn sgr_mouse_report(&self, point: Point, button: u8, pressed: bool) {
        let c = if pressed { 'M' } else { 'm' };

        let msg = format!(
            "\x1b[<{};{};{}{}",
            button,
            point.column + 1,
            point.line + 1,
            c
        );

        self.notifier.notify(msg.as_bytes().to_vec());
    }

    fn normal_mouse_report(&self, point: Point, button: u8, is_utf8: bool) {
        let Point { line, column } = point;
        let max_point = if is_utf8 { 2015 } else { 223 };

        if line >= max_point || column >= max_point {
            return;
        }

        let mut msg = vec![b'\x1b', b'[', b'M', 32 + button];

        let mouse_pos_encode = |pos: usize| -> Vec<u8> {
            let pos = 32 + 1 + pos;
            let first = 0xC0 + pos / 64;
            let second = 0x80 + (pos & 63);
            vec![first as u8, second as u8]
        };

        if is_utf8 && column >= Column(95) {
            msg.append(&mut mouse_pos_encode(column.0));
        } else {
            msg.push(32 + 1 + column.0 as u8);
        }

        if is_utf8 && line >= 95 {
            msg.append(&mut mouse_pos_encode(line.0 as usize));
        } else {
            msg.push(32 + 1 + line.0 as u8);
        }

        self.notifier.notify(msg);
    }

    fn start_selection(
        &mut self,
        terminal: &mut Term<EventProxy>,
        selection_type: SelectionType,
        x: f32,
        y: f32,
    ) {
        let location =
            self.selection_point(x, y, terminal.grid().display_offset());
        terminal.selection = Some(Selection::new(
            selection_type,
            location,
            self.selection_side(x, y),
        ));
    }

    fn update_selection(
        &mut self,
        terminal: &mut Term<EventProxy>,
        x: f32,
        y: f32,
    ) {
        let display_offset = terminal.grid().display_offset();
        let location = self.selection_point(x, y, display_offset);
        let side = self.selection_side(x, y);
        if let Some(ref mut selection) = terminal.selection {
            selection.update(location, side);
        }
    }

    /// Which side of a cell the pointer is on, in **logical** terms.
    ///
    /// terra patch: `Side` is what alacritty's `Selection` uses to decide
    /// whether the anchor cell is itself inside the range, so it is logical —
    /// but the pointer offset producing it is visual. Inside an RTL run the
    /// two are mirrored, and skipping the flip costs an off-by-one at *both*
    /// ends of every RTL selection: a zero-drag click on the left half of a
    /// Hebrew glyph selects one character instead of clearing the selection,
    /// and dragging makes the boundary cell flicker in and out.
    fn selection_side(&self, x: f32, y: f32) -> Side {
        let cell_x = x as usize % self.size.cell_width as usize;
        let half_cell_width = (self.size.cell_width as f32 / 2.0) as usize;
        let visual_right = cell_x > half_cell_width;

        let (vline, vcol) = self.visual_cell_at(x, y);
        let map = self.row_map(vline);
        let is_rtl = map.is_rtl(map.logical_of(vcol));

        if visual_right != is_rtl {
            Side::Right
        } else {
            Side::Left
        }
    }

    fn resize(
        &mut self,
        terminal: &mut Term<EventProxy>,
        layout_size: Size,
        font_size: Size,
    ) {
        if layout_size == self.size.layout_size
            && font_size.width as u16 == self.size.cell_width
            && font_size.height as u16 == self.size.cell_height
        {
            return;
        }

        let lines = (layout_size.height / font_size.height.floor()) as u16;
        let cols = (layout_size.width / font_size.width.floor()) as u16;
        if lines > 0 && cols > 0 {
            self.size = TerminalSize {
                layout_size,
                cell_height: font_size.height as u16,
                cell_width: font_size.width as u16,
                num_lines: lines,
                num_cols: cols,
            };

            self.notifier.on_resize(self.size.into());
            terminal.resize(TermSize::new(
                self.size.num_cols as usize,
                self.size.num_lines as usize,
            ));
        }
    }

    fn write<I: Into<Cow<'static, [u8]>>>(&self, input: I) {
        self.notifier.notify(input);
    }

    fn scroll(&mut self, terminal: &mut Term<EventProxy>, delta_value: i32) {
        if delta_value != 0 {
            let scroll = Scroll::Delta(delta_value);
            if terminal
                .mode()
                .contains(TermMode::ALTERNATE_SCROLL | TermMode::ALT_SCREEN)
            {
                let line_cmd = if delta_value > 0 { b'A' } else { b'B' };
                let mut content = vec![];

                for _ in 0..delta_value.abs() {
                    content.push(0x1b);
                    content.push(b'O');
                    content.push(line_cmd);
                }

                self.notifier.notify(content);
            } else {
                terminal.grid_mut().scroll_display(scroll);
            }
        }
    }

    /// Based on alacritty/src/display/hint.rs > regex_match_at
    /// Retrieve the match, if the specified point is inside the content matching the regex.
    fn regex_match_at(
        &self,
        terminal: &Term<EventProxy>,
        point: Point,
        regex: &mut RegexSearch,
    ) -> Option<Match> {
        let x = visible_regex_match_iter(terminal, regex)
            .find(|rm| rm.contains(&point));
        x
    }
}

/// Copied from alacritty/src/display/hint.rs:
/// Iterate over all visible regex matches.
fn visible_regex_match_iter<'a>(
    term: &'a Term<EventProxy>,
    regex: &'a mut RegexSearch,
) -> impl Iterator<Item = Match> + 'a {
    let viewport_start = Line(-(term.grid().display_offset() as i32));
    let viewport_end = viewport_start + term.bottommost_line();
    let mut start =
        term.line_search_left(Point::new(viewport_start, Column(0)));
    let mut end = term.line_search_right(Point::new(viewport_end, Column(0)));
    start.line = start.line.max(viewport_start - 100);
    end.line = end.line.min(viewport_end + 100);

    RegexIter::new(start, end, Direction::Right, term, regex)
        .skip_while(move |rm| rm.end().line < viewport_start)
        .take_while(move |rm| rm.start().line <= viewport_end)
}

/// What focus state to report, or `None` when there is nothing to send.
///
/// Reporting only on change keeps the PTY quiet, but the mode is typically
/// enabled *after* the window already has focus — so enabling it must also
/// produce a report, which falls out of resetting `last` to `None` whenever
/// the mode is off.
fn focus_report(
    mode_enabled: bool,
    focused: bool,
    last: Option<bool>,
) -> Option<bool> {
    if !mode_enabled || last == Some(focused) {
        return None;
    }
    Some(focused)
}

pub struct RenderableContent {
    pub grid: Grid<Cell>,
    pub hovered_hyperlink: Option<RangeInclusive<Point>>,
    pub selectable_range: Option<SelectionRange>,
    pub cursor: Cell,
    pub terminal_mode: TermMode,
    pub terminal_size: TerminalSize,
    /// terra patch: one BiDi map per *visible* row, index 0 being the
    /// topmost visible row. Rebuilt by every [`TerminalBackend::sync`].
    ///
    /// It lives here, rather than being recomputed in the view, because the
    /// renderer and the hit-tester must agree exactly. `selection_point` has
    /// no access to anything the paint loop computes, and two independent
    /// derivations of one permutation would be free to drift — which shows
    /// up only as clicks landing a cell off, and only on RTL rows.
    pub bidi: Vec<RowMap>,
}

impl Default for RenderableContent {
    fn default() -> Self {
        Self {
            grid: Grid::new(0, 0, 0),
            hovered_hyperlink: None,
            selectable_range: None,
            bidi: Vec::new(),
            cursor: Cell::default(),
            terminal_mode: TermMode::empty(),
            terminal_size: TerminalSize::default(),
        }
    }
}

impl Drop for TerminalBackend {
    fn drop(&mut self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

#[derive(Clone)]
pub struct EventProxy(mpsc::Sender<Event>);

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let _ = self.0.send(event.clone());
    }
}

#[cfg(test)]
mod focus_tests {
    use super::focus_report;

    /// A program that has not asked for focus reports must never be sent
    /// one — the bytes would land in its input stream as garbage.
    #[test]
    fn nothing_is_reported_while_the_mode_is_off() {
        assert_eq!(focus_report(false, true, None), None);
        assert_eq!(focus_report(false, false, Some(true)), None);
    }

    /// The mode is enabled *after* the window already has focus, so enabling
    /// it has to produce a report. This is the case that made Codex render a
    /// permanently-unfocused composer.
    #[test]
    fn enabling_the_mode_reports_the_current_state() {
        assert_eq!(focus_report(true, true, None), Some(true));
        assert_eq!(focus_report(true, false, None), Some(false));
    }

    #[test]
    fn only_changes_are_reported() {
        assert_eq!(focus_report(true, true, Some(true)), None);
        assert_eq!(focus_report(true, false, Some(true)), Some(false));
        assert_eq!(focus_report(true, true, Some(false)), Some(true));
    }
}
