# Offline Markdown dependencies

`markdown.js` contains official browser builds from the npm registry. The pinned versions, registry tarball URLs, SHA-512 tarball integrity values, and output SHA-256 hashes are recorded in `manifest.json`.

- markdown-it 15.0.3: CommonMark parsing, GFM tables, strikethrough, and linkification. MIT.
- KaTeX 0.19.0: TeX rendered to HTML plus accessible MathML. MIT.
- highlight.js 11.12.0: the official common-language browser build, including Rust, JavaScript, TypeScript, Python, shell, SQL, Swift, and 28 other languages. BSD-3-Clause.

The distributions are concatenated without changing their code. Source-map URL comments are removed. `markdown.css` is the official KaTeX stylesheet with its WOFF2 font files embedded as data URLs. WOFF and TTF fallbacks are removed. Supported WebKit and Chromium clients can use WOFF2. No CDN or font server is required at runtime.

The host injects the JS at `// MARKDOWN_VENDOR` and the CSS at `/* MARKDOWN_VENDOR_STYLES */` in `gui_app.html`. The integration code and safe renderer rules live in that HTML file. Do not load another parser or math renderer from a CDN.

Raw HTML is disabled. Link and image renderers enforce the product URL allowlist. Local file links use delegated data attributes. KaTeX runs with `trust:false`, fresh macros for each expression, and expansion/size limits. Unsupported or unfinished TeX remains visible as escaped text. Unknown code languages and code blocks over 64 KiB remain readable without syntax highlighting.

The product adds disabled GFM task checkboxes and TeX delimiters (`$`, `$$`, `\(`, `\[`). These extensions intentionally differ from plain CommonMark. Footnotes, Mermaid, executable code cells, and trusted HTML extensions are not enabled.

To update, obtain the pinned packages from the official npm registry, verify their tarball integrity before extracting files, retain the licenses, rebuild the two assets, and update the manifest hashes. The production behavioral checks run with `node scripts/test-markdown.mjs` and `node scripts/test-rendering.mjs`.
