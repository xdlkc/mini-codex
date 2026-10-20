#!/usr/bin/env node
// Run against real JSDOM + shipped Tiptap IIFE. Install locked vendor dev deps in a temporary directory.
// EDITOR_TEST_NODE_MODULES=/tmp/editor-build/node_modules node --test scripts/test-document-tools.mjs
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import path from 'node:path';
import test from 'node:test';
const require=createRequire(import.meta.url);let JSDOM;
try{({JSDOM}=process.env.EDITOR_TEST_NODE_MODULES?require(path.join(process.env.EDITOR_TEST_NODE_MODULES,'jsdom')):createRequire(new URL('../src/vendor/editor/package.json',import.meta.url))('jsdom'));}catch{throw new Error('Install src/vendor/editor/package-lock.json in a temporary directory, then set EDITOR_TEST_NODE_MODULES to its node_modules directory.');}
const work=readFileSync(new URL('../src/gui_work_local.js',import.meta.url),'utf8');
const core=readFileSync(new URL('../src/gui_documents.js',import.meta.url),'utf8'),tools=readFileSync(new URL('../src/gui_document_tools.js',import.meta.url),'utf8'),vendor=readFileSync(new URL('../src/vendor/editor.js',import.meta.url),'utf8'),filesSource=readFileSync(new URL('../src/gui_files.js',import.meta.url),'utf8');
function productionFunction(source,name){const start=source.search(new RegExp('^(?:async )?function '+name+'\\(','m'));assert.ok(start>=0,'Missing production helper '+name);const tail=source.slice(start+1),next=tail.search(/^(?:async )?function [\w$]+\(|^const [\w$]+=/m);return source.slice(start,next<0?source.length:start+1+next);}
const helpers=['docsButton','docsText','docsActions','docsCurrent','docsSelection','docsCommand','docsHideFloating','docsPlaceFloating','docsImage','docsClearPrint','docsPreparePrint','docsPrint'].map(name=>productionFunction(core,name)).join('\n');
const fileReader=productionFunction(filesSource,'fileBase64');
const plain=value=>JSON.parse(JSON.stringify(value));
function deferred(){let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};}
const textJSON=text=>({type:'doc',content:[{type:'paragraph',content:text?[{type:'text',text}]:undefined}]});
const ID='att_'+'a'.repeat(32),SECOND='att_'+'b'.repeat(32);
async function settle(){for(let i=0;i<12;i++)await Promise.resolve();await new Promise(resolve=>setTimeout(resolve,25));}
function harness(){
  const dom=new JSDOM('<!doctype html><html><body><div id="modal-backdrop"><h2 id="modal-title"></h2><div id="modal-body"></div></div></body></html>',{pretendToBeVisual:true,runScripts:'outside-only',url:'http://127.0.0.1:1/'}),w=dom.window;
  w.TextEncoder=TextEncoder;w.TextDecoder=TextDecoder;w.scrollBy=()=>{};w.scrollTo=()=>{};const rectangle=()=>({x:100,y:100,left:100,right:600,top:100,bottom:125,width:500,height:25,toJSON(){return {};}});w.Range.prototype.getBoundingClientRect=rectangle;w.Range.prototype.getClientRects=()=>[rectangle()];w.HTMLElement.prototype.getBoundingClientRect=rectangle;w.HTMLElement.prototype.scrollIntoView=function(){this.scrolled=true;};w.document.elementFromPoint=()=>w.document.activeElement;
  const calls=[],errors=[],asked=[],comments=[],opened=[],changes=[],sessions=[];const h={window:w,calls,errors,asked,comments,opened,changes,files:[],respond:null};
  Object.assign(w,{$:id=>w.document.getElementById(id),documentsUi:{current:null,navigation:0,selectionToolbar:null,slashMenu:null,find:{}},desktop:{navigation:0},showError:error=>errors.push(error.message||String(error)),
    rpcCall:()=>{throw new Error('Document tools must use local RPC');},rpcLocalCall:async(method,params={})=>{calls.push({method,params:plain(params)});const custom=h.respond?.(method,params);if(custom!==undefined)return custom;if(method==='files/list')return {data:plain(h.files)};if(method==='files/import'){const attachment={id:'att_'+String(h.files.length+1).padStart(32,'0'),name:params.name,mimeType:params.mimeType||'text/plain',kind:/^image\//.test(params.mimeType||'')?'image':'text',size:Buffer.from(params.dataBase64,'base64').length};h.files.push(attachment);return {attachment:plain(attachment)};}throw new Error('Unexpected local RPC '+method);},
    openCustomModal(title,html){w.document.getElementById('modal-title').textContent=title;w.document.getElementById('modal-body').innerHTML=html;w.document.getElementById('modal-backdrop').classList.add('open');},closeModal(){w.document.getElementById('modal-backdrop').classList.remove('open');},
    docsAskChange:async(session,selection)=>asked.push({session:session.doc.id,selection:plain(selection)}),docsAddComment:async(session,selection)=>comments.push({session:session.doc.id,selection:plain(selection)}),
    openAttachment:async(id,options)=>opened.push({id,options:plain(options)}),openImageEditor:async options=>{h.imageOptions=options;opened.push({imageEditor:plain(options)});},
    docsChanged:session=>{changes.push(session.doc.id);if(w.documentsUi.current===session)w.docsSelectionTools(session);},docsUpdateTools:session=>{if(w.documentsUi.current===session)w.docsSelectionTools(session);},
  });
  w.eval(vendor);w.eval(helpers+'\n'+fileReader+'\n'+tools+'\nwindow.__testBlockChoices=docsBlockChoices;');
  h.open=(text='',json=null)=>{const old=w.documentsUi.current;if(old){old.bridge?.unmount();old.shell.remove();}w.documentsUi.navigation++;
    const shell=w.document.createElement('section');w.document.body.append(shell);const session={doc:{id:'doc-'+(sessions.length+1),revision:1},shell,bridge:null,source:null,deleted:false,locked:false,conflict:null};session.toolbar=w.document.createElement('div');session.toolbar.append(w.docsText('button','Insert'));shell.append(session.toolbar);session.findBar=w.docsBuildFind(session);shell.append(session.findBar);session.scroll=w.document.createElement('div');session.content=w.document.createElement('div');session.scroll.append(session.content);shell.append(session.scroll);Object.defineProperty(session.content,'clientWidth',{value:500});Object.defineProperty(session.scroll,'clientWidth',{value:900});session.scroll.style.paddingLeft='50px';session.scroll.style.paddingRight='50px';
    session.bridge=w.EditorBridge.create({element:session.content,json:json||textJSON(text),onUpdate:()=>w.docsChanged(session),onSelectionUpdate:()=>w.docsUpdateTools(session),onError:error=>errors.push(error.message),onWorkBlockAction:action=>w.docsWorkBlockAction?.(session,action)?.catch(error=>errors.push(error.message))});sessions.push(session);w.documentsUi.current=session;return session;};
  h.work=()=>{h.routes=[];w.desktop.drafts=new w.Map();Object.assign(w,{docsBlockChoices:w.__testBlockChoices,openDocument:async id=>h.routes.push({open:id}),showLibrary:async()=>{},switchMode:async mode=>h.routes.push({mode}),newThread:async()=>h.routes.push({newChat:true}),fillPrompt:text=>h.routes.push({prompt:text}),saveDraft(){},savePreferences:async()=>{},draftKey:()=>w.documentsUi.current?.doc.id||'work',docsFlush:async()=>{},docsMutate:async()=>{},toast(){},rpcSend(){},remoteContext:()=>null,mentionedFiles:[],desktopAttachments:[],renderComposerChips(){}});w.eval(work);};
  h.select=async(session,from,to=from)=>{session.bridge.editor.chain().focus().setTextSelection({from,to}).run();await settle();};
  h.button=(label,root=w.document.getElementById('modal-body'))=>Array.from(root.querySelectorAll('button')).find(button=>button.textContent===label);
  h.field=label=>w.document.querySelector(`[aria-label="${label}"]`);
  h.click=async button=>{assert.ok(button,'Missing button');button.click();await settle();};
  h.destroy=()=>{for(const session of sessions)session.bridge?.destroy();w.close();};return h;
}

test('selection tools pass canonical UTF16 quote to Ask/Comment and run actual mark transactions',async t=>{
  const h=harness();t.after(h.destroy);const s=h.open('A😀B text');await h.select(s,2,4);h.window.docsSelectionTools(s);const toolbar=h.window.documentsUi.selectionToolbar;assert.equal(toolbar.hidden,false);assert.equal(toolbar.style.top,'67px');
  await h.click(h.button('Ask for change',toolbar));assert.equal(h.asked[0].selection.quote,'😀');assert.deepEqual({from:h.asked[0].selection.from,to:h.asked[0].selection.to},{from:2,to:4});
  await h.click(h.button('Comment',h.window.documentsUi.selectionToolbar));assert.equal(h.comments[0].selection.quote,'😀');await h.click(h.button('B',h.window.documentsUi.selectionToolbar));assert.ok(JSON.stringify(s.bridge.getJSON()).includes('"bold"'));s.bridge.commands.undo();assert.ok(!JSON.stringify(s.bridge.getJSON()).includes('"bold"'));
});

test('Link dialog rejects active protocols and ignores stale modal/session/doc-state callbacks',async t=>{
  const h=harness();t.after(h.destroy);const a=h.open('safe text');await h.select(a,1,5);h.window.docsLinkDialog(a);h.field('Link URL').value='javascript:alert(1)';await h.click(h.button('Insert link'));assert.equal(a.bridge.editor.getAttributes('link').href,undefined);assert.match(h.window.document.querySelector('[data-docs-error]').textContent,/valid/);
  h.field('Link URL').value='https://example.com';await h.click(h.button('Insert link'));assert.equal(a.bridge.getJSON().content[0].content[0].marks[0].attrs.href,'https://example.com');
  await h.select(a,1,5);h.window.docsLinkDialog(a);const oldButton=h.button('Update link');h.window.openCustomModal('Other','<p id="other">Keep new modal</p>');oldButton.click();await settle();assert.equal(h.window.document.getElementById('other').textContent,'Keep new modal');
  h.window.docsLinkDialog(a);const stale=h.button('Update link');h.open('B stays');stale.click();await settle();assert.equal(h.window.documentsUi.current.bridge.editor.state.doc.textContent,'B stays');
});

test('slash is a real one-transaction block change, keyboard navigates, Escape stays dismissed and IME is untouched',async t=>{
  const h=harness();t.after(h.destroy);const s=h.open('/hea');await h.select(s,5);h.window.docsSlash(s);let menu=h.window.documentsUi.slashMenu;assert.equal(menu.hidden,false);assert.equal(menu.querySelectorAll('button').length,3);
  const key=(name,extra={})=>{const event=new h.window.KeyboardEvent('keydown',{key:name,bubbles:true,cancelable:true,...extra});h.window.docsEditorKey(s,event);return event;};key('ArrowDown');assert.equal(menu.querySelector('[aria-selected=true]').textContent,'Heading 2');key('Enter');await settle();assert.equal(s.bridge.getJSON().content[0].type,'heading');assert.equal(s.bridge.getJSON().content[0].attrs.level,2);assert.equal(s.bridge.editor.state.doc.textContent,'');s.bridge.commands.undo();assert.equal(s.bridge.editor.state.doc.textContent,'/hea');
  h.window.docsSlash(s);key('Escape');h.window.docsSlash(s);assert.equal(menu.hidden,true);s.bridge.commands.insertContent('a');h.window.docsSlash(s);assert.equal(menu.hidden,false);
  s.shell.dispatchEvent(new h.window.CompositionEvent('compositionstart',{bubbles:true}));const before=s.bridge.getJSON();const composing=key('Enter',{isComposing:true});assert.equal(composing.defaultPrevented,false);assert.deepEqual(s.bridge.getJSON(),before);assert.equal(menu.hidden,true);s.shell.dispatchEvent(new h.window.CompositionEvent('compositionend',{bubbles:true}));await settle();h.window.docsSlash(s);assert.equal(menu.hidden,false);
});

test('Find spans marks, uses Unicode positions, replaces all in one real Undo step, and never crosses inline atoms',async t=>{
  const h=harness();t.after(h.destroy);const s=h.open('',{type:'doc',content:[{type:'paragraph',content:[{type:'text',text:'A😀 '},{type:'text',text:'ap',marks:[{type:'bold'}]},{type:'text',text:'ple apple'},{type:'inlineMath',attrs:{latex:'x'}},{type:'text',text:'apple'}]}]});h.window.docsToggleFind(s,true);const query=h.field('Find text');query.value='apple';query.oninput();assert.equal(s._docsFind.matches.length,3);assert.deepEqual(plain(s._docsFind.matches[0]),{from:5,to:10});
  h.field('Replace with').value='pear';await h.click(h.button('Replace all',s.findBar));assert.equal(s.bridge.editor.state.doc.textContent,'A😀 pear pearpear');s.bridge.commands.undo();assert.equal(s.bridge.editor.state.doc.textContent,'A😀 apple appleapple');query.value='apple\uFFFCapple';query.oninput();assert.equal(s._docsFind.matches.length,0);
  const event=new h.window.KeyboardEvent('keydown',{key:'f',metaKey:true,bubbles:true,cancelable:true});h.window.docsEditorKey(s,event);assert.equal(event.defaultPrevented,true);assert.equal(h.window.document.activeElement,query);s.locked=true;query.value='apple';query.oninput();h.field('Replace with').value='blocked';await h.click(h.button('Replace all',s.findBar));assert.equal(s.bridge.editor.state.doc.textContent,'A😀 apple appleapple');
});

test('actual dropped file import is local, stores only attachment IDs, and a late result never edits a new document',async t=>{
  const h=harness();t.after(h.destroy);const a=h.open('A');await h.select(a,2);const file=new h.window.File(['Original 中文\r\n'],'note.txt',{type:'text/plain'});await h.window.docsImportFiles(a,[file]);const call=h.calls.find(call=>call.method==='files/import');assert.equal(Buffer.from(call.params.dataBase64,'base64').toString(),'Original 中文\r\n');assert.ok(JSON.stringify(a.bridge.getJSON()).includes('fileAttachment'));assert.ok(!JSON.stringify(a.bridge.getJSON()).includes('dataBase64'));a.bridge.commands.undo();assert.equal(a.bridge.editor.state.doc.textContent,'A');
  const response=deferred();h.respond=method=>method==='files/import'?response.promise:undefined;const pending=h.window.docsImportFiles(a,[file]);await settle();const b=h.open('B unchanged');response.resolve({attachment:{id:SECOND,kind:'text',name:'late.txt',mimeType:'text/plain',size:2}});await pending;assert.equal(b.bridge.editor.state.doc.textContent,'B unchanged');assert.ok(!JSON.stringify(b.bridge.getJSON()).includes(SECOND));
});

test('Library picker replaces image attrs with real local IDs, supports alt/size, and stale state cannot save',async t=>{
  const h=harness();t.after(h.destroy);h.files=[{id:ID,kind:'image',name:'Old.png',mimeType:'image/png',size:4},{id:SECOND,kind:'image',name:'New.png',mimeType:'image/png',size:5}];const s=h.open('',{type:'doc',content:[{type:'image',attrs:{attachmentId:ID,name:'Old.png'}},{type:'paragraph'}]});s.bridge.commands.setNodeSelection(0);await h.window.docsPickAttachments(s,'image');const radios=h.window.document.querySelectorAll('[role=radiogroup] input');radios[1].checked=true;radios[1].onchange();h.field('Alternative text').value='New alt';h.field('Width in pixels').value='320';await h.click(h.button('Save / replace'));const image=s.bridge.getJSON().content[0];assert.equal(image.attrs.attachmentId,SECOND);assert.equal(image.attrs.alt,'New alt');assert.equal(image.attrs.width,320);s.bridge.commands.undo();assert.equal(s.bridge.getJSON().content[0].attrs.attachmentId,ID);
  s.bridge.commands.setNodeSelection(0);await h.window.docsPickAttachments(s,'image');const save=h.button('Save / replace');s.bridge.commands.insertContent({type:'paragraph',content:[{type:'text',text:'Changed while dialog open'}]});const before=s.bridge.getJSON();save.click();await settle();assert.deepEqual(s.bridge.getJSON(),before);await h.click(h.button('Cancel'));assert.equal(h.window.document.getElementById('modal-backdrop').classList.contains('open'),false);
});

test('math modal inserts actual inline/block nodes and ignores superseded modal results',async t=>{
  const h=harness();t.after(h.destroy);const s=h.open('Formula: ');await h.select(s,10);h.window.docsInsertMath(s);h.field('LaTeX source').value='x^2';await h.click(h.button('Insert formula'));assert.ok(JSON.stringify(s.bridge.getJSON()).includes('inlineMath'));s.bridge.commands.undo();assert.equal(s.bridge.editor.state.doc.textContent,'Formula: ');
  h.window.docsInsertMath(s);const button=h.button('Insert formula');h.field('LaTeX source').value='y';h.window.openCustomModal('Other','<p id="new-modal">New</p>');button.click();await settle();assert.equal(h.window.document.getElementById('new-modal').textContent,'New');assert.ok(!JSON.stringify(s.bridge.getJSON()).includes('inlineMath'));
});

test('late Library read, locked session, and active table layout keep the right document and visible DOM',async t=>{
  const h=harness();t.after(h.destroy);const a=h.open('A');const pending=deferred();h.respond=method=>method==='files/list'?pending.promise:undefined;const loading=h.window.docsPickAttachments(a,'file');const b=h.open('B');h.window.openCustomModal('Other','<p id="other">Keep</p>');pending.resolve({data:[{id:ID,kind:'text',name:'late'}]});await loading;assert.equal(h.window.document.getElementById('other').textContent,'Keep');a.locked=true;assert.equal(h.window.docsToolSnapshot(a),null);assert.equal(b.bridge.editor.state.doc.textContent,'B');
  h.window.closeModal();b.bridge.commands.setTextSelection(1);b.bridge.commands.insertTable({rows:2,cols:2,withHeaderRow:true});h.window.docsCommand(b,'tableLayout','full');h.window.docsToolsTableLayout(b);const table=b.content.querySelector('table');assert.equal(table.dataset.layout,'full');assert.equal(table.closest('.tableWrapper').style.width,'800px');h.window.docsCommand(b,'tableLayout','normal');h.window.docsToolsTableLayout(b);assert.equal(table.style.width,'auto');assert.equal(table.closest('.tableWrapper').style.marginLeft,'');
});

 test('source Find replacement has text Undo/Redo fallback and a locked session refuses changes',async t=>{
  const h=harness();t.after(h.destroy);const s=h.open('');s.bridge.destroy();s.bridge=null;s.source=h.window.document.createElement('textarea');s.source.value='one 😀 one';s.content.replaceChildren(s.source);h.window.docsToggleFind(s,true);h.field('Find text').value='one';h.field('Find text').oninput();h.field('Replace with').value='two';await h.click(h.button('Replace all',s.findBar));assert.equal(s.source.value,'two 😀 two');
  const key=(extra={})=>new h.window.KeyboardEvent('keydown',{key:'z',metaKey:true,bubbles:true,cancelable:true,...extra});const undo=key();h.window.docsEditorKey(s,undo);assert.equal(undo.defaultPrevented,true);assert.equal(s.source.value,'one 😀 one');h.window.docsEditorKey(s,key({shiftKey:true}));assert.equal(s.source.value,'two 😀 two');s.locked=true;h.field('Replace with').value='blocked';await h.click(h.button('Replace all',s.findBar));assert.equal(s.source.value,'two 😀 two');
 });

 test('slash choices refresh from real editor input transactions without keyup and old sessions cannot hide current tools',async t=>{
  const h=harness();t.after(h.destroy);const a=h.open('A');const b=h.open('/');await h.select(b,2);h.window.docsSelectionTools(b);b.bridge.commands.insertContent('table');await settle();const menu=h.window.documentsUi.slashMenu;assert.equal(menu.hidden,false);assert.deepEqual(Array.from(menu.querySelectorAll('button'),button=>button.textContent),['Table']);h.window.docsSlash(a);h.window.docsSelectionTools(a);assert.equal(menu.hidden,false);
  b.bridge.editor.commands.blur();await settle();h.window.docsSlash(b);assert.equal(menu.hidden,true);
 });

 test('actual TipTap AI block fragment replacement keeps partial-paragraph prefix/suffix and Undo restores the document',async t=>{
  const h=harness();t.after(h.destroy);const original='前😀 prefix SELECT suffix';for(const markdown of ['**replacement**','## New heading\n\n- first\n- second']){
    const s=h.open(original);const before=s.bridge.getJSON(),from=original.indexOf('SELECT')+1,to=from+'SELECT'.length,imported=h.window.EditorBridge.importMarkdown(markdown);assert.equal(imported.lossless,true);assert.equal(s.bridge.editor.state.doc.textBetween(from,to,'\n','\uFFFC'),'SELECT');
    assert.equal(s.bridge.editor.chain().focus().insertContentAt({from,to},imported.json.content).run(),true);const visible=s.bridge.editor.state.doc.textContent;assert.ok(visible.startsWith('前😀 prefix '));assert.ok(visible.endsWith(' suffix'));assert.ok(!visible.includes('SELECT'));assert.ok(visible.includes(markdown.startsWith('##')?'New heading':'replacement'));s.bridge.commands.undo();assert.deepEqual(plain(s.bridge.getJSON()),plain(before));
  }
 });

test('print snapshots keep rich content, exclude the app shell, and leave the live editor and Undo untouched',async t=>{
  const h=harness();t.after(h.destroy);const w=h.window,s=h.open('',{type:'doc',content:[{type:'paragraph',content:[{type:'text',text:'中文 😀 print'},{type:'inlineMath',attrs:{latex:'E=mc^2'}}]}]});s.doc.title='打印标题';
  w.docsFlush=async()=>{};const before=plain(s.bridge.getJSON());const root=await w.docsPreparePrint(s);
  assert.ok(root.isConnected);assert.ok(root.textContent.includes('打印标题')&&root.textContent.includes('中文 😀 print'));
  assert.equal(root.querySelector('[contenteditable]'),null);assert.equal(root.querySelector('#modal-backdrop'),null);
  assert.equal(root.querySelector('img.ProseMirror-separator'),null);assert.ok(root.querySelector('.katex'));
  assert.deepEqual(plain(s.bridge.getJSON()),before);assert.ok(s.content.isConnected);
  w.eval(core.split('\n').find(line=>line.startsWith("window.addEventListener('afterprint'")));
  w.__mcodexNative=true;w.dispatchEvent(new w.Event('afterprint'));assert.ok(root.isConnected,'native print sheet must retain the snapshot until it finishes drawing');
  w.__mcodexNative=false;w.dispatchEvent(new w.Event('afterprint'));assert.equal(root.isConnected,false);
  w.docsClearPrint();assert.equal(root.isConnected,false);assert.ok(s.content.isConnected);
  let printed=0;w.print=()=>{printed++;assert.ok(w.document.getElementById('page-print-root'));};await w.docsPrint(s);
  assert.equal(printed,1);assert.equal(w.document.getElementById('page-print-root'),null);
  s.bridge.destroy();s.bridge=null;s.source=w.document.createElement('textarea');s.doc.content='<script>literal</script>\n中文 source';s.source.value=s.doc.content;
  const source=await w.docsPreparePrint(s);assert.equal(source.querySelector('script'),null);assert.ok(source.querySelector('pre').textContent===s.doc.content);
});

test('save failures, missing images and stale navigation never open a print dialog or leak a partial print tree',async t=>{
  const h=harness();t.after(h.destroy);const w=h.window,s=h.open('A');let printed=0;w.print=()=>printed++;
  w.docsFlush=async()=>{throw new Error('Save failed');};await assert.rejects(w.docsPrint(s),/Save failed/);assert.equal(printed,0);
  const image=h.open('',{type:'doc',content:[{type:'image',attrs:{attachmentId:ID,name:'missing.png'}}]});w.docsFlush=async()=>{};
  h.respond=method=>{if(method==='files/read')throw new Error('Missing original');};await assert.rejects(w.docsPrint(image),/Missing original/);assert.equal(printed,0);assert.equal(w.document.getElementById('page-print-root'),null);
  const gate=deferred();h.respond=method=>method==='files/read'?gate.promise:undefined;
  const pending=w.docsPrint(image);await settle();h.open('B should stay');gate.resolve({attachment:{mimeType:'image/png'},content:{dataBase64:'aGVsbG8='}});await pending;
  assert.equal(printed,0);assert.equal(w.document.getElementById('page-print-root'),null);assert.equal(w.documentsUi.current.bridge.editor.state.doc.textContent,'B should stay');
});

test('Work blocks round-trip with their full data; actual edit transactions retain Undo and visualization isolation',async t=>{
  const h=harness();t.after(h.destroy);h.work();const w=h.window;
  for(const kind of ['prompt','task','instructions','highlight','page','chat','visualize']){
    const json={type:'doc',content:[w.docsWorkNode(kind,{title:'中文 block',text:'请求 text',targetId:['page','chat'].includes(kind)?'local-id':'',html:kind==='visualize'?'<button>Interactive</button>':''})]};
    const saved=w.EditorBridge.validateJSON(json),markdown=w.EditorBridge.exportMarkdown(saved),restored=w.EditorBridge.importMarkdown(markdown.markdown);
    assert.equal(markdown.lossless,true);assert.equal(restored.lossless,true);assert.deepEqual(plain(restored.json),plain(saved));
  }
  const s=h.open('',{type:'doc',content:[w.docsWorkNode('prompt',{title:'Before',text:'Original'})]});
  await h.click(h.button('Edit block',s.content));h.field('Block title').value='After';h.field('Block content').value='New request';await h.click(h.button('Save block'));
  assert.equal(s.bridge.getJSON().content[0].attrs.title,'After');s.bridge.commands.undo();assert.equal(s.bridge.getJSON().content[0].attrs.text,'Original');
  const visual=h.open('',{type:'doc',content:[w.docsWorkNode('visualize',{html:'<script>parent.invalid=true<\/script>'})]});const frame=visual.content.querySelector('iframe');
  assert.equal(frame.getAttribute('sandbox'),'allow-scripts');assert.ok(frame.srcdoc.includes("connect-src 'none'")&&frame.srcdoc.includes("frame-src 'none'"));
  assert.equal(w.invalid,undefined);assert.equal(h.errors.length,0,h.errors.join('; '));
});

test('Task block routes to a local Codex draft with visible page guidance and does not automatically run tools',async t=>{
  const h=harness();t.after(h.destroy);h.work();const w=h.window;
  const s=h.open('',{type:'doc',content:[w.docsWorkNode('instructions',{text:'Preserve source data.'}),w.docsWorkNode('task',{title:'Audit',text:'Inspect this project.'})]});
  let remote=true;w.remoteContext=()=>remote?'ssh-fixture':null;w.remoteSelect=async value=>{assert.equal(value,null);remote=false;h.routes.push({local:true});};
  const task=s.bridge.editor.state.doc.child(0).nodeSize;s.bridge.commands.setNodeSelection(task);await w.docsWorkBlockAction(s,{action:'run'});
  assert.deepEqual(h.routes.map(value=>Object.keys(value)[0]),['local','mode','newChat','prompt']);assert.equal(h.routes[1].mode,'codex');
  assert.ok(h.routes[3].prompt.includes('Preserve source data.')&&h.routes[3].prompt.includes('Inspect this project.'));
  assert.equal(h.calls.filter(call=>call.method==='turn/start').length,0);assert.equal(remote,false);
});

test('Pages image generation waits for explicit insertion and rejects a result after the page changed',async t=>{
  const h=harness();t.after(h.destroy);h.work();const w=h.window,s=h.open('Original');
  await h.select(s,9);await w.docsPickAttachments(s,'image');await h.click(h.button('Generate image'));
  assert.equal(typeof h.imageOptions.onUse,'function');assert.equal(s.bridge.editor.state.doc.textContent,'Original');
  h.respond=method=>method==='files/read'?{attachment:{id:ID,kind:'image',mimeType:'image/png',name:'Generated.png'}}:undefined;
  await h.imageOptions.onUse({id:ID});assert.ok(JSON.stringify(s.bridge.getJSON()).includes(ID));assert.ok(!JSON.stringify(s.bridge.getJSON()).includes('dataBase64'));
  s.bridge.commands.undo();assert.equal(s.bridge.editor.state.doc.textContent,'Original');
  await w.docsPickAttachments(s,'image');await h.click(h.button('Generate image'));const late=h.imageOptions.onUse;s.bridge.commands.insertContent(' new edit');const before=s.bridge.getJSON();
  await assert.rejects(late({id:ID}),/page changed/);assert.deepEqual(s.bridge.getJSON(),before);
});
