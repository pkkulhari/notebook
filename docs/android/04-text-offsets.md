# 4. UTF-16 text positions

**Status:** Done
**Needs:** step 3
**Behaviour change:** none on desktop

## Why

Every position in the app is a count of something, and the platforms count different things:

| Where | Counts |
| --- | --- |
| GTK `TextIter` offsets, cursor in `Preferences` | characters (Unicode scalar values) |
| Loro `insert`/`delete`, and text event deltas | characters |
| `markdown::parse` spans, hidden ranges, blocks, markers and links | characters (the `offsets` table in `crates/core/src/markdown.rs:45`) |
| `markdown::list_enter`'s `marker` | characters |
| Android `Editable`, `TextWatcher`, selection, `Layout` | **UTF-16 units** |

Any character outside the Basic Multilingual Plane, such as 🌿, counts as 1 character but 2 UTF-16 units. If Android used character positions, every edit after an emoji would land one place off. Core should do all conversion, so Kotlin never counts characters.

## Design

Add a unit type in `model.rs`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Units { Chars, Utf16 }
```

### Markdown

- `markdown::parse(source, units)`: when building the `offsets` table, advance by `ch.len_utf16()` instead of 1 in `Utf16` mode. The end-of-document adjustment uses the same unit instead of `source.chars().count()`.
- `list_enter(line)` needs no unit: everything before `marker` (indent, bullet or number, task box) is ASCII, so it counts the same in both.
- `summary()` is unaffected, because it works on strings.
- GTK passes `Units::Chars`.

### Editor

`Drafts::new(units)` stores the unit, and every `Draft` uses it.

- **Typing:** `insert` uses `LoroText::insert_utf16` and `delete` uses `delete_utf16` in `Utf16` mode. `replace` compares against `text.slice` after converting positions with `LoroText::convert_pos(…, PosType::Utf16, PosType::Unicode)`.
- **Incoming changes:** event deltas count characters, and a `Delete` can't be converted afterwards because its characters are gone. So `undo`, `redo` and `import` first take `let before = text.to_string()`. They then walk each delta over `before` in a single pass, adding `len_utf16()` for retained and deleted characters to build UTF-16 `TextEdit`s. For multiple deltas, apply each one to the working copy before walking the next. Imports and undos are rare, so an O(n) pass over the text is fine.
- Loro can't emit UTF-16 event deltas here: its event unit (`PosType::Event`) is UTF-16 only when built with the `wasm` feature. So the manual walk stays.

### Preferences

`Preferences.cursor` is stored per device and never synced, so each platform stores its own unit. Android clamps it to the text length when restoring.

## Tests

1. `parse` in both units for `"# नमस्ते 🌿\n\n**hello _世界_** and `λ` 🌿🌿 ~~x~~"`. Each UTF-16 range, when decoded, gives the same substring as the matching character range.
2. A property test with random strings drawn from ASCII, CJK, combining marks and emoji checks the same thing for all span kinds, hidden ranges and link ranges.
3. `list_enter("- [ ] 🌿 x")` puts `marker` after the checkbox, which is the same position in both units.
4. Editor in `Utf16` mode: insert after an emoji, delete across an emoji, and import a remote edit made after an emoji. Applying the returned edits to a Java-style UTF-16 buffer (`Vec<u16>`) gives `draft.text()`.
5. Undo after an emoji places the cursor at the right UTF-16 position.
6. The existing `markdown` tests pass with `Units::Chars`.

## Notes

- **`Units`** is in `model.rs`, with `width(ch)` and `count(text)`. GTK passes `Units::Chars` to `markdown::parse` and `Drafts::new`.
- **Typing in UTF-16** converts each position with `LoroText::convert_pos`, which is O(log n). A position inside a surrogate pair returns `OutOfSync`, found by converting it back and comparing. `replace` converts first, then trims the common prefix and suffix in characters.
- **Incoming changes:** in UTF-16 mode, `import`, `undo` and `redo` copy the text before the change, and measure each delta against that copy. In `Chars` mode there is no copy, so GTK pays nothing.
- **Tests:**
  - The parser property test checks 500 random Markdown strings, comparing every span, hidden range, block, list marker and link between the two units.
  - The editor property test runs in both units: 20 seeds × 40 rounds of concurrent edits, with `insert`, `delete`, `replace`, `undo` and `redo`. The returned edits are applied to a `Vec<u16>` in UTF-16 mode.
  - Both were mutation-checked. Making UTF-16 count characters in `parse`, or in the delta walk, fails them.
- **Shared test generator:** `Rng` is now in `lib.rs` (test builds only), shared by the Markdown and editor tests.
- **Undo steps:** `UndoManager::record_new_checkpoint` doesn't split edits made within the 500 ms merge interval. A test that needs separate undo steps sets the interval to 0.
