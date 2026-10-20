# Vendored libraries

Plain ES modules, loaded through the import map in `../index.html`. No build
step and no runtime network access. Fetched once with `npm pack` on
2026-10-20; only the files below were copied, with the trailing
`//# sourceMappingURL=` line removed (the maps are not shipped).

| File | Package | Version | Source file in the package | License |
|---|---|---|---|---|
| `preact.mjs` | [preact](https://github.com/preactjs/preact) | 11.0.1 | `dist/preact.mjs` | MIT, `licenses/preact.LICENSE` |
| `hooks.mjs` | preact/hooks | 11.0.1 | `hooks/dist/hooks.mjs` | MIT, `licenses/preact.LICENSE` |
| `htm.mjs` | [htm](https://github.com/developit/htm) | 3.1.1 | `dist/htm.module.js` | Apache-2.0, `licenses/htm.LICENSE` |
| `marked.mjs` | [marked](https://github.com/markedjs/marked) | 18.1.1 | `lib/marked.esm.js` | MIT (+ original Markdown BSD notice), `licenses/marked.LICENSE` |

Total about 64 KB uncompressed. Syntax highlighting is our own small
tokenizer (`../src/highlight.js`), so no highlighter is vendored.

SHA-256 after stripping the source-map line:

```
17598141bfddaf9bc74873b88e19e4cfee3605078752a3d693d15a57019865d4  preact.mjs
d8c209c75f5b0eb60b5d89dc050e8eb8f9cabb484eaf2e51ea144700142a1f64  hooks.mjs
ab33dd3f38059b9be4d5f5350128eefb2356639c4e0bbe9d9e8b3ba75847e9e4  htm.mjs
3131eb6f6cc1711da76e7847e11a2f07d0957f5ecf32a3ceb58e98d72859f5f3  marked.mjs
```

To update: `npm pack preact htm marked`, unpack, copy the same files, strip
the source-map line, update this table and the hashes.
