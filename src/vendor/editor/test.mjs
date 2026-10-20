import assert from 'node:assert/strict';
import test from 'node:test';
import {JSDOM,ResourceLoader} from 'jsdom';
import {create,importMarkdown,exportMarkdown,validateJSON,schema,safeLink} from './bridge.js';
const requests=[];
class NoNetwork extends ResourceLoader{fetch(url){requests.push(url);return null;}}
const dom=new JSDOM('<!doctype html><html><head></head><body></body></html>',{pretendToBeVisual:true,url:'http://127.0.0.1:1/',resources:new NoNetwork()});
for(const name of ['window','document','Node','HTMLElement','Element','HTMLInputElement','HTMLImageElement','ShadowRoot','Document','DOMParser','MutationObserver','getComputedStyle'])Object.defineProperty(globalThis,name,{value:typeof dom.window[name]==='function'&&name==='getComputedStyle'?dom.window[name].bind(dom.window):dom.window[name],configurable:true});
Object.defineProperty(globalThis,'navigator',{value:dom.window.navigator,configurable:true});
globalThis.requestAnimationFrame=callback=>setTimeout(callback,0);globalThis.cancelAnimationFrame=clearTimeout;globalThis.innerHeight=1000;globalThis.innerWidth=1200;
const rect=()=>({top:0,bottom:0,left:0,right:0,width:0,height:0,x:0,y:0,toJSON(){return {};}});
dom.window.Range.prototype.getBoundingClientRect=rect;dom.window.Range.prototype.getClientRects=()=>[];dom.window.HTMLElement.prototype.scrollIntoView=()=>{};
globalThis.fetch=()=>{throw new Error('Editor must not make network requests');};
const text=editor=>editor.editor.state.doc.textContent;
const paragraph=value=>({type:'doc',content:[{type:'paragraph',content:value?[{type:'text',text:value}]:undefined}]});
const host=()=>{const element=document.createElement('div');document.body.append(element);return element;};
const settle=async()=>{for(let i=0;i<10;i++)await Promise.resolve();await new Promise(resolve=>setTimeout(resolve,5));};
const ID='att_'+'a'.repeat(32);

test('actual schema matches the frozen contract, strict JSON rejects unknown attrs/structure/URLs',()=>{
  assert.deepEqual(Object.keys(schema.marks).sort(),['bold','code','italic','link','strike','underline']);assert.deepEqual(Object.keys(schema.marks.link.attrs),['href']);
  for(const node of [{type:'iframe',attrs:{src:'https://forbidden.invalid'}},{type:'image',attrs:{attachmentId:ID,src:'https://forbidden.invalid/a'}},{type:'paragraph',attrs:{style:'background:url(https://forbidden.invalid)'}}])assert.throws(()=>validateJSON({type:'doc',content:[node]}));
  const unsafe=paragraph('bad');unsafe.content[0].content[0].marks=[{type:'link',attrs:{href:'javascript:alert(1)'}}];assert.throws(()=>validateJSON(unsafe));
  const mixed=paragraph('mixed');mixed.content[0].content[0].marks=[{type:'code'},{type:'bold'}];assert.throws(()=>validateJSON(mixed));
  assert.throws(()=>validateJSON({type:'doc',content:[{type:'taskItem',attrs:{checked:true}}]}));
  const tooWide={type:'doc',content:[{type:'table',content:[{type:'tableRow',content:[{type:'tableCell',attrs:{colspan:1001},content:[{type:'paragraph'}]}]}]}]};assert.throws(()=>validateJSON(tooWide));const longLabel={type:'doc',content:[{type:'image',attrs:{attachmentId:ID,name:'中'.repeat(400)}}]};assert.throws(()=>validateJSON(longLabel));
  const intact=create({element:host(),json:paragraph('Keep me')});assert.throws(()=>intact.setJSON(longLabel));assert.equal(text(intact),'Keep me');intact.destroy();
  assert.equal(safeLink('https://example.com/a'),true);assert.equal(safeLink('mailto:user@example.com'),true);for(const url of ['javascript:alert(1)','data:text/html,hello','//example.com','https://example.com/\nx','https://user:secret@example.com','mailto:'])assert.equal(safeLink(url),false);
});

test('real Markdown import/export retains headings, nested lists, GFM tasks/tables, marks, code and math',()=>{
  const input='# Title\n\n**bold** *italic* ~~strike~~ ++underline++ `code` [link](https://example.com)\n\n- outer\n  - inner\n\n- [x] done\n- [ ] open\n\n| a | b |\n| :--- | ---: |\n| x | y |\n\n```rust\nlet x = "<script>";\n```\n\nInline $x^2$ and \\(y+1\\).\n\n\\[z^2\\]\n';
  const report=importMarkdown(input);assert.equal(report.lossless,true,JSON.stringify(report.warnings));const json=JSON.stringify(report.json);for(const type of ['heading','bulletList','taskList','table','codeBlock','inlineMath','blockMath'])assert.ok(json.includes('"type":"'+type+'"'),type);const exported=exportMarkdown(report.json);assert.equal(exported.lossless,true);assert.deepEqual(importMarkdown(exported.markdown).json,report.json);
  assert.equal(importMarkdown('').lossless,true);
});

test('raw HTML, unsafe links, remote images and unsupported footnotes keep exact source with warnings',()=>{
  for(const source of ['<script>alert(1)</script>\n\n<img src="https://forbidden.invalid/a" onerror="alert(2)">','[run](javascript:alert)','![remote](https://forbidden.invalid/image.png)','A[^1]\n\n[^1]: private footnote']){
    const report=importMarkdown(source);assert.equal(report.lossless,false);assert.ok(report.warnings.length);assert.equal(report.sourceMarkdown,source);assert.equal(report.json.content[0].content[0].text,source);
    const element=host(),editor=create({element,markdown:source});assert.equal(editor.getMarkdown(),source);assert.equal(element.querySelector('script,img,iframe'),null);editor.destroy();element.remove();
  }
  assert.deepEqual(requests,[]);
});

test('official history survives unmount/mount, detached DOM, and independent document switches',async()=>{
  const aHost=host(),bHost=host(),a=create({element:aHost,json:paragraph('A')}),b=create({element:bHost,json:paragraph('B')});
  a.commands.insertContent('1');b.commands.insertContent('2');assert.equal(text(a),'1A');assert.equal(text(b),'2B');assert.equal(a.canUndo(),true);
  a.unmount();assert.equal(text(a),'1A');a.mount(aHost);assert.equal(a.canUndo(),true);assert.equal(a.commands.undo(),true);assert.equal(text(a),'A');assert.equal(text(b),'2B');assert.equal(a.canRedo(),true);a.commands.redo();assert.equal(text(a),'1A');
  aHost.remove();document.body.append(aHost);assert.equal(a.canUndo(),true);a.commands.undo();assert.equal(text(a),'A');assert.equal(text(b),'2B');b.commands.undo();assert.equal(text(b),'B');a.destroy();b.destroy();aHost.remove();bHost.remove();await settle();
});

test('actual formatting, task list, table layout/fit, attachment and math commands modify canonical JSON',()=>{
  const element=host(),updates=[],editor=create({element,json:paragraph('Example'),onUpdate:event=>updates.push(event.json)});
  editor.commands.setTextSelection({from:1,to:8});editor.commands.toggleBold();assert.equal(editor.getJSON().content[0].content[0].marks[0].type,'bold');editor.commands.toggleHeading({level:2});assert.equal(editor.getJSON().content[0].type,'heading');
  editor.setJSON(paragraph('task'),{emitUpdate:false});editor.commands.toggleTaskList();assert.equal(editor.getJSON().content[0].type,'taskList');
  editor.setJSON(paragraph(''),{emitUpdate:false});editor.commands.insertTable({rows:2,cols:2,withHeaderRow:true});editor.commands.setTableLayout('full');const table=editor.getJSON().content.find(node=>node.type==='table');assert.equal(table.attrs.layout,'full');assert.equal(editor.commands.fitTable(),true);
  editor.setJSON({type:'doc',content:[{type:'image',attrs:{attachmentId:ID,name:'local image',alt:'example',width:300}},{type:'fileAttachment',attrs:{attachmentId:ID,name:'file.pdf',mimeType:'application/pdf',size:10}},{type:'paragraph',content:[{type:'inlineMath',attrs:{latex:'x^2'}}]},{type:'blockMath',attrs:{latex:'\\frac{a}{b}'}}]},{emitUpdate:false});const json=editor.getJSON();assert.ok(!JSON.stringify(json).includes('src'));assert.ok(element.querySelector('.katex'));assert.equal(element.querySelector('img').hasAttribute('src'),false);assert.equal(editor.exportMarkdown().lossless,false);assert.ok(updates.length>=3);editor.destroy();element.remove();
});

test('local attachment Markdown syntax distinguishes image and file and never stores image bytes',()=>{
  const image=importMarkdown(`![alt](attachment:${ID})`);assert.equal(image.lossless,true);assert.equal(image.json.content[0].type,'image');assert.equal(image.json.content[0].attrs.attachmentId,ID);
  const file=importMarkdown(`[report.pdf](attachment:${ID})`);assert.equal(file.lossless,true);assert.equal(file.json.content[0].type,'fileAttachment');assert.equal(file.json.content[0].attrs.name,'report.pdf');assert.deepEqual(importMarkdown(exportMarkdown(image.json).markdown).json,image.json);
});

test('async image resolver refuses remote/SVG URLs and late results after destruction or node replacement',async()=>{
  const pending=[];const errors=[];const element=host();let resolve;const result=new Promise(yes=>resolve=yes);const editor=create({element,json:{type:'doc',content:[{type:'image',attrs:{attachmentId:ID}}]},resolveImage:id=>{pending.push(id);return result;},onError:error=>errors.push(error)});await settle();assert.deepEqual(pending,[ID]);const image=element.querySelector('img');editor.destroy();resolve('data:image/png;base64,aGVsbG8=');await settle();assert.equal(image.hasAttribute('src'),false);element.remove();
  for(const url of ['https://forbidden.invalid/x','data:image/svg+xml;base64,PHN2Zz4=']){const root=host(),instance=create({element:root,json:{type:'doc',content:[{type:'image',attrs:{attachmentId:ID}}]},resolveImage:()=>url,onError:error=>errors.push(error)});await settle();assert.equal(root.querySelector('img').hasAttribute('src'),false);instance.destroy();root.remove();}assert.equal(errors.length,2);const live=host(),valid=create({element:live,json:{type:'doc',content:[{type:'image',attrs:{attachmentId:ID}}]},resolveImage:()=> 'data:image/png;base64,aGVsbG8='});await settle();assert.equal(live.querySelector('img').getAttribute('src'),'data:image/png;base64,aGVsbG8=');assert.ok(!JSON.stringify(valid.getJSON()).includes('data:'));valid.destroy();live.remove();assert.deepEqual(requests,[]);
});

test('paste/drop HTML is stripped before parsing and only literal clipboard text enters the document',()=>{
  const element=host(),editor=create({element,json:paragraph('')});assert.equal(editor.editor.options.editorProps.transformPastedHTML('<img src="https://forbidden.invalid/x">'),'');const event={clipboardData:{types:['text/html','text/plain'],getData:type=>type==='text/plain'?'<img src="https://forbidden.invalid/x">':'<img src=x onerror=alert(1)>'},preventDefault(){this.prevented=true;}};
  assert.equal(editor.editor.options.editorProps.handlePaste(editor.editor.view,event),true);assert.equal(text(editor),'<img src="https://forbidden.invalid/x">');assert.equal(element.querySelector('img'),null);editor.destroy();element.remove();
});

test('optional official non-React DragHandle creates without changing the frozen schema',async()=>{
  const element=host(),editor=create({element,json:paragraph('Drag this block'),dragHandle:true});assert.equal(editor.getJSON().content[0].type,'paragraph');assert.deepEqual(Object.keys(editor.editor.schema.nodes).sort(),Object.keys(schema.nodes).sort());editor.destroy();element.remove();await settle();
});

 test('attachment single click selects the real node; delete/undo restore it; only double click requests Open',()=>{
  const element=host(),events=[],editor=create({element,json:{type:'doc',content:[{type:'image',attrs:{attachmentId:ID,name:'Image'}},{type:'fileAttachment',attrs:{attachmentId:ID,name:'File'}},{type:'paragraph'}]},onAttachmentClick:event=>events.push(event)});
  element.querySelector('.editor-attachment-image').dispatchEvent(new window.MouseEvent('click',{bubbles:true}));assert.equal(editor.editor.state.selection.node.type.name,'image');assert.equal(events.at(-1).open,false);assert.equal(events.at(-1).position,0);editor.commands.deleteSelection();assert.ok(!editor.getJSON().content.some(node=>node.type==='image'));editor.commands.undo();assert.ok(editor.getJSON().content.some(node=>node.type==='image'));
  element.querySelector('.editor-file-attachment').dispatchEvent(new window.MouseEvent('click',{bubbles:true}));assert.equal(editor.editor.state.selection.node.type.name,'fileAttachment');editor.commands.deleteSelection();assert.ok(!editor.getJSON().content.some(node=>node.type==='fileAttachment'));editor.commands.undo();assert.ok(editor.getJSON().content.some(node=>node.type==='fileAttachment'));
  element.querySelector('.editor-file-attachment').dispatchEvent(new window.MouseEvent('dblclick',{bubbles:true}));assert.equal(events.at(-1).open,true);assert.equal(events.at(-1).kind,'fileAttachment');assert.equal(editor.editor.state.selection.node.type.name,'fileAttachment');editor.destroy();element.remove();
 });

test('shipped IIFE exposes EditorBridge and runs real transactions with no module loader/CDN',async()=>{
  const {readFile}=await import('node:fs/promises');const path=await import('node:path');const {fileURLToPath}=await import('node:url');const output=process.env.EDITOR_VENDOR_OUTPUT||path.dirname(path.dirname(fileURLToPath(import.meta.url)));
  const isolated=new JSDOM('<!doctype html><div id="editor"></div>',{pretendToBeVisual:true,runScripts:'outside-only',url:'http://127.0.0.1:1/',resources:new NoNetwork()});isolated.window.TextEncoder=TextEncoder;isolated.window.TextDecoder=TextDecoder;isolated.window.fetch=()=>{throw new Error('Runtime network is forbidden');};isolated.window.eval(await readFile(path.join(output,'editor.js'),'utf8'));
  const api=isolated.window.EditorBridge;assert.equal(api.version,'3.32.0');assert.equal(api.schemaVersion,2);const editor=api.create({element:isolated.window.document.getElementById('editor'),markdown:'# Offline\n\nEditable text'});editor.commands.setTextSelection({from:1,to:8});editor.commands.toggleItalic();assert.equal(editor.getJSON().content[0].content[0].marks[0].type,'italic');assert.equal(editor.canUndo(),true);editor.commands.undo();assert.ok(!editor.getJSON().content[0].content[0].marks);editor.destroy();isolated.window.close();assert.deepEqual(requests,[]);
});
