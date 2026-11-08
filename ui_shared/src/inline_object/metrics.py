"""Regenerate the ink-free inline-object metric fonts. Requires fontTools.

Two faces, because cosmic-text takes a glyph's ascent and descent from its
FONT (scaled by the span's font size), and the two glyphs need opposite
things:

* `Smudgy Inline Object` reserves one em for U+FFFC; terminal layout scales
  that advance to an embedded widget's measured width and sets its line
  height independently. Its ascent and descent are ZERO so that the glyph,
  sized by the widget's width, never moves the text baseline of its row.

  The same face maps U+0001 to a zero-advance glyph: the marker of an anchor
  (an object that reserves no cells), set at the text's own size with no line
  height of its own, so its box in the paragraph is its visual line's and the
  face's zero ascent and descent leave the row alone. A C0 control is bidi
  class BN, so it takes the level of the text around it and never parts a
  right-to-left run (a private-use letter is strong LTR and would), and
  line-break class CM, so it attaches to the character before it and adds no
  break opportunity: it travels with the word after it exactly as a letter
  would. Unlike U+200B it is not default-ignorable, so the shaper keeps its
  glyph instead of hiding it.
* `Smudgy Inline Riser` maps U+E000 (private use: neither whitespace nor
  a default-ignorable, so shaping and wrapping treat it as an ordinary
  zero-width letter) to a zero-advance glyph with ascent 1 em and descent
  0.5 em. Placed right after a tall object's glyph at a font size the
  layout computes, it pushes the row's text baseline down to the object's
  last text row; being zero-width it always fits beside the glyph, so a
  wrap never separates them.

No glyph is painted. This source and the generated assets use the
repository license.
"""
import sys
from pathlib import Path
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).parent


def build(file_name, family, ps_name, glyphs, ascent, descent):
    """`glyphs` maps each code point to (glyph name, advance)."""
    font = FontBuilder(1000, isTTF=True)
    names = [".notdef"] + sorted({name for name, _ in glyphs.values()})
    font.setupGlyphOrder(names)
    font.setupCharacterMap({code_point: name for code_point, (name, _) in glyphs.items()})
    font.setupGlyf({name: TTGlyphPen(None).glyph() for name in names})
    advances = {name: advance for name, advance in glyphs.values()}
    advances[".notdef"] = advances.get("glyph", 0)
    font.setupHorizontalMetrics({name: (advances[name], 0) for name in names})
    font.setupHorizontalHeader(ascent=ascent, descent=-descent)
    font.setupNameTable({
        "familyName": family, "styleName": "Regular",
        "uniqueFontIdentifier": ps_name + "1",
        "fullName": family, "psName": ps_name,
        "version": "Version 1.0",
        "copyright": "Original Smudgy layout metric asset. GPL-3.0-or-later.",
    })
    font.setupOS2(sTypoAscender=ascent, sTypoDescender=-descent,
                  usWinAscent=ascent, usWinDescent=descent)
    font.setupPost()
    font.setupMaxp()
    font.font.recalcTimestamp = False
    font.font["head"].created = font.font["head"].modified = 2082844800
    font.save(OUT / file_name)


build("metrics.ttf", "Smudgy Inline Object", "SmudgyInlineObject",
      {0xFFFC: ("glyph", 1000), 0x0001: ("marker", 0)}, 0, 0)
build("riser.ttf", "Smudgy Inline Riser", "SmudgyInlineRiser", {0xE000: ("glyph", 0)}, 1000, 500)
