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
