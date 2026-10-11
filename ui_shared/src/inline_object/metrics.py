"""Regenerate the original ink-free object metric font. Requires fontTools.

The font reserves one em for U+FFFC; terminal layout scales that advance to
an embedded widget's measured width and sets its line height independently.
No glyph is painted. This source and the generated asset use the repository license.
"""
from pathlib import Path
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

font = FontBuilder(1000, isTTF=True)
names = [".notdef", "object"]
font.setupGlyphOrder(names)
font.setupCharacterMap({0xFFFC: "object"})
font.setupGlyf({name: TTGlyphPen(None).glyph() for name in names})
font.setupHorizontalMetrics({name: (1000, 0) for name in names})
font.setupHorizontalHeader(ascent=500, descent=-500)
font.setupNameTable({
    "familyName": "Smudgy Inline Object", "styleName": "Regular",
    "uniqueFontIdentifier": "SmudgyInlineObject1",
    "fullName": "Smudgy Inline Object", "psName": "SmudgyInlineObject",
    "version": "Version 1.0",
    "copyright": "Original Smudgy layout metric asset. GPL-3.0-or-later.",
})
font.setupOS2(sTypoAscender=500, sTypoDescender=-500, usWinAscent=500, usWinDescent=500)
font.setupPost()
font.setupMaxp()
font.font.recalcTimestamp = False
font.font["head"].created = font.font["head"].modified = 2082844800
font.save(Path(__file__).with_name("metrics.ttf"))
