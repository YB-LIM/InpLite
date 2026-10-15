# InpLite

A lightweight editor for **Abaqus input files (`.inp`)** that stays fast on multi‑GB models.

Version 1.0 · Windows (x64) · written in Rust with [egui](https://github.com/emilk/egui)

<img width="1682" height="1082" alt="For_GitHub" src="https://github.com/user-attachments/assets/a026e427-aa91-4d44-a076-abb67a24ec7d" />


## Why it is fast

Most of an `.inp` file is node/element tables that nobody edits by hand. InpLite memory‑maps the
file and scans it once (≈0.5 s per GB). Every long run of data lines stays **on disk** and appears
in the editor as a single blue placeholder line:

```
*Node
**@@BULK 1@@ *Node data: 10,096,807 lines, 491.2 MB
*Element, type=C3D8R
**@@BULK 2@@ *Element data: 8,640,000 lines, 667.8 MB
```

The editable text is only keyword lines, comments and small data blocks (a few hundred KB even
for a 1 GB model). The line-number gutter shows the **real** file line numbers. On save, each
placeholder is replaced by the original bytes, so untouched data is copied byte for byte.

The editor itself only highlights, lays out and paints the lines on screen, and its undo history
stores the changed text rather than copies of the whole text, so typing and scrolling cost the same
in a 100-line file and in a flat file with 130,000 keyword lines. Problem lists, outline and model
checks are computed in the background.

Measured on a 1.16 GB model (10.1 M nodes, 8.6 M C3D8R elements):

| | |
|---|---|
| open (warm cache) | 0.45 s |
| private memory after open | < 1 MB (+60 MB after the background mesh check) |
| background mesh check (duplicate IDs, undefined nodes, connectivity) | 3–4 s |
| find node / element label | < 1 ms |
| save | 2 s, byte-identical round trip |
| keyword-aware compare of two 1.2 GB files | < 1 s after loading |

Flat 174 MB model with 58,000 keywords (130,000 editable lines): about 1 ms per frame, 4–6 ms per
keystroke.

## Features

* **Syntax colouring** for keywords, parameters, values, comments, `<parameter>` references and data.
* **Instant syntax errors (red)** using a schema extracted from the *Abaqus 2025 Keywords Guide*
  (600 keywords, 2,355 parameters): unknown keyword or parameter (with "did you mean"), missing
  required parameter, mutually exclusive parameters used together, bad numbers in `*NODE`/`*ELEMENT`,
  lines over 256 characters, unbalanced quotes. Parameter abbreviations that Abaqus accepts but the
  guide does not list (e.g. `VAR=PRESELECT` for `VARIABLE=PRESELECT`) are not errors: any unique
  prefix of at least 3 letters counts as the parameter.
* **Model checks (background)**: `*PART`/`*ASSEMBLY`/`*INSTANCE`/`*STEP` pairing, model data inside a
  step, history data outside a step, undefined node
  sets / element sets / surfaces / materials / amplitudes / interactions (definitions inside
  `*INCLUDE` files are read too), undefined `<parameters>`, keywords not available in the
  detected product (Standard/Explicit), undocumented parameter values (warning), duplicate
  node/element labels, elements referencing undefined nodes, wrong number of element nodes.
* **Outline** of parts, assembly/instances, materials, interaction properties and steps, with filter.
  Long lists (flat input files) are grouped by keyword, e.g. one expandable *Nset entry for 40,000 sets.
* **`*INCLUDE` split**: right-click a data block (the blue line in the editor, or the data
  viewer), a keyword line, or an outline entry → *Save as \*Include file…*. The block (keyword
  line + data) is written to its own file in the background and replaced by an `*Include` line.
  The file goes to the default include folder (Settings: folder of the input file, a sub-folder
  such as `include`, or a fixed folder); *Browse…* picks another one. Files in the input file's
  folder or below are referenced with a relative path (`include/part.inp`, works on Windows and
  Linux), others with the full path.
  * *Tools → Save all part meshes* (Part/Assembly input; for a flat input: the whole mesh), or
    right-click a part → *Save this part's mesh*: choose one file (`*Node` + `*Element` + the
    sets right after them), two files (`*Node`, `*Element`) or four files (`*Node`, `*Element`,
    `*Nset`, `*Elset`) per part.
  * *Tools → Save data blocks* writes every keyword block with at least N data lines (N chosen
    in the dialog) into its own file.
  * Grouped outline entries (e.g. *Element, 2,245 blocks) can be saved into one file or one file each.
  * Batch saves show the list of files and a folder selector first; existing files are skipped
    unless *Overwrite* is ticked. Ctrl+click an `*INCLUDE` line opens the file; *Inline include
    here* (outline) does the reverse.
* **Name jump**: Ctrl+click / F12 on a set, surface, material, amplitude, instance or `<parameter>`
  jumps to its definition (also into include files).
* **Find references** (Shift+F12 or right-click): with the cursor on such a name, the *References*
  tab lists its definition and every place it is used (e.g. all `*Boundary`, `*Cload` lines and
  sections that use an `elset`); click an entry to jump there.
* **Go to** (Ctrl+G): real line number, node label or element label. In Part/Assembly input the
  same label exists in several parts: Enter jumps to the first one, Enter again to the next part.
* **Structure detection**: the status bar shows *Part / Assembly* or *Flat input*; mesh export and
  Go to adapt to it.
* **Data viewer** for large blocks: virtual scrolling over millions of lines, aligned columns,
  go to label, right-click menu. The line index and label searches run in the background, so the
  window never freezes. *Edit as text* loads a block into the editor (up to 200,000 lines / 16 MB;
  for larger blocks a dialog explains why and offers *Save as \*Include file* or the data viewer).
* **Export without mesh**: writes a small copy with node/element tables replaced by comments,
  handy for sending a model setup to someone.
* **Compare** (File → Compare with file…): block-by-block diff that ignores blanks, letter case
  and comments; parameter changes and data-line changes are listed; huge blocks are compared line
  by line in the background.
* **Completion** (automatic, or Ctrl+Space; accept with Enter, Tab or a double-click): keywords (adds the first required parameter),
  parameters of the current keyword, documented values, and names of sets/materials/etc. defined
  in the model.
* **Model summary**: counts of nodes/elements per type and part, sets, surfaces, materials,
  sections, interactions, steps and procedures (long lists wrap; the window scrolls).
* **Safe save** (in the background, with a progress dialog): the new file is written to a temporary file, flushed and size-checked, the
  previous version is kept as `name.inp.bak`, then the new file is moved into place. If the file
  was changed by another program since it was opened you are asked before overwriting. If data
  blocks were deleted from the text you are asked before they are dropped.

Not included on purpose: 3D mesh viewer, renumbering, keyword dialogs, reformatting.
Non‑ASCII text (e.g. Korean comments) is not supported: it is not displayed properly, but the
bytes are written back unchanged on save.

## Keyboard

| | |
|---|---|
| Ctrl+O / Ctrl+S / Ctrl+Shift+S | open / save / save as |
| Ctrl+F | find in the editor text |
| Ctrl+G | go to line / node / element |
| Ctrl+click, F12 | go to definition, open `*INCLUDE` file, open data block |
| Shift+F12 | find references |
| Ctrl+Space | completion |
| Ctrl+Z / Ctrl+Y (Ctrl+Shift+Z) | undo / redo |
| Ctrl+A, Shift+arrows, Ctrl+arrows, double/triple click | select all / extend / word-wise / word / line |
| middle mouse button | autoscroll: hold and move (or click, move, click again to stop); Esc stops |
| F5 | re-run checks |
| Ctrl + / Ctrl - | font size |

## Windows 11 right-click menu

Windows 11 shows menu entries of classic (non-Store) programs only under *Show more options*
(or Shift+right-click). InpLite also registers itself under *Open with*, which is in the short
menu. The installer has an optional component *Windows 11: always show the full right-click menu*,
which switches Explorer back to the Windows 10 style menu for your account (takes effect after
signing out or restarting Explorer; undone by the uninstaller). A top-level entry in the new
Windows 11 menu would need a signed MSIX package, which this draft does not have.

## Settings

*Tools → Settings*: data-block threshold (default 2,000 lines; InpLite lowers it automatically
so the editable text stays around 10,000 lines), `.bak` backups, font size, default folder for
`*Include` files. Settings live in
`%APPDATA%\InpLite\settings.json`.

## Disclaimer

InpLite is an independent, personal open-source project. It is **not an official SIMULIA /
Dassault Systèmes product**; it is not developed, endorsed, supported or validated by Dassault
Systèmes and has no relation to the company the developer works for. Abaqus, SIMULIA and
Dassault Systèmes are trademarks of Dassault Systèmes or its subsidiaries. The software is
provided "as is" (MIT License) and you use it at your own risk: check edited input files before
running analyses and keep backups. Syntax checks may be incomplete; Abaqus (e.g. datacheck) is the
reference. Please do not contact SIMULIA support about this tool. (Also shown under
*Information → Disclaimer*.)

## Developer

Developed by **Youngbin LIM** · built with Claude (Anthropic)
[GitHub](https://github.com/YB-LIM) · [LinkedIn](https://www.linkedin.com/in/lyb0684/) ·
[ResearchGate](https://www.researchgate.net/profile/Lim-Youngbin?ev=hdr_xprf)

Link icons: GitHub and ResearchGate from [Simple Icons](https://simpleicons.org) (CC0 1.0);
LinkedIn from [Font Awesome Free](https://fontawesome.com) (CC BY 4.0), shown in grey.
`tools/make_link_icons.py` regenerates them.

## Building from source

Requirements: [Rust](https://rustup.rs) 1.88 or newer.

```powershell
cargo build --release
# -> target\release\inplite.exe
```

Installer (optional): install [NSIS](https://nsis.sourceforge.io), copy the exe to
`dist\inplite.exe`, then

```powershell
makensis installer\inplite.nsi
# -> dist\InpLite-1.0.0-setup.exe
```

`build-windows-from-linux.sh` shows how the released binaries were cross-compiled on Linux
(MinGW + `-Zbuild-std`).

### Keyword schema

`assets/keywords.json` was generated from the Abaqus 2025 Keywords Guide PDF:

```bash
pip install pymupdf
python tools/extract_schema.py Abaqus2025_KEYWORDS.pdf schema_full.json
python tools/compact_schema.py schema_full.json assets/keywords.json
```

Re-run these for another Abaqus version. The schema contains keyword/parameter names and short
excerpts of the guide for hover help; check with Dassault Systèmes before distributing it publicly.

### Source layout

| file | |
|---|---|
| `load.rs` | memory-mapped scanner, skeleton text + bulk blocks |
| `store.rs` | mapped sources, bulk block store, placeholder format |
| `doc.rs` | document model, safe save, export without mesh, include writer |
| `syntax.rs` | lexer, highlighter, instant checks |
| `analysis.rs` | block structure, outline, definitions/references, model checks |
| `mesh.rs` | node/element index and mesh checks |
| `diff.rs`, `diffview.rs` | keyword-aware compare |
| `codeview.rs` | virtualised editor widget: input, selection, undo, visible-line layout, gutter |
| `editor.rs` | completion, hover, navigation, find, right-click menu |
| `outline.rs`, `viewer.rs`, `panels.rs`, `app.rs` | UI |
| `schema.rs` | keyword schema, element node counts |
| `shots.rs` | screenshot/self-test automation (feature `shots`, development only) |

Tests: `cargo test` (unit tests incl. byte-exact save round trips and include split/inline).
