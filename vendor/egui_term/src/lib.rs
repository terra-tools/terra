// terra: vendored code keeps upstream style; silence pedantic lints.
#![allow(clippy::too_many_arguments, clippy::manual_is_multiple_of)]

mod backend;
mod bidi;
mod emoji;
mod bindings;
mod font;
mod theme;
mod types;
mod view;

pub use backend::settings::BackendSettings;
pub use backend::tap::OutputTap;
pub use backend::{
    // terra patch: `LinkAction` is half of `BackendCommand::ProcessLink`, so
    // an embedder (or a harness test) cannot form that command without it.
    BackendCommand,
    ClipboardType,
    LinkAction,
    PtyEvent,
    TerminalBackend,
    TerminalMode,
};
pub use bidi::{BidiBase, RowMap};
pub use bindings::{Binding, BindingAction, InputKind, KeyboardBinding};
pub use font::{FontSettings, TerminalFont};
pub use theme::{ColorPalette, TerminalTheme};
pub use view::{paste_bytes, TerminalView};
