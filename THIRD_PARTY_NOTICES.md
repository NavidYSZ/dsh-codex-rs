# Third-party notices

## DeepSeek Harness

The Rust crates reproduce tool descriptions and schemas, the system prompt,
runtime-context texts, the session format and the request wire format of
[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 0.2
(commit 687f282e), and recorded sessions of it are used as test fixtures
(`docs/rust/upstream/`, `crates/*/tests/fixtures/`).

```text
MIT License

Copyright (c) 2026 DeepSeek

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Web UI libraries

Preact, htm and marked are vendored in `ui/vendor/`; their licenses are in
`ui/vendor/licenses/` and listed in `ui/vendor/README.md`.

## Node.js runtime

The macOS app bundles Node.js 26.13.0 from https://nodejs.org/. Its full license
and bundled dependency notices are included as `Contents/Resources/NODE-LICENSE.txt`.
The source build verifies the archive checksum pinned in `desktop/fetch-node.py`.
