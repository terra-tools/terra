# Tab icon assets

The `*.svg` files here are the sources; the `*-64.png` files next to them are
what terra actually ships (see `crates/terra-app/src/tab_icon.rs`). Regenerate
the PNGs after touching any SVG:

```sh
for f in crates/terra-app/assets/tab-icons/*.svg; do
    rsvg-convert -w 64 -h 64 -o "${f%.svg}-64.png" "$f"
done
```

## Simple Icons — CC0 1.0 Universal

The icons in the table below come from [Simple Icons](https://simpleicons.org)
v16.28.0, fetched from `https://unpkg.com/simple-icons@16.28.0/icons/<slug>.svg`
and given an explicit `fill` with that icon's published brand colour (the
upstream files carry no fill and render black).
Simple Icons is released under
[CC0 1.0 Universal](https://creativecommons.org/publicdomain/zero/1.0/): the
*icon files* are in the public domain and may be copied, modified and
redistributed without permission or attribution.

| file | slug | drawn for |
| --- | --- | --- |
| `claude.svg` | `claude` | `claude` (Claude Code) |
| `fishshell.svg` | `fishshell` | `fish` |
| `python.svg` | `python` | `python`, `python3`, `ipython`, `uv`, `pip` |
| `nodedotjs.svg` | `nodedotjs` | `node`, `npm`, `pnpm`, `yarn`, `bun`, `deno` |
| `docker.svg` | `docker` | `docker`, `docker-compose`, `podman` |
| `git.svg` | `git` | `git`, `lazygit`, `tig`, `gh` |
| `htop.svg` | `htop` | `htop`, `top`, `btop` |
| `vim.svg` | `vim` | `vim`, `vi` |
| `neovim.svg` | `neovim` | `nvim`, `neovim` |
| `tmux.svg` | `tmux` | `tmux`, `screen`, `zellij` |
| `rust.svg` | `rust` | `cargo`, `rustc`, `rustup` |
| `cursor.svg` | `cursor` | `cursor`, and its "Edit Settings With" row |

### Brand-guideline caveat

CC0 covers the *files*, not the trademarks they depict. Each of these marks
still belongs to its owner, and Simple Icons ships them on the understanding
that downstream use follows the owner's own brand guidelines. terra uses them
the way a file manager uses application icons: as a small, unaltered indicator
of *which program is running in a tab*, never as terra's own branding, never to
suggest that any of these projects endorses or is affiliated with terra.

The shapes are not redrawn, and each is filled with the brand colour Simple
Icons publishes for it (baked into the SVGs here, and listed in the table
above). The one exception is a colour too dark to see against terra's dark tab
bar — Rust's mark is `#000000` — which `tab_icon::readable_on_dark` lifts
toward white at load. That is a legibility floor, not a restyle: it fires only
below a luminance of 0.10, and of the icons here only Rust's is affected.

Note that Simple Icons is a single-colour set by design, so an icon here is the
one-colour version of a mark that may officially be multicolour (Python's, for
example). If a trademark owner objects to any of this, drop the file and its
row from `tab_icon.rs`; the generic terminal glyph takes over automatically.

## `openai.svg` — OpenAI's blossom mark

**Not** from Simple Icons: Simple Icons dropped its `openai` entry (it is absent
from v16.28.0 and 404s on `cdn.simpleicons.org`). The mark drawn for `codex`
here is the official OpenAI symbol as published on Wikimedia Commons:

- source: <https://commons.wikimedia.org/wiki/File:OpenAI_logo_2025_(symbol).svg>
- file: <https://upload.wikimedia.org/wikipedia/commons/6/66/OpenAI_logo_2025_%28symbol%29.svg>
- Commons states the *copyright* status as public domain (the mark is below the
  threshold of originality), credited to OpenAI.

The blossom/knot symbol was taken, not the wordmark, and the only edit is the
explicit `fill="#000000"` this repository gives every icon (the upstream file
carries no fill and renders black anyway). OpenAI's mark is **a trademark**, and
the same brand-guideline caveat above applies in full: it appears only as an
indicator of which program is running in a tab, never as terra's branding and
never to imply that OpenAI endorses or is affiliated with terra. Public domain
covers the file; it does not license the trademark.

Its colour is `#000000`, so — exactly like Rust's — `readable_on_dark` lifts it
toward white at load so it is visible on the dark bar.

Cursor's mark, in the table above, is `#000000` for the same reason and gets
the same lift.

## `vscode.svg` — hand-authored

**Not** from Simple Icons either: Simple Icons carries no `visualstudiocode`
entry (it 404s on v16.28.0, having been dropped upstream), so there was no
CC0 file to take. `vscode.svg` is therefore drawn here, by hand, from the
mark's plain geometry: a chevron fold and the bar beside it, one path with an
even-odd hole, in VS Code's `#0098FF`. It is a simplified rendition rather
than a copy of Microsoft's artwork — no gradients, no exact control points —
and it exists for one purpose, the same as every other file here: saying which
program a tab or a menu row is about.

"Visual Studio Code" and its logo are Microsoft trademarks, and the
brand-guideline caveat above applies in full. terra uses this shape only as an
indicator, never as terra's own branding, and never to imply that Microsoft
endorses or is affiliated with terra. If Microsoft objects, delete the file
and its `TabIcon::VsCode` row; the generic glyph takes over automatically.

| file | drawn for |
| --- | --- |
| `openai.svg` | `codex` (process name and title keyword) |

## `opencode.svg` — OpenCode's own mark

From OpenCode's published brand assets at <https://opencode.ai/brand>
(`https://opencode.ai/opencode-brand-assets.zip`, `Logo/opencode-logo-dark.svg`
— the variant intended for dark backgrounds). That page offers the assets for
download and states no licence, usage terms or brand guidelines of its own; the
mark is OpenCode's trademark, and terra uses it under the same caveat as every
other logo here.

The artwork is unmodified. It is 240×300, and every icon here is square, so the
shipped `opencode.svg` wraps it unchanged in a 300×300 canvas
(`<g transform="translate(30 0)">`) — a centring translation, not a redraw.

Note that this is a two-tone mark, and its darker inner block (`#4B4646`) falls
under the `MIN_LUMINANCE` floor, so `readable_on_dark` lifts that block the way
it lifts Rust's black. The mark stays legible — light plate, mid-grey block —
but it is not pixel-identical to the source at paint time.

| file | drawn for |
| --- | --- |
| `opencode.svg` | `opencode` (process name and title keyword) |

## `powershell.svg` — the PowerShell mark

**Not** from Simple Icons: PowerShell is absent from v16.28.0. The file here
comes from a public gist:

- source: <https://gist.githubusercontent.com/Xainey/d5bde7d01dcbac51ac951810e94313aa/raw/6c858c46726541b48ddaaebab29c41c07a196394/PowerShell.svg>
- author of the gist: Xainey. The artwork it contains is **Microsoft's
  PowerShell logo**, not the gist author's own design.

Be clear about what this is and is not: the gist states **no licence** at all.
So unlike Simple Icons (CC0) or OpenAI's mark (public domain on Commons), this
entry documents *provenance* rather than a licence grant. It is included on the
same footing as every other logo here — the brand-guideline caveat above applies
in full, and it applies with particular force to a Microsoft trademark: the mark
appears only as an indicator of which program is running in a tab, never as
terra's branding, and never to suggest that Microsoft endorses or is affiliated
with terra. If Microsoft (or the gist's author) objects, drop the file and its
row and `pwsh` falls back to the generic glyph, exactly as it did before.

The artwork is unmodified. It is 204.691×154.521 and every icon here is square,
so the shipped `powershell.svg` wraps it unchanged in a 204.691×204.691 canvas
(`<g transform="translate(0 25.085)">`) — a centring translation, not a redraw.

| file | drawn for |
| --- | --- |
| `powershell.svg` | `pwsh`, `powershell` (process name and title keyword) |

## `terminal.svg` — part of terra

`terminal.svg` is not from Simple Icons. It is a hand-authored `>_` prompt
glyph, original to this repository and covered by terra's own MIT licence. It
is the fallback drawn for any program with no icon of its own — and, since the
tab icons stopped branding default shells, the glyph deliberately drawn for
`zsh`, `bash`, `sh`, `dash` and `cmd` as well. It depicts no product, which is
the point.

## `gear.svg` — part of terra

`gear.svg` is not from Simple Icons either: a hand-authored gear in the same
stroke style as `terminal.svg`, original to this repository and covered by
terra's own MIT licence. It is drawn only by the chevron menu's Settings row —
no process ever resolves to it. Like the `>_` it is chrome, tinted to the text
colour at paint time.

## Icons deliberately absent

- **zsh and bash.** Their Simple Icons marks (`zsh`, `gnubash`) shipped here
  until the tab icons stopped treating a default shell as an identity; a shell
  is chrome, so it wears `terminal.svg`. The files were deleted rather than left
  unreferenced.
- **ssh.** No such icon exists upstream — OpenSSH is not a Simple Icons entry.
