# Terminal renderer

`@xterm/xterm` 6.0.0 and `@xterm/addon-fit` 0.11.0 come from the official npm registry. Versions and integrity hashes are pinned in `package-lock.json`. The MIT licenses are copied to the parent vendor directory and included in the macOS package.

Run `node src/vendor/terminal/vendor.mjs` to install locked dependencies and copy the runtime assets. The JavaScript assets only omit their source map comment. All other bytes are retained, including Unicode separators. `node --test scripts/test-embedded-ui.mjs` verifies the complete browser script.
