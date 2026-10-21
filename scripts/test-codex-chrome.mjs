import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source=readFileSync(new URL('../src/gui_codex.js',import.meta.url),'utf8');
const historySource=source.slice(0,source.indexOf('function codexSyncChrome('));
const archiveSource=source.slice(source.indexOf('function codexThreadContext('),source.indexOf('const codexBaseModelPicker='));
const plain=value=>JSON.parse(JSON.stringify(value));
function historyHarness(){
  const buttons=new Map(),classes=new Set(),calls=[];
  const context=vm.createContext({JSON,Date,Promise,Error,navigator:{language:'en'},EFFORT_LABELS:{},
    currentThread:null,currentCwd:'/first',currentSettingsSection:null,
    desktop:{mode:'codex',navigation:0,projectId:null,state:{settings:{preferences:{}}}},
    document:{body:{classList:{contains:value=>classes.has(value)}}},
    $:id=>{if(!buttons.has(id))buttons.set(id,{});return buttons.get(id);},
    remoteContext:()=>context.profile||null,
    switchMode:async mode=>{context.desktop.mode=mode;context.desktop.navigation++;},
    openThread:async id=>{context.desktop.navigation++;context.currentThread=id;context.currentCwd='/'+id;classes.clear();calls.push(['open',id]);},
    remoteSelect:async profile=>{context.profile=profile;context.currentThread=null;context.desktop.navigation++;},
    newThread:async()=>{context.desktop.navigation++;context.currentThread=null;classes.clear();calls.push(['new']);},
    updateCwdUI:cwd=>{context.currentCwd=cwd;},
    openSettingsView:async section=>{context.currentSettingsSection=section;classes.add('in-settings');},
  });
  vm.runInContext(historySource+'\ncodexChrome.mounted=true;',context);
  return {context,calls,read:()=>plain(vm.runInContext('codexChrome',context)),record:()=>context.codexRecordView()};
}
test('Back and Forward restore execution folders and do not create duplicate entries',async()=>{
  const h=historyHarness();h.record();h.record();assert.equal(h.read().history.length,1);
  h.context.currentThread='second';h.context.currentCwd='/second';h.record();
  await h.context.codexNavigateHistory(-1);assert.equal(h.context.currentThread,null);assert.equal(h.context.currentCwd,'/first');
  await h.context.codexNavigateHistory(1);assert.equal(h.context.currentThread,'second');assert.equal(h.read().position,1);assert.equal(h.read().history.length,2);
});
test('the window title follows the selected chat and clears when returning home',()=>{
  const h=historyHarness();h.context.currentThread='chat';h.context.$('topbar-title').textContent='Selected chat';h.context.codexUpdateDocumentTitle();assert.equal(h.context.document.title,'Selected chat');
  h.context.$('topbar-title').textContent='Renamed chat';h.context.codexUpdateDocumentTitle();assert.equal(h.context.document.title,'Renamed chat');h.context.currentThread=null;h.context.codexUpdateDocumentTitle();assert.equal(h.context.document.title,'Codex');
});
test('a new destination after Back discards only the Forward branch',async()=>{
  const h=historyHarness();h.record();h.context.currentThread='second';h.record();await h.context.codexNavigateHistory(-1);
  h.context.currentThread='third';h.record();assert.deepEqual(h.read().history.map(view=>view.threadId),[null,'third']);
});
test('a failed history navigation keeps the current cursor and permits another attempt',async()=>{
  const h=historyHarness();h.context.currentThread='first';h.record();h.context.currentThread='second';h.record();
  h.context.openThread=async()=>{throw new Error('host disconnected');};
  await assert.rejects(h.context.codexNavigateHistory(-1),/host disconnected/);assert.equal(h.read().position,1);assert.equal(h.read().replaying,false);
});
test('a user navigation during a pending Back remains visible and is recorded',async()=>{
  const h=historyHarness();h.context.currentThread='first';h.record();h.context.currentThread='second';h.record();let release;
  h.context.openThread=async()=>{h.context.desktop.navigation++;await new Promise(resolve=>release=resolve);};
  const pending=h.context.codexNavigateHistory(-1);h.context.currentThread='user-selected';h.context.desktop.navigation++;release();await pending;
  assert.equal(h.context.currentThread,'user-selected');assert.equal(h.read().history.at(-1).threadId,'user-selected');assert.equal(h.read().replaying,false);
});
test('a blank history entry restores its host before its folder',async()=>{
  const h=historyHarness();h.context.profile='ssh-a';h.context.currentCwd='/remote/a';h.record();h.context.profile=null;h.context.currentCwd='/local';h.record();
  await h.context.codexNavigateHistory(-1);assert.equal(h.context.profile,'ssh-a');assert.equal(h.context.currentCwd,'/remote/a');
});
test('an internal navigation increment after a document flush preserves Forward history',async()=>{
  const h=historyHarness();h.context.codexProjectsUi={intent:0};h.record();h.context.currentThread='second';h.context.currentCwd='/second';h.record();
  h.context.newThread=async()=>{h.context.codexProjectsUi.intent++;await Promise.resolve();h.context.desktop.navigation++;h.context.currentThread=null;};
  await h.context.codexNavigateHistory(-1);assert.equal(h.read().position,0);assert.equal(h.read().history.length,2);
  await h.context.codexNavigateHistory(1);assert.equal(h.context.currentThread,'second');assert.equal(h.read().position,1);
});

function archiveHarness({confirm=true,failArchive=false,failRefresh=false}={}){
  const calls=[],tasks=[{id:'follow-up',targetThreadId:'chat',state:'active'},{id:'other-task',targetThreadId:'other',state:'active'}];
  const context=vm.createContext({Map,Set,Promise,Date,Error,setTimeout,currentThread:'chat',
    desktop:{mode:'codex',navigation:1,tabs:[{id:'chat'},{id:'other'}]},
    remoteUi:{ambiguous:new Set(),threads:new Map([['chat','ssh-a']])},
    remoteConnection:id=>({id:'connection-a',profileId:id}),remoteCall:async(connection,method,params)=>{
      calls.push({host:connection.profileId,method,params:plain(params)});
      if(method==='thread/read')return {thread:{id:'chat',status:{type:'idle'}}};
      if(method==='scheduler/list')return {data:plain(tasks)};
      if(method==='scheduler/update'){Object.assign(tasks.find(task=>task.id===params.id),params);return {};}
      if(method==='thread/archive'&&failArchive)throw new Error('archive failed');
      return {};
    },
    rpcLocalCall:()=>{throw new Error('unexpected local RPC');},
    codexText:en=>en,threadMenu(){},archiveThread:async()=>{throw new Error('unexpected ordinary archive');},
    scheduledConfirm:async()=>confirm,newThread:async()=>{context.currentThread=null;},
    refreshThreads:async()=>{if(failRefresh)throw new Error('refresh failed');},toast:(message,action)=>{context.undo=action.run;},
  });vm.runInContext(archiveSource,context);return {context,calls,tasks};
}
test('canceling an archive confirmation leaves the chat and all follow-ups unchanged',async()=>{
  const h=archiveHarness({confirm:false});await h.context.archiveThread('chat');
  assert.equal(h.calls.some(call=>call.method==='scheduler/update'||call.method==='thread/archive'),false);assert.equal(h.context.currentThread,'chat');
});
test('archive delegates heartbeat deletion to the original SSH host; Undo restores only the chat',async()=>{
  const h=archiveHarness();await h.context.archiveThread('chat');
  assert.ok(h.calls.some(call=>call.method==='thread/archive'));assert.ok(h.calls.every(call=>call.host==='ssh-a'));
  assert.equal(h.calls.some(call=>call.method==='scheduler/update'),false);
  assert.deepEqual(h.context.desktop.tabs.map(tab=>tab.id),['other']);await h.context.undo();
  assert.equal(h.calls.at(-1).method,'thread/unarchive');assert.equal(h.calls.some(call=>call.method==='scheduler/create'||call.method==='scheduler/update'),false);
});
test('archive and refresh errors never fabricate heartbeat edits or restoration',async()=>{
  const failed=archiveHarness({failArchive:true});await assert.rejects(failed.context.archiveThread('chat'),/archive failed/);assert.equal(failed.tasks[0].state,'active');
  const refresh=archiveHarness({failRefresh:true});await assert.rejects(refresh.context.archiveThread('chat'),/refresh failed/);
  for(const h of [failed,refresh])assert.equal(h.calls.some(call=>call.method==='scheduler/update'),false);
});
test('an ambiguous thread is blocked before any read or mutation',async()=>{
  const h=archiveHarness();h.context.remoteUi.ambiguous.add('chat');await assert.rejects(h.context.archiveThread('chat'),/more than one host/);assert.equal(h.calls.length,0);
});
