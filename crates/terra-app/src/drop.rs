//! Files dropped on the window become shell words in the focused tab.
//!
//! Dragging a file out of Finder (or Nautilus, or Explorer) onto a terminal is
//! how everyone types a long path: iTerm2, Ghostty and VS Code's terminal all
//! answer a drop by *pasting the paths as a command line*, and terra does the
//! same. The mapping is the whole feature and it is a pure function, which is
//! what lives here — the app only has to hand it `i.raw.dropped_files` and put
//! the result on the PTY through the ordinary paste path (bracketed paste
//! included, so a shell that asked for the markers still gets them).
//!
//! Two rules copied from the terminals above, because a user's muscle memory
//! is the spec:
//!
//! * **One space between paths, and a trailing space after the last one.** The
//!   trailing space is not a typo — after a drop the cursor sits ready for the
//!   next word, so `cp ` + drop + drop + Return is the whole gesture.
//! * **Quote only when the shell would otherwise mis-read it.** A dropped
//!   `/usr/bin/env` must paste as itself, or the terminal is unusable for
//!   copying a path back out; a `~/My Notes/todo (old).md` must come out as
//!   one word.
//!
//! The quoting is `sh`'s, not a library's: single quotes, with an embedded
//! quote spliced as `'\''`. It is one screenful and it is testable, which beats
//! a dependency for a job this small.

use std::path::Path;

/// Which shell's quoting rules the paste is written for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    /// POSIX `sh` — single quotes, `'\''` for an embedded quote.
    Posix,
    /// `cmd.exe`/PowerShell — double quotes, since `\` is a path separator
    /// there rather than an escape.
    Windows,
}

impl Style {
    /// What the shell on *this* platform expects.
    pub fn native() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

/// Characters a POSIX shell reads literally wherever they appear in a word.
///
/// Everything else — spaces, `$`, backticks, `*?[]`, quotes, `\`, `;&|<>()`,
/// `{}`, `!`, `#`, newlines — either expands, splits or terminates the word.
/// A `~` is literal everywhere but the first position, which `quote_posix`
/// handles on its own. Letters and digits are judged by Unicode, so a path through `~/文档` or
/// `Ordner` stays readable: the shell splits on `IFS` (space, tab, newline)
/// and expands on ASCII punctuation, and non-ASCII letters are neither.
fn posix_bare(c: char) -> bool {
    matches!(
        c,
        '_' | '-' | '.' | '/' | ':' | '@' | '%' | '+' | ',' | '=' | '~'
    ) || (c.is_alphanumeric() && !c.is_whitespace() && !c.is_control())
}

/// One path as one shell word.
///
/// Returns the path untouched when every character is safe, so the common case
/// — an absolute path of ordinary names — pastes as the plain text a user
/// would have typed.
pub fn quote(path: &str, style: Style) -> String {
    match style {
        Style::Posix => quote_posix(path),
        Style::Windows => quote_windows(path),
    }
}

fn quote_posix(path: &str) -> String {
    // A leading `~` is bare-safe by the character rule and still expands, so
    // it is the one position that forces quoting on its own; `-` first would
    // be read as an option by whatever command is being built.
    let leading = path.starts_with('~') || path.starts_with('-');
    if !path.is_empty() && !leading && path.chars().all(posix_bare) {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len() + 2);
    out.push('\'');
    for c in path.chars() {
        if c == '\'' {
            // Close, escape a literal quote, reopen — the only way out of a
            // single-quoted string in sh.
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn quote_windows(path: &str) -> String {
    let bare = |c: char| posix_bare(c) || c == '\\';
    if !path.is_empty() && !path.starts_with('-') && path.chars().all(bare) {
        return path.to_string();
    }
    // A Windows filename cannot contain `"`, so wrapping is enough — except
    // for trailing backslashes, which would escape the closing quote. Double
    // exactly that run, the rule CommandLineToArgvW parses back.
    let trailing = path.len() - path.trim_end_matches('\\').len();
    let mut out = String::with_capacity(path.len() + 2 + trailing);
    out.push('"');
    out.push_str(path);
    for _ in 0..trailing {
        out.push('\\');
    }
    out.push('"');
    out
}

/// The text a set of dropped paths pastes as: quoted words, single spaces
/// between them, one trailing space.
pub fn paste_text<'a, I>(paths: I, style: Style) -> Option<String>
where
    I: IntoIterator<Item = &'a Path>,
{
    let mut out = String::new();
    for path in paths {
        // `to_string_lossy` rather than a skip: a path that is not UTF-8 is
        // still better pasted with a replacement character than swallowed, and
        // the user can see what happened.
        out.push_str(&quote(&path.to_string_lossy(), style));
        out.push(' ');
    }
    (!out.is_empty()).then_some(out)
}

/// What one frame's `i.raw.dropped_files` should put on the PTY.
///
/// `None` when the drop carried nothing terra can name — the web backend's
/// byte-only drops, which the native app never sees.
pub fn dropped_text(files: &[egui::DroppedFile], style: Style) -> Option<String> {
    paste_text(files.iter().filter_map(|f| f.path.as_deref()), style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn an_ordinary_path_pastes_as_itself() {
        for path in [
            "/usr/bin/env",
            "/Users/me/projects/terra/README.md",
            "/opt/homebrew/bin/rg",
            "relative/path.txt",
            "/tmp/a-b_c.2024,v1+final=yes@host%20",
        ] {
            assert_eq!(quote(path, Style::Posix), path, "{path} should stay bare");
        }
    }

    #[test]
    fn a_space_or_a_shell_metacharacter_forces_quotes() {
        assert_eq!(quote("/tmp/My Notes", Style::Posix), "'/tmp/My Notes'");
        assert_eq!(quote("/tmp/$HOME", Style::Posix), "'/tmp/$HOME'");
        assert_eq!(quote("/tmp/a`b`", Style::Posix), "'/tmp/a`b`'");
        assert_eq!(quote("/tmp/a*b?", Style::Posix), "'/tmp/a*b?'");
        assert_eq!(quote("/tmp/a;rm -rf /", Style::Posix), "'/tmp/a;rm -rf /'");
        assert_eq!(quote("/tmp/a\\b", Style::Posix), "'/tmp/a\\b'");
        assert_eq!(quote("/tmp/(old)", Style::Posix), "'/tmp/(old)'");
        assert_eq!(quote("/tmp/a\nb", Style::Posix), "'/tmp/a\nb'");
        assert_eq!(quote("", Style::Posix), "''");
    }

    /// The interesting one: single quotes cannot nest, so the word is closed,
    /// the quote escaped on its own, and the word reopened.
    #[test]
    fn an_embedded_single_quote_is_spliced_not_nested() {
        assert_eq!(
            quote("/tmp/it's here", Style::Posix),
            "'/tmp/it'\\''s here'"
        );
        assert_eq!(quote("'", Style::Posix), "''\\'''");
        // Double quotes need nothing special inside single quotes.
        assert_eq!(quote("/tmp/\"q\"", Style::Posix), "'/tmp/\"q\"'");
    }

    /// A leading `~` still expands even though every character is bare-safe,
    /// and a leading `-` would be read as an option.
    #[test]
    fn a_leading_tilde_or_dash_is_quoted() {
        assert_eq!(quote("~/notes", Style::Posix), "'~/notes'");
        assert_eq!(quote("-rf", Style::Posix), "'-rf'");
        // Not in the middle, where neither means anything.
        assert_eq!(quote("/tmp/a~b-c", Style::Posix), "/tmp/a~b-c");
    }

    #[test]
    fn unicode_names_stay_readable() {
        assert_eq!(
            quote("/Users/me/文档/notes.md", Style::Posix),
            "/Users/me/文档/notes.md"
        );
        assert_eq!(quote("/Users/me/Müller", Style::Posix), "/Users/me/Müller");
        assert_eq!(quote("/Users/me/מסמכים", Style::Posix), "/Users/me/מסמכים");
        // A non-breaking space is not `IFS`, but it is whitespace and nobody
        // can see it — quote it rather than hand over a word that looks like
        // two.
        assert_eq!(quote("/tmp/a\u{a0}b", Style::Posix), "'/tmp/a\u{a0}b'");
    }

    #[test]
    fn several_paths_are_space_separated_with_a_trailing_space() {
        let paths = [
            PathBuf::from("/tmp/one"),
            PathBuf::from("/tmp/two words"),
            PathBuf::from("/tmp/three"),
        ];
        assert_eq!(
            paste_text(paths.iter().map(PathBuf::as_path), Style::Posix).unwrap(),
            "/tmp/one '/tmp/two words' /tmp/three "
        );
    }

    #[test]
    fn one_path_still_ends_in_a_space() {
        let one = PathBuf::from("/tmp/x");
        assert_eq!(
            paste_text([one.as_path()], Style::Posix).unwrap(),
            "/tmp/x "
        );
    }

    #[test]
    fn a_drop_with_no_paths_pastes_nothing() {
        assert_eq!(paste_text([], Style::Posix), None);
        assert_eq!(dropped_text(&[], Style::Posix), None);
        assert_eq!(
            dropped_text(&[egui::DroppedFile::default()], Style::Posix),
            None
        );
    }

    #[test]
    fn dropped_files_map_to_their_paths() {
        let files = vec![
            egui::DroppedFile {
                path: Some(PathBuf::from("/tmp/a b")),
                ..Default::default()
            },
            // No path (the web backend's shape): skipped, the rest still land.
            egui::DroppedFile::default(),
            egui::DroppedFile {
                path: Some(PathBuf::from("/tmp/c")),
                ..Default::default()
            },
        ];
        assert_eq!(
            dropped_text(&files, Style::Posix).unwrap(),
            "'/tmp/a b' /tmp/c "
        );
    }

    #[test]
    fn windows_paths_use_double_quotes_and_keep_their_separators() {
        assert_eq!(
            quote(r"C:\Users\me\notes.md", Style::Windows),
            r"C:\Users\me\notes.md"
        );
        assert_eq!(
            quote(r"C:\Users\me\My Files\a.txt", Style::Windows),
            "\"C:\\Users\\me\\My Files\\a.txt\""
        );
        // A trailing separator would escape the closing quote; it is doubled,
        // which is how CommandLineToArgvW reads it back as one backslash.
        assert_eq!(
            quote(r"C:\Program Files\", Style::Windows),
            "\"C:\\Program Files\\\\\""
        );
        assert_eq!(
            paste_text([Path::new(r"C:\a b"), Path::new(r"C:\c")], Style::Windows).unwrap(),
            "\"C:\\a b\" C:\\c "
        );
    }
}
