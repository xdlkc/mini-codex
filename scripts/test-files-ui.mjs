#!/usr/bin/env node
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
const source=readFileSync(new URL('../src/gui_files.js',import.meta.url),'utf8');
const plain=value=>JSON.parse(JSON.stringify(value));
function deferred(){let resolve;const promise=new Promise(yes=>resolve=yes);return {promise,resolve};}
function harness(respond=()=>({})){
 const nodes=new Map(),calls=[],prefs=[],closed=[];
 function element(){const classes=new Set();return {children:[],textContent:'',value:'',style:{},classList:{add:k=>classes.add(k),contains:k=>classes.has(k)},querySelectorAll(){return [];},querySelector(){return null;},prepend(){},append(){}};}
 const node=id=>{if(!nodes.has(id))nodes.set(id,element());return nodes.get(id);};
 const context=vm.createContext({console,Map,Set,Promise,Array,Number,Object,String,Uint8Array,setTimeout,clearTimeout,
  desktop:{mode:'work',navigation:0,state:{settings:{preferences:{attachmentDrafts:{work:[{type:'attachment',id:'saved',attachmentId:'saved',name:'saved.pdf',parseStatus:'ready'}]},mentionDrafts:{work:['README.md']}}}}},
  currentThread:null,desktopAttachments:[],mentionedFiles:[],isTurnActive:false,desktopReady:new Promise(()=>{}),scheduledUi:{goal:null},
  $:node,document:{createElement:element,body:element()},
  draftKey(){return context.currentThread||context.desktop.mode;},saveDraft(){},restoreDraft(){},savePreferences:async patch=>prefs.push(plain(patch)),
  renderComposerChips(){node('composer-chips').children=[...context.mentionedFiles,...context.desktopAttachments].map(element);},
  updateSubmitButton(){},sendTurn:async()=>{},retryLastTurn:async()=>{},showLibrary:async()=>{},upsertTimelineItem(){},
  rpcCall(method,params={}){calls.push({method,params});return Promise.resolve(respond(method,params));},
  showError(){},toast(){},saveDraft(){},syncShellNavigation(){},itemEls:new Map(),
 });context.window=context;context.__requestClose=async()=>closed.push(true);vm.runInContext(source,context);context.fileBase64=async()=> 'bG9jYWwgZmlsZQ==';
 return {context,node,calls,prefs,closed,state:vm.runInContext('filesUi',context)};
}
const card={id:'imported',name:'Imported report.pdf',mimeType:'application/pdf',kind:'pdf',size:50,parseStatus:'ready'};

test('draft attachments and mentions survive the reset that occurs before restoreDraft',()=>{
 const h=harness();h.context.restoreDraft();assert.equal(h.context.desktopAttachments[0].attachmentId,'saved');assert.deepEqual(plain(h.context.mentionedFiles),['README.md']);
 h.context.desktopAttachments=[];h.context.mentionedFiles=[];h.context.renderComposerChips();h.context.restoreDraft();
 assert.equal(h.context.desktopAttachments[0].attachmentId,'saved');assert.deepEqual(plain(h.context.mentionedFiles),['README.md']);
});
test('a delayed import belongs to its original draft after navigation',async()=>{
 const pending=deferred(),h=harness(method=>method==='files/import'?pending.promise:{});
 const work=h.context.addAttachments([{name:'report.pdf',type:'application/pdf',size:50}]);await Promise.resolve();await Promise.resolve();
 h.context.desktop.navigation++;h.context.currentThread='new-chat';h.context.desktopAttachments=[{attachmentId:'new-file',name:'new.txt'}];pending.resolve({attachment:card});await work;
 assert.equal(h.context.desktopAttachments[0].attachmentId,'new-file');assert.equal(h.state.drafts.get('work').at(-1).attachmentId,'imported');
 assert.equal(h.prefs.at(-1).attachmentDrafts.work.at(-1).attachmentId,'imported');
});
test('importing from Library does not silently attach files to the current chat',async()=>{
 const h=harness(method=>method==='files/import'?{attachment:card}:{});await h.context.addAttachments([{name:'report.pdf',size:50}],{attach:false});
 assert.deepEqual(h.context.desktopAttachments,[]);assert.ok(!h.prefs.at(-1).attachmentDrafts.work.some(file=>file.attachmentId==='imported'));
});
test('closing the native window waits for import and persisted draft before delegating close',async()=>{
 const pending=deferred(),h=harness(method=>method==='files/import'?pending.promise:{});const importing=h.context.addAttachments([{name:'report.pdf',size:50}]);await Promise.resolve();await Promise.resolve();
 const closing=h.context.__requestClose();await Promise.resolve();assert.equal(h.closed.length,0);pending.resolve({attachment:card});await importing;await closing;
 assert.equal(h.closed.length,1);assert.equal(h.prefs.at(-1).attachmentDrafts.work.at(-1).attachmentId,'imported');
});
test('stored image references avoid sending a second copy while legacy inline images remain',()=>{
 const h=harness();const inputs=h.context.messageAttachments([{type:'input_image',image_url:'data:image/png;base64,new'},{type:'input_image',image_url:'data:image/png;base64,legacy'},{...card,id:'stored-image',kind:'image',imageIndex:0}]);
 assert.deepEqual(plain(inputs).map(input=>input.type),['attachment','image']);assert.equal(inputs[0].attachmentId,'stored-image');assert.equal(inputs[1].imageUrl,'data:image/png;base64,legacy');
});
test('unreadable imports remain visible but cannot start a model request',async()=>{
 const h=harness(method=>method==='files/import'?{attachment:{...card,parseStatus:'no_text'}}:{});await h.context.addAttachments([{name:'scan.pdf',size:50}]);
 assert.equal(h.context.desktopAttachments[0].parseStatus,'no_text');await h.context.sendTurn();assert.ok(!h.calls.some(call=>call.method==='turn/start'));assert.equal(h.node('submit-btn').disabled,true);
});
