#!/usr/bin/env node
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
test('the JavaScript embedded by the actual Rust HTML loader parses as one classic browser script',()=>{
  const root=new URL('../src/',import.meta.url),loader=readFileSync(new URL('gui.rs',root),'utf8');
  const from=loader.search(/\.replace\(\s*"\/\/ DESKTOP_SCRIPT"/),tail=loader.slice(from),relative=tail.search(/\.replace\(\s*"\/\* SCHEDULED_STYLES \*\/"/),to=from+relative;
  assert.ok(from>=0&&to>from,'locate the production resource list');
  const paths=[...loader.slice(from,to).matchAll(/include_str!\("([^"]+\.js)"\)/g)].map(match=>match[1]);
  assert.ok(paths.includes('gui_remote.js')&&paths.includes('gui_images.js')&&paths.includes('gui_models.js')&&paths.includes('gui_documents.js')&&paths.includes('gui_document_tools.js')&&paths.includes('gui_native_windows.js')&&paths.includes('gui_native_controls.js')&&paths.includes('gui_work_local.js'));
  assert.equal(new Set(paths).size,paths.length,'a script must not be embedded twice');
  const html=readFileSync(new URL('gui_app.html',root),'utf8').replace('// DESKTOP_SCRIPT',()=>paths.map(path=>readFileSync(new URL(path,root),'utf8')).join('\n')).replace('// MARKDOWN_VENDOR',()=>readFileSync(new URL('vendor/markdown.js',root),'utf8')).replace('// EDITOR_VENDOR',()=>readFileSync(new URL('vendor/editor.js',root),'utf8'));
  const scripts=[...html.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)].map(match=>match[1]);
  assert.ok(scripts.length);new vm.Script(scripts.join('\n'),{filename:'Rust embedded desktop UI'});
});
