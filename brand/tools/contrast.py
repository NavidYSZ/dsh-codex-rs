#!/usr/bin/env python3
"""WCAG 2.x contrast ratios for brand/colors.json.

    python3 brand/tools/contrast.py            # table for both themes
    python3 brand/tools/contrast.py '#4466F2' '#FFFFFF'   # one pair

Text roles (`text`, `textMuted`) must reach 4.5:1 on `bg`, `surface` and
`surfaceRaised`; `accent` must reach 4.5:1 on the same surfaces (links) and
the text on `accent` (white in light, `bg` in dark) 4.5:1 (buttons); status colours are checked at 3:1
(graphics, status dots) and listed with their value for text use.
"""
import json, sys, os

def lin(c):
    c /= 255
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4

def lum(hexstr):
    h = hexstr.lstrip('#')
    r, g, b = (int(h[i:i + 2], 16) for i in (0, 2, 4))
    return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)

def ratio(a, b):
    la, lb = lum(a), lum(b)
    hi, lo = max(la, lb), min(la, lb)
    return (hi + 0.05) / (lo + 0.05)

def main():
    if len(sys.argv) == 3:
        print(f'{ratio(sys.argv[1], sys.argv[2]):.2f}:1')
        return
    path = os.path.join(os.path.dirname(__file__), '..', 'colors.json')
    colors = json.load(open(path))
    ok = True
    for theme in ('light', 'dark'):
        c = colors[theme]
        print(f'\n{theme}')
        surfaces = ('bg', 'surface', 'surfaceRaised')
        for fg, minimum in (('text', 4.5), ('textMuted', 4.5), ('accent', 4.5), ('online', 3), ('warning', 3), ('danger', 3)):
            for bg in surfaces:
                r = ratio(c[fg], c[bg])
                flag = 'ok ' if r >= minimum else 'LOW'
                ok &= r >= minimum
                print(f'  {flag} {fg:<10} on {bg:<14} {c[fg]} / {c[bg]}  {r:5.2f}:1  (min {minimum})')
        # Text on accent: white in light, `bg` in dark (BRAND.md, colour roles).
        on_accent = '#FFFFFF' if theme == 'light' else c['bg']
        r = ratio(on_accent, c['accent'])
        flag = 'ok ' if r >= 4.5 else 'LOW'
        ok &= r >= 4.5
        label = 'white' if theme == 'light' else 'bg'
        print(f'  {flag} {label:<10} on {"accent":<14} {on_accent} / {c["accent"]}  {r:5.2f}:1  (min 4.5)')
        r = ratio(c['text'], c['accentSoft'])
        flag = 'ok ' if r >= 4.5 else 'LOW'
        ok &= r >= 4.5
        print(f'  {flag} {"text":<10} on {"accentSoft":<14} {c["text"]} / {c["accentSoft"]}  {r:5.2f}:1  (min 4.5)')
        r = ratio(c['accent'], c['accentSoft'])
        print(f'  --  {"accent":<10} on {"accentSoft":<14} {c["accent"]} / {c["accentSoft"]}  {r:5.2f}:1  (chip text, informative)')
        r = ratio(c['border'], c['surface'])
        print(f'  --  {"border":<10} on {"surface":<14} {c["border"]} / {c["surface"]}  {r:5.2f}:1  (hairline, informative)')
    print('\nall minimums met' if ok else '\nSOME PAIRS BELOW MINIMUM')
    sys.exit(0 if ok else 1)

if __name__ == '__main__':
    main()
