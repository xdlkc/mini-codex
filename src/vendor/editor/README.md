# Offline Work editor vendor

`../editor.js` is a standalone browser IIFE. It installs `window.EditorBridge`. `../editor.css` contains editor styles plus scoped KaTeX CSS and embedded WOFF2 fonts. Load both resources locally. No React, package loader, CDN, account, provider, persistence service, or network connection is required at runtime.

The pinned runtime uses Tiptap 3.32.0, ProseMirror, the official non-React DragHandle, official MarkdownManager/Marked 17.0.6, and official mathematics nodes with KaTeX 0.18.10. The math version meets the extension's peer dependency. Its CSS selectors and font family names are isolated under `.mini-rich-editor`, so the existing message renderer can retain its separate KaTeX version.

`schema.json` is generated from the actual configured ProseMirror schema. `manifest.json` records each bundled npm package, exact version, official registry URL, tarball integrity, license hashes, source hashes, output hashes, and font hashes. `licenses/` retains package notices. `../LICENSE-editor-dependencies.txt` combines those notices for the existing desktop/agent packaging scripts.

## Host contract

```js
const controller = EditorBridge.create({
  element: hostElement,
  json: documentJSON,             // Or markdown: sourceMarkdown
  editable: true,
  ariaLabel: 'Document content',
  dragHandle: true,              // Optional; false/omitted adds no drag handle
  resolveImage: async attachmentId => verifiedLocalImageDataURL,
  onUpdate: ({ json, editor, transaction, bridge }) => saveJSON(json),
  onSelectionUpdate: ({ editor, selection, transaction, bridge }) => updateSelection(selection),
  onAttachmentClick: ({ attachmentId, kind, node, position, open }) => {
    // Single click selects the actual ProseMirror node and sets open:false.
    // Double click sets open:true. Open the local original only in that case.
  },
  onFilesDrop: files => importFilesAsLocalAttachments(files),
  onWorkBlockAction: action => handleLocalWorkBlock(action),
  onError: error => showEditorError(error),
})
```

`json` takes priority when supplied. Do not supply an HTML string. JSON boundaries reject unknown nodes, attributes, marks, bad content structure, invalid attachment IDs, and unsafe link protocols. `schemaVersion` is `2` (the host still reads v1). Image/File attachment IDs are required even though the underlying ProseMirror attribute has a `null` default.

The controller exposes:

- `editor`: the real Tiptap instance for trusted command chains, selection, Find, decoration plugins, and table operations.
- `commands`: the current Tiptap command collection.
- `getJSON()`, `getMarkdown()`, `exportMarkdown()`, `getImportReport()`.
- `setJSON(json, {emitUpdate:true})`, `setMarkdown(source, {emitUpdate:true})`.
- `focus(position?)`, `isActive(name, attrs?)`, `canUndo()`, `canRedo()`.
- `mount(element)`, `unmount()`, `destroy()`, and read-only `mounted`/`destroyed` flags.

Updates pass canonical JSON. `selection` is `{from,to,empty}` and uses ProseMirror UTF-16 positions. For backend quote checks, use `editor.state.doc.textBetween(from,to,'\n','\uFFFC')`. Do not use DOM text or JavaScript code-point indices. A hard break and each non-text leaf use the same placeholder in that quote contract.

Keep one controller per document. `unmount()` saves the full EditorState and plugin history; `mount()` restores it. Removing/reinserting the same host element also keeps state and Undo. `destroy()` is permanent. The upstream `editor.isDestroyed` getter is also true while unmounted; use the controller's `destroyed` flag to distinguish permanent destruction.

Additional commands are `insertAttachmentImage(attrs)`, `insertFileAttachment(attrs)`, `setTableLayout('normal'|'flexible'|'full')`, and `fitTable()`. Fit clears stored column widths only in the selected table. Standard Tiptap commands cover marks, headings, lists, nested task lists, history, table rows/columns/merge/split, and math. Pass a DragHandle options object instead of `true` to customize its `render`, positioning, and node-change hooks. `EditorBridge.DragHandle` also exposes the official extension.

The raw editor and command chains are for trusted application code. Untrusted data must enter through `create`/`setJSON`/`setMarkdown`/`importMarkdown`; never pass it to raw `editor.commands.setContent` as HTML. The transaction filter adds a second JSON check. HTML clipboard/drop content is stripped before parsing and pasted as literal plain text. A files drop calls the host import hook and does not load paths or upload data itself.

## Attachments and formulas

Image attrs are `attachmentId,name,alt,title,width,height`. File attrs are `attachmentId,name,mimeType,size`. There is no `src`, arbitrary HTML, event handler, or remote URL attribute. An image NodeView calls `resolveImage(id)` and accepts only local PNG/JPEG/WebP base64 data URLs. It rejects remote URLs/SVG and ignores results after node destruction/replacement. The host must verify bytes and account scope before returning a data URL. The controller never persists image bytes in JSON. File previews require the host's explicit user-action callback.

`inlineMath` and `blockMath` store only `attrs.latex`. The official mathematics nodes render with `trust:false`, bounded expansion, and size limits. `$...$`, `$$...$$`, `\(...\)`, and `\[...\]` are supported by the importer. Math is displayed, not executed as JavaScript.

## Local Work blocks

`workBlock` is an atomic draggable block with `kind/title/text/targetId/html`. Kinds are `prompt`, `task`, `instructions`, `highlight`, `page`, `chat`, and `visualize`. The host handles Edit/Open/Chat actions; the editor does not send requests or run local tools. Markdown uses a `mcodex-block` JSON fence. Visualization NodeViews use an opaque `allow-scripts` iframe and a restrictive CSP from `EditorBridge.sandboxHTML`; no network, same-origin, nested frames, forms or popups are granted. Native hosts must also enforce their own frame and IPC boundary.

## Markdown conversion and limits

`EditorBridge.importMarkdown(source)` returns:

```js
{schemaVersion:2, json, sourceMarkdown, warnings:[{code,message}], lossless:true|false}
```

The importer checks token types before conversion. Raw HTML, unsafe link targets, remote images, footnotes, unsupported structure, and failed semantic round-trips return `lossless:false`. Their exact source remains in `sourceMarkdown` and a literal text fallback. Do not automatically migrate such a document into rich text. Keep the source editor until the user chooses a supported conversion. Initial creation does not fire `onUpdate`. For an unchanged fallback, `getMarkdown()` returns the exact original source.

`EditorBridge.exportMarkdown(json)` and `controller.exportMarkdown()` return `{markdown,warnings,lossless}`. `getMarkdown()` returns only the string. Rich JSON is canonical. Merged cells, column widths, non-default table layouts, image metadata, and file MIME/size cannot all fit in Markdown. Export reports that limit instead of silently claiming a faithful round-trip. Underline uses Tiptap's `++text++` extension, not plain CommonMark. Tables/tasks use GFM; formulas and local attachment references are explicit extensions.

Local image syntax is `![alt](attachment:att_32hex)`. A file block uses `[name](attachment:att_32hex)` on its own block. External images stay as source text and never issue a request. Markdown link titles are retained in source mode because the frozen link schema stores only `href`. This avoids silently losing titles during automatic migration.

The official Markdown package is still documented as Beta. Round-trip tests cover the supported subset; this is not a claim that every Markdown or rich-text construct is lossless. Collaboration, arbitrary HTML paste, remote images, document persistence, AI changes, comments, and UI menus remain host features.

## Rebuild and test

Use Node 22 and npm 10 or newer. Build in a temporary directory to keep `node_modules` out of source archives. All dependency versions are locked; installation scripts are disabled. A populated npm cache permits `--offline`.

```sh
EDITOR_SOURCE="$PWD/src/vendor/editor"
EDITOR_OUTPUT="$PWD/src/vendor"
EDITOR_BUILD_DIR="$(mktemp -d)"
cp -R "$EDITOR_SOURCE/." "$EDITOR_BUILD_DIR/"
cd "$EDITOR_BUILD_DIR"
npm ci --ignore-scripts --registry=https://registry.npmjs.org
node build.mjs --out-dir "$EDITOR_OUTPUT"
EDITOR_VENDOR_OUTPUT="$EDITOR_OUTPUT" npm test
cd "$EDITOR_OUTPUT"
rm -rf "$EDITOR_BUILD_DIR"
```

The build writes the two runtime assets, aggregated license, and `editor/{schema.json,manifest.json,licenses/}`. It never edits Rust, Cargo, frontend controllers, or credentials. The committed source and lockfile are enough to reproduce the bundle. Actual tests cover strict schema/URL checks, Markdown tasks/tables/code/math, exact unsupported-source retention, real commands, two independent history stacks across unmount/mount/detach, attachment selection/delete/Undo, late image results, HTML paste, optional DragHandle, and the shipped IIFE without a runtime loader.

Official references: [Editor API](https://tiptap.dev/docs/editor/api/editor), [StarterKit](https://tiptap.dev/docs/editor/extensions/functionality/starterkit), [Markdown and its limits](https://tiptap.dev/docs/editor/markdown), [Mathematics](https://tiptap.dev/docs/editor/extensions/nodes/mathematics), [DragHandle](https://tiptap.dev/docs/editor/extensions/functionality/drag-handle).
