import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFileSync} from 'node:fs';
const source=readFileSync(new URL('../src/gui_native_windows.js',import.meta.url),'utf8');
const clone=value=>JSON.parse(JSON.stringify(value));
function client(kind,settings){
  let result;const sent=[];const context=vm.createContext({Object,window:{__mcodexWindow:{kind},__nativeToken:'private-window-token',__onServerMessage:message=>result=clone(message)},rpcSend:message=>sent.push(clone(message))});vm.runInContext(source,context);
  return {send:message=>context.rpcSend(message),receive:value=>{context.window.__onServerMessage(value);return result;},sent,read(){return this.receive({id:'read',result:{settings:clone(settings)}}).result.settings;},apply(params){Object.assign(settings.preferences,params.preferences);if(params.mode)settings.mode=params.mode;}};
}
test('main, Mini and popout persist distinct text/file/search drafts while sharing global settings',()=>{
  const settings={mode:'work',preferences:{drafts:{work:'MAIN'},attachmentDrafts:{work:[{id:'main-file'}]},webSearchDrafts:{work:'live'},theme:'light'}};
  const main=client('main',settings),mini=client('mini',settings),popout=client('popout',settings);
  for(const [c,text] of [[mini,'MINI'],[popout,'POPOUT']]){
    c.send({id:1,method:'desktop/settings/update',params:{mode:'chat',preferences:{drafts:{chat:text},attachmentDrafts:{chat:[{id:text+'-file'}]},webSearchDrafts:{chat:'disabled'}}}});c.apply(c.sent[0].params);
  }
  assert.equal(main.read().mode,'work');assert.equal(main.read().preferences.drafts.work,'MAIN');
  assert.equal(mini.read().preferences.drafts.chat,'MINI');assert.equal(popout.read().preferences.drafts.chat,'POPOUT');
  assert.equal(mini.read().preferences.attachmentDrafts.chat[0].id,'MINI-file');assert.equal(popout.read().preferences.webSearchDrafts.chat,'disabled');
  mini.send({id:2,method:'desktop/settings/update',params:{preferences:{theme:'dark'}}});mini.apply(mini.sent[1].params);assert.equal(main.read().preferences.theme,'dark');
  assert.equal(mini.sent[0]._nativeToken,'private-window-token');
});
test('auxiliary settings projection does not change ordinary RPC payloads or a main window reply',()=>{
  const settings={mode:'codex',preferences:{drafts:{codex:'saved'}}};const c=client('mini',settings),main=client('main',settings);
  c.send({id:'turn',method:'turn/start',params:{threadId:'local-thread',input:[{type:'text',text:'hello'}]}});assert.deepEqual(c.sent[0].params,{threadId:'local-thread',input:[{type:'text',text:'hello'}]});
  assert.deepEqual(main.read(),settings);assert.deepEqual(c.read().preferences.drafts,{});assert.equal(c.read().mode,'chat');
  const read={id:1,result:{thread:{id:'x',mode:'work'}}};assert.deepEqual(c.receive(read),read);
});
const controls=readFileSync(new URL('../src/gui_native_controls.js',import.meta.url),'utf8').split('const nativeRenderSettings=')[0];
function handoffClient(kind='mini'){
  const sent=[],stored=[],input={value:'unsent'},settings={};let nextTimer=0;
  const c=vm.createContext({Object,Map,Set,JSON,Promise,Error,crypto:{randomUUID:()=> 'handoff-ticket'},setTimeout:()=>++nextTimer,clearTimeout:()=>{},
    window:{},nativeWindowInfo:{kind},shellReady:Promise.resolve(),filesReady:Promise.resolve(),voiceReady:Promise.resolve(),filesUi:{importing:false},
    currentThread:null,currentCwd:'/local/project',currentModel:'fixture-model',desktop:{mode:'work',projectId:'project-local',navigation:1,drafts:new Map()},desktopAttachments:[{attachmentId:'local-file',name:'image.png'}],mentionedFiles:['local-ref'],
    $:()=>input,rpcSend:message=>sent.push(clone(message)),draftKey:()=>c.currentThread||c.desktop.mode,
    saveDraft:()=>c.desktop.drafts.set(c.currentThread||c.desktop.mode,input.value),savePreferences:async params=>{stored.push(clone(params));Object.assign(settings,params);},renderComposerChips:()=>{},
    fillPrompt:text=>input.value=text,remoteContext:()=>null,flushDocument:async()=>{},rpcLocalCall:async(method,params)=>{assert.equal(method,'thread/start');stored.push({method,params:clone(params)});return {thread:{id:'new-local-thread'}};},
    openThread:async id=>{c.saveDraft();c.currentThread=id;input.value=c.desktop.drafts.get(id)||'';},
  });vm.runInContext(controls,c);return {c,input,sent,stored};
}
async function queued(){for(let i=0;i<10;i++)await Promise.resolve();}
test('a draft is cleared only after the receiving window confirms saving it',async()=>{
  const h=handoffClient(),pending=h.c.nativeOpenMain();await queued();assert.equal(h.input.value,'unsent');
  const outgoing=h.sent[0];assert.equal(outgoing.__window,'main');assert.equal(outgoing.draft.projectId,'project-local');assert.equal(outgoing.draft.attachments[0].attachmentId,'local-file');
  await h.c.window.__nativeInvoke('handoffComplete',{ticket:outgoing.ticket,threadId:'received'});await pending;
  assert.equal(h.input.value,'');assert.equal(h.c.desktopAttachments.length,0);assert.equal(h.c.mentionedFiles.length,0);
});
test('late acknowledgment keeps edits made after a handoff started',async()=>{
  const h=handoffClient(),pending=h.c.nativeOpenMain();await queued();h.input.value='new edits after transfer';
  await h.c.window.__nativeInvoke('handoffComplete',{ticket:h.sent[0].ticket});await pending;
  assert.equal(h.input.value,'new edits after transfer');assert.equal(h.c.desktopAttachments.length,1);
});
test('receiving a new chat keeps the main draft, local project, files and references; save failure sends no acknowledgment',async()=>{
  const h=handoffClient('main');h.input.value='original main draft';h.c.desktopAttachments=[];h.c.mentionedFiles=[];
  await h.c.window.__nativeInvoke('handoff',{ticket:'transfer',draft:{text:'Mini text',mode:'work',projectId:'project-local',cwd:'/project',model:'model',attachments:[{attachmentId:'image'}],mentions:['file']}});
  assert.equal(h.c.desktop.drafts.get('work'),'original main draft');assert.equal(h.input.value,'Mini text');assert.equal(h.c.desktopAttachments[0].attachmentId,'image');
  assert.equal(h.stored.find(item=>item.method)?.params.projectId,'project-local');assert.equal(h.sent.at(-1).__window,'handoffComplete');
  h.sent.length=0;h.c.savePreferences=async()=>{throw new Error('Disk unavailable');};
  await assert.rejects(h.c.window.__nativeInvoke('handoff',{ticket:'failed',threadId:'new-local-thread',draft:{text:'keep this',attachments:[],mentions:[]}}),/Disk unavailable/);assert.equal(h.sent.length,0);
});

test('popping out a reference-only draft transfers and acknowledges its file mentions',async()=>{
  const h=handoffClient('main');h.input.value='';h.c.desktopAttachments=[];h.c.mentionedFiles=['notes.md'];const pending=h.c.nativePopout();await queued();
  assert.equal(h.sent[0].__window,'popout');assert.deepEqual(h.sent[0].draft.mentions,['notes.md']);
  await h.c.window.__nativeInvoke('handoffComplete',{ticket:h.sent[0].ticket});await pending;assert.equal(h.c.mentionedFiles.length,0);
});
test('navigation during draft persistence does not open or transfer another chat',async()=>{
  const h=handoffClient('main');let resolve;h.c.savePreferences=()=>new Promise(done=>resolve=done);
  const pending=h.c.nativePopout();await queued();h.c.currentThread='another-chat';h.c.desktop.navigation++;h.input.value='another draft';resolve();
  await assert.rejects(pending,/chat changed/);assert.equal(h.sent.length,0);assert.equal(h.input.value,'another draft');
});
