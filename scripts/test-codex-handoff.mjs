import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
const source=readFileSync(new URL('../src/gui_codex_handoff.js',import.meta.url),'utf8');
const core=source.slice(0,source.indexOf('// UI composition'));
const plain=value=>JSON.parse(JSON.stringify(value));
function deferred(){let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};}
async function settle(){for(let i=0;i<30;i++)await Promise.resolve();}
function fixture(overrides={}){
  return {data:{thread:{id:'source',mode:'codex',cwd:'/repo/nested',status:{type:'idle'}},blockedReason:null,workspace:{repository:{repoRoot:'/repo'},localCheckout:{path:'/repo',state:'active',dirtyStatus:{clean:true}},sourceBranch:'main',defaultBranch:'main',head:'commit',branches:[{name:'main',commit:'commit',worktreePath:'/repo'}],worktrees:[{path:'/repo',primary:true},{path:'/existing',branch:'feat/existing',state:'active'}],pending:[]}},state:{branch:'feat/new',localBranch:'main',targetPath:''},...overrides};
}
function harness(respond=()=>({}),save=async()=>{}){
  const calls=[],opened=[],saved=[],notices=[],painted=[],rendered=[],nodes=new Map();let context,uuid=0;
  const node=id=>{if(!nodes.has(id))nodes.set(id,{value:'',scrollTop:0,readOnly:false,classList:{add(){},remove(){}},focus(){}});return nodes.get(id);};
  const preferences={order:[JSON.stringify(['local','source'])],pinnedOrder:[JSON.stringify(['local','source'])],threadSections:{[JSON.stringify(['local','source'])]:'custom'}};
  context=vm.createContext({Map,Set,JSON,Promise,Error,Date,crypto:{randomUUID:()=>`request-${++uuid}`},
    desktop:{mode:'codex',navigation:1,tabs:[{id:'other',title:'Other'},{id:'source',title:'Original'}],drafts:new Map([['source','original text']]),autoScroll:false},
    filesUi:{drafts:new Map([['source',[{attachmentId:'att_source',name:'source.txt'}]]]),mentions:new Map([['source',['src/file.rs']]]),importing:0,waiters:[]},
    webSearchUi:{drafts:new Map([[JSON.stringify(['local','source']),'live']])},
    codexSidebarUi:{preferences,write:Promise.resolve()},
    codexSidebarPreferences:()=>context.codexSidebarUi.preferences,codexSidebarKey:(host,id)=>JSON.stringify([host,id]),
    currentThread:'source',currentCwd:'/repo/nested',currentModel:'selected-model',currentEffort:'high',currentSandbox:'workspace-write',drawerOpen:true,currentDrawerTab:'files',intent:1,host:'local',
    $:node,remoteContext:()=>context.host==='local'?null:context.host,codexText:en=>en,codexForkIntent:()=>context.intent,codexForkCurrent:intent=>context.desktop.mode==='codex'&&context.intent===intent,
    codexThreadContext:()=>{const host=context.host;return {host,call:async(method,params,timeout)=>{calls.push({host,method,params:plain(params),timeout});return respond(method,params);}};},
    fileDraftMaps(){},webSearchDrafts:()=>context.webSearchUi.drafts,webSearchThreadKey:(host,id)=>JSON.stringify([host,id]),webSearchFlushWrites:async()=>{},flushDocument:async()=>{},closeModal:()=>vm.runInContext('codexHandoffUi.dialog=null',context),
    saveDraft(){const id=context.currentThread;context.desktop.drafts.set(id,node('composer-input').value);context.filesUi.drafts.set(id,context.desktopAttachments.map(file=>({...file})));context.filesUi.mentions.set(id,[...context.mentionedFiles]);},
    restoreDraft(){node('composer-input').value=context.desktop.drafts.get(context.currentThread)||'';},
    desktopAttachments:[{attachmentId:'att_source',name:'source.txt'}],mentionedFiles:['src/file.rs'],
    savePreferences:async patch=>{saved.push(plain(patch));await save(patch);},
    openThread:async id=>{context.saveDraft();context.intent++;context.currentThread=id;context.currentCwd='/destination/nested';context.restoreDraft();opened.push(id);},
    updateModelAndEffortUI:()=>rendered.push('model'),renderDrawerContent:async()=>rendered.push('drawer'),remoteRemember(){},refreshThreads:async()=>{},
    codexHandoffPaint:()=>painted.push(true),codexHandoffProgress:job=>{context.lastProgress=job.status;context.codexHandoffDialogOwner={job};},
    toast:(message,action)=>notices.push({message,action}),
  });
  node('composer-input').value='original text';node('conversation-wrap').scrollTop=321;vm.runInContext(core,context);
  const read=expression=>vm.runInContext(expression,context);
  function dialog(){const d=fixture();d.context=context.codexThreadContext();d.intent=context.intent;d.validate=()=>{};context.dialog=d;vm.runInContext('codexHandoffUi.dialog=dialog',context);return d;}
  return {context,node,calls,opened,saved,notices,painted,rendered,read,dialog};
}
const result={thread:{id:'destination',name:'Original',cwd:'/destination/nested',model:'saved-model',reasoningEffort:'high',pinned:true},handoff:{id:'stored-op',direction:'toWorktree',warnings:[]}};
test('default-branch handoff requires a fresh non-default branch',()=>{
  const h=harness(),d=fixture();assert.equal(h.context.codexHandoffPlan(d).error,null);
  assert.deepEqual(plain(h.context.codexHandoffPlan(d).params),{threadId:'source',direction:'toWorktree',worktreeBranch:'feat/new'});
  d.state.branch='main';assert.match(h.context.codexHandoffPlan(d).error,/default branch/);
  d.state.branch='feat/existing';d.data.workspace.branches.push({name:'feat/existing'});assert.match(h.context.codexHandoffPlan(d).error,/already exists/);
  for(const name of ['-bad','bad/','bad..name','bad.lock','refs/.private','bad name','bad\\name','bad@{1}'])assert.equal(h.context.codexHandoffBranchValid(name),false,name);
});
test('a named branch moves only after a different available local branch is selected',()=>{
  const h=harness(),d=fixture();d.data.workspace.sourceBranch='feat/source';d.data.workspace.branches.push({name:'feat/source',worktreePath:'/repo'});
  assert.deepEqual(plain(h.context.codexHandoffPlan(d).params),{threadId:'source',direction:'toWorktree',worktreeBranch:'feat/source',localBranch:'main'});
  d.state.localBranch='feat/source';assert.match(h.context.codexHandoffPlan(d).error,/Select local/);
});
test('direct continuation preserves a dirty existing worktree and waits for the running response',()=>{
  const h=harness(),d=fixture();d.state.targetPath='/existing';d.data.workspace.worktrees[1].dirtyStatus={clean:false};
  assert.deepEqual(plain(h.context.codexHandoffPlan(d).params),{threadId:'source',direction:'toWorktree',targetPath:'/existing',continueExisting:true});
  d.data.thread.status.type='active';assert.match(h.context.codexHandoffPlan(d).error,/Wait for the current response/);
});
test('returning locally requires a clean checkout and a matching branch head',()=>{
  const h=harness(),d=fixture();d.data.workspace.repository.repoRoot='/linked';d.data.workspace.sourceBranch='feat/linked';d.state.branch='feat/linked';
  assert.deepEqual(plain(h.context.codexHandoffPlan(d).params),{threadId:'source',direction:'toLocal',localBranch:'feat/linked'});
  d.data.workspace.localCheckout.dirtyStatus.clean=false;assert.match(h.context.codexHandoffPlan(d).error,/Stash or commit/);
  d.data.workspace.localCheckout.dirtyStatus.clean=true;d.data.workspace.branches.push({name:'feat/linked',commit:'other'});assert.match(h.context.codexHandoffPlan(d).error,/different commit/);
});
test('queued messages and voice calls disable handoff; a goal or heartbeat is not an exclusion',()=>{
  const h=harness(),d=fixture();for(const reason of ['queuedFollowUps','voiceCall']){d.data.blockedReason=reason;assert.ok(h.context.codexHandoffPlan(d).error);}
  d.data.blockedReason=null;d.data.thread.goal={status:'paused'};d.data.heartbeatCount=1;assert.equal(h.context.codexHandoffPlan(d).error,null);
  assert.equal(h.calls.length,0);
});
test('a completed Git move with a failed chat copy keeps the entire original composer and placement',async()=>{
  const failed={thread:{id:'source',cwd:'/repo/nested'},handoff:{id:'stored-op',warnings:['Git changes were handed off, but the chat could not be copied']}};
  const h=harness(()=>failed),d=h.dialog();await h.context.codexHandoffStart(d);
  const job=h.read('codexHandoffUi.jobs.values().next().value');assert.equal(job.status,'warning');assert.equal(job.chatCopied,false);
  assert.deepEqual(h.opened,[]);assert.equal(h.context.currentThread,'source');assert.equal(h.node('composer-input').value,'original text');
  assert.equal(h.context.desktop.drafts.get('source'),'original text');assert.equal(h.context.filesUi.drafts.get('source')[0].attachmentId,'att_source');
  assert.deepEqual(plain(h.context.codexSidebarUi.preferences.pinnedOrder),[JSON.stringify(['local','source'])]);assert.equal(h.context.desktop.tabs[1].id,'source');
});
test('completion moves the composer, files, pin position and open tab without recreating the source draft',async()=>{
  const h=harness(()=>result),d=h.dialog();await h.context.codexHandoffStart(d);
  assert.deepEqual(h.opened,['destination']);assert.equal(h.context.currentModel,'saved-model');assert.equal(h.node('composer-input').value,'original text');
  assert.equal(h.context.desktop.drafts.has('source'),false);assert.equal(h.context.filesUi.drafts.has('source'),false);
  assert.equal(h.context.filesUi.drafts.get('destination')[0].attachmentId,'att_source');assert.deepEqual(plain(h.context.filesUi.mentions.get('destination')),['src/file.rs']);
  assert.deepEqual(plain(h.context.desktop.tabs.map(tab=>tab.id)),['other','destination']);
  const prefs=h.context.codexSidebarUi.preferences;assert.deepEqual(plain(prefs.pinnedOrder),[JSON.stringify(['local','destination'])]);assert.equal(prefs.threadSections[JSON.stringify(['local','destination'])],'custom');
  assert.equal(h.context.webSearchUi.drafts.get(JSON.stringify(['local','destination'])),'live');assert.equal(h.node('conversation-wrap').scrollTop,321);assert.deepEqual(h.rendered,['model','drawer']);
  assert.equal(h.calls[0].method,'thread/handoff');assert.equal(h.calls[0].timeout,180000);assert.equal(h.calls[0].params.model,'selected-model');assert.equal(h.calls[0].params.reasoningEffort,'high');
});
test('an existing destination draft preserves the complete original composer bundle',async()=>{
  const h=harness();h.context.desktop.drafts.set('destination','new target draft');
  const job={thread:{id:'source'},context:h.context.codexThreadContext()};await h.context.codexHandoffMoveState(job,result.thread);
  assert.equal(h.context.desktop.drafts.get('source'),'original text');assert.equal(h.context.desktop.drafts.get('destination'),'new target draft');assert.equal(h.context.filesUi.drafts.get('source').length,1);assert.equal(h.context.filesUi.drafts.has('destination'),false);assert.equal(h.notices.length,1);
});
test('double-clicking during the preflight save starts one RPC',async()=>{
  const waiting=deferred(),h=harness(()=>result,()=>waiting.promise),d=h.dialog();
  const first=h.context.codexHandoffStart(d),second=h.context.codexHandoffStart(d);await settle();assert.equal(h.calls.length,0);waiting.resolve();await Promise.all([first,second]);assert.equal(h.calls.filter(call=>call.method==='thread/handoff').length,1);
});
test('navigation during the preflight save closes its stale dialog without starting Git',async()=>{
  const waiting=deferred(),h=harness(()=>result,()=>waiting.promise),d=h.dialog();const started=h.context.codexHandoffStart(d);await settle();h.context.intent++;h.context.currentThread='other';waiting.resolve();await started;
  assert.equal(h.calls.length,0);assert.equal(d.starting,false);assert.equal(h.read('codexHandoffUi.dialog'),null);
});
test('an import already in progress is saved with the source before handoff begins',async()=>{
  const h=harness(()=>result),d=h.dialog();h.context.filesUi.importing=1;const started=h.context.codexHandoffStart(d);await settle();assert.equal(h.calls.length,0);
  h.context.desktopAttachments.push({attachmentId:'arrived',name:'arrived.txt'});h.context.filesUi.importing=0;for(const resolve of h.context.filesUi.waiters.splice(0))resolve();await started;
  assert.deepEqual(plain(h.context.filesUi.drafts.get('destination').map(file=>file.attachmentId)),['att_source','arrived']);assert.equal(h.calls.filter(call=>call.method==='thread/handoff').length,1);
});
test('a late remote result keeps its captured host and does not navigate away from a newer chat',async()=>{
  const waiting=deferred(),h=harness(()=>waiting.promise);h.context.host='alpha';const d=h.dialog();h.context.desktop.drafts.set('source','remote draft');h.node('composer-input').value='remote draft';
  const started=h.context.codexHandoffStart(d);await settle();h.context.intent++;h.context.currentThread='newer';h.context.host='beta';h.node('composer-input').value='newer draft';waiting.resolve(result);await started;
  assert.equal(h.calls[0].host,'alpha');assert.deepEqual(h.opened,[]);assert.equal(h.context.currentThread,'newer');assert.equal(h.node('composer-input').value,'newer draft');assert.equal(h.context.desktop.drafts.get('destination'),'remote draft');assert.ok(h.notices.some(note=>note.action?.label==='Open'));
});
test('a timeout remains pending and can recover the committed result without another handoff',async()=>{
  let completed=false;const h=harness((method)=>{if(method==='thread/handoff')throw new Error('Request timed out: thread/handoff');if(method==='thread/handoff/status')return completed?{active:false,operation:{status:'committed',ownerThreadId:'destination',warnings:[]}}:{active:true};return {thread:result.thread};}),d=h.dialog();
  await h.context.codexHandoffStart(d);const job=h.read('codexHandoffUi.jobs.values().next().value');assert.equal(job.status,'unknown');assert.equal(h.context.codexHandoffBusy(job),true);assert.equal(h.context.desktop.drafts.get('source'),'original text');
  completed=true;assert.equal(await h.context.codexHandoffCheck(job),true);assert.deepEqual(h.opened,['destination']);assert.equal(job.status,'success');assert.equal(h.calls.filter(call=>call.method==='thread/handoff').length,1);
});
test('an earlier sidebar write finishes before source placement is remapped',async()=>{
  const waiting=deferred(),h=harness();h.context.codexSidebarUi.write=waiting.promise;const job={thread:{id:'source'},context:h.context.codexThreadContext()};
  const moved=h.context.codexHandoffMoveState(job,result.thread);await settle();assert.equal(h.saved.length,0);waiting.resolve();await moved;assert.equal(h.context.codexSidebarUi.preferences.pinnedOrder[0],JSON.stringify(['local','destination']));
});
test('status recovery never follows a reassigned chat to another host',async()=>{
  const h=harness();h.context.host='beta';const job={thread:{id:'source'},context:{host:'alpha',call:async()=>{throw new Error('old connection');}}};
  await assert.rejects(h.context.codexHandoffCheck(job),/execution host changed/);assert.equal(h.calls.length,0);
});
