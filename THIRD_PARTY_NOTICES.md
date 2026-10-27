# Third-party notices

The MIT license in [LICENSE](LICENSE) covers this project's own code and
original assets only. Third-party works keep their own licenses, listed here.
Release tarballs ship this file and LICENSE next to the binary.

## Brand logos

`desktop/app/assets/logos/` holds 46 brand logos as SVG paths from
[Simple Icons](https://simpleicons.org) 16.33.0, released under CC0 1.0
(`LICENSE.simple-icons.md`). CC0 covers the SVG paths only. The brands
themselves, their names and their marks remain the trademarks of their
owners; this project is not affiliated with, endorsed or sponsored by any of
them. Simple Icons' own notice is in `DISCLAIMER.simple-icons.md`. See
"Trademarks" in the [README](README.md).

## Icons

The UI icons come from [Lucide](https://lucide.dev) through `gpui-kit-assets`:
ISC license (Copyright (c) 2026 Lucide Icons and Contributors), with the
icons derived from Feather under the MIT license (Copyright (c) 2013-present
Cole Bemis). The full text ships in the `gpui-kit-assets` crate (`LICENSE-LUCIDE`).

## Fonts

In `desktop/web/fonts/` (the desktop app embeds the same files):

- Noto Sans, Noto Sans Mono and Noto Serif Italic, subset. Copyright 2022 The
  Noto Project Authors. SIL Open Font License 1.1.
- JetBrains Mono NL (Regular, Medium, Bold), subset. Copyright 2020 The
  JetBrains Mono Project Authors. SIL Open Font License 1.1.
- DejaVu Sans, subset to the braille block. Copyright (c) 2003 Bitstream,
  Inc.; DejaVu changes are in the public domain. Bitstream Vera / DejaVu Fonts
  License.

Full texts: `desktop/web/fonts/OFL.txt` and
`desktop/web/fonts/LICENSE-DejaVu.txt`; the per-file summary is
`desktop/web/fonts/LICENSE.txt`.

## Data

In `desktop/core/assets/geo/` (the map and the city lookup):

- Natural Earth 1:110m countries: public domain
  (`LICENSE.natural-earth.md`). Made with Natural Earth.
- A derivative of the GeoNames `cities15000` dump: (c) GeoNames
  (https://www.geonames.org), licensed under
  [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Changes: only
  places with 100,000+ people kept, columns trimmed, coordinates rounded to
  0.01 degrees. Details in `LICENSE.geonames.md`.

## Rust dependencies

Everything comes from crates.io (see `desktop/Cargo.lock`; no git
dependencies). The licenses of the crates in the build, from `cargo metadata`:

- Almost all are permissive: MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib,
  0BSD, Unlicense, CC0-1.0, Unicode-3.0 and CDLA-Permissive-2.0, mostly as
  "MIT OR Apache-2.0". Where a crate offers a choice (for example `r-efi`:
  MIT OR Apache-2.0 OR LGPL-2.1-or-later; `self_cell`: Apache-2.0 OR
  GPL-2.0-only), this project uses it under the permissive option.
- MPL-2.0, used unmodified from crates.io: `dwrote`, `nucleo-matcher`,
  `option-ext` (and `cbindgen`, a build tool). Their source is on crates.io
  and in each crate's repository.
- [GPUI](https://gpui.rs) (the `gpui-pre-*` crates) and
  [gpui-kit](https://gpui-kit.com) (`gpui-kit`, `gpui-kit-assets`) are
  Apache-2.0.
- SQLite, bundled through `libsqlite3-sys`, is in the public domain.
- zstd (`zstd`, `zstd-sys`): BSD-3-Clause.
- FreeType, linked by the desktop app through GPUI's font stack
  (`freetype-sys`): used under the FreeType License. Portions of this software
  are copyright © The FreeType Project (www.freetype.org). All rights
  reserved.

To list every crate and its license:

```bash
cd desktop && cargo metadata --format-version 1 --locked \
  | python3 -c "import json,sys; [print(p['name'], p['version'], p['license']) for p in json.load(sys.stdin)['packages']]"
```
