# Interface fonts (ADR-0077)

The desktop's interface text: proportional, anti-aliased, with Thai.

| File | Font | From |
|---|---|---|
| `NotoSans-Regular.ttf`, `NotoSans-Bold.ttf` | Noto Sans, Latin subset (see below) | [notofonts/latin-greek-cyrillic](https://github.com/notofonts/latin-greek-cyrillic) |
| `NotoSansThai-Regular.ttf`, `NotoSansThai-Bold.ttf` | Noto Sans Thai, hinted | [notofonts/thai](https://github.com/notofonts/thai) |

Downloaded from
[notofonts.github.io](https://github.com/notofonts/notofonts.github.io/tree/main/fonts)
on 2026-10-06. The Thai fonts are unchanged.

## The Latin subset (ADR-0084)

Each program that draws text carries these fonts, and the boot archive
stays in memory. Full Noto Sans (620 KB each, mostly Greek, Cyrillic and
Vietnamese) became a subset of what an English and Thai interface uses
(47 KB each), with fontTools 4.66:

```
python -m fontTools.subset NotoSans-Regular.ttf --no-hinting   --layout-features=kern --output-file=NotoSans-Regular.ttf   --unicodes=U+0020-007E,U+00A0-017F,U+2000-206F,U+20A0-20CF,U+2100-214F,U+2190-21FF,U+2200-22FF,U+FFFD
```

(and the same for `NotoSans-Bold.ttf`). The ranges are Basic Latin,
Latin-1, Latin Extended-A, punctuation, currency signs, letterlike
symbols, arrows, mathematical operators and the replacement character.
Hinting is dropped: the renderer (`ab_glyph`) does not use it. The OFL
declares no Reserved Font Name for Noto, so the subset keeps its name.

## License

All four are licensed under the SIL Open Font License, Version 1.1
([OFL.txt](OFL.txt)):

- Copyright 2022 The Noto Project Authors
  (https://github.com/notofonts/latin-greek-cyrillic)
- Copyright 2022 The Noto Project Authors (https://github.com/notofonts/thai)

The OFL allows bundling the fonts with software, as here (built into the
display service), provided the license and copyright notices go with them.
The fonts are not sold on their own and keep their reserved names.
