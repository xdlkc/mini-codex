#!/usr/bin/env node
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
const source=readFileSync(new URL('../src/gui_models.js',import.meta.url),'utf8');
const plain=value=>JSON.parse(JSON.stringify(value));
function deferred(){let resolve,reject;const promise=new Promise((a,b)=>{resolve=a;reject=b;});return {promise,resolve,reject};}
function harness(respond=()=>({})){
  const calls=[],nodes=new Map();
  const element=()=>({classList:{remove(){}},textContent:'',append(){}});
  const context=vm.createContext({console,Date,desktop:{mode:'codex'},currentCwd:'/local',modelCatalog:[],currentModel:'original',currentEffort:'medium',fastMode:false,EFFORT_LABELS:{medium:'Medium'},
    $:id=>{if(!nodes.has(id))nodes.set(id,element());return nodes.get(id);},document:{createElement:element},
    rpcCall(method,params={}){calls.push({method,params});return Promise.resolve(respond(method,params));},
    closeFloatingMenu(){},renderSettingsSection:async()=>{},currentSettingsSection:'general',handleServerNotification(){},updateModelAndEffortUI(){},showError(){},
  });vm.runInContext(source,context);return {context,calls};
}
test('unknown models expose Auto and an explicitly labeled manual setting, without invented levels',()=>{
  const h=harness(),options=plain(h.context.modelEffortOptions({supportedReasoningEfforts:[],reasoningMetadataKnown:false}));
  assert.deepEqual(options.map(v=>v.reasoningEffort),['auto','medium']);assert.match(options[1].description,/not confirmed/);
});
test('selecting an unknown model sends Auto instead of inheriting another model effort',async()=>{
  const h=harness();h.context.modelCatalog=[{model:'unknown',defaultReasoningEffort:'auto',supportedReasoningEfforts:[]}];await h.context.selectModel('unknown');
  assert.deepEqual(plain(h.calls[0]),{method:'settings/write',params:{model:'unknown',reasoningEffort:'auto',serviceTier:'default'}});assert.equal(h.context.currentModel,'unknown');assert.equal(h.context.currentEffort,'auto');
});
test('a reported model default is used when the previous effort is unsupported',async()=>{
  const h=harness();h.context.modelCatalog=[{model:'reported',defaultReasoningEffort:'high',supportedReasoningEfforts:[{reasoningEffort:'low'},{reasoningEffort:'high'}]}];await h.context.selectModel('reported');assert.equal(h.context.currentEffort,'high');
  await assert.rejects(h.context.selectEffort('xhigh'),/not reported/);assert.equal(h.calls.length,1);
});
test('a failed settings write preserves the active model and effort',async()=>{
  const h=harness(()=>Promise.reject(new Error('disk full')));await assert.rejects(h.context.selectModel('new'),/disk full/);assert.equal(h.context.currentModel,'original');assert.equal(h.context.currentEffort,'medium');
});
test('an older model refresh cannot overwrite a newer result or a different host context',async()=>{
  const first=deferred(),second=deferred();let count=0;const h=harness(()=>++count===1?first.promise:second.promise);
  const a=h.context.refreshModelCatalog(true,true),b=h.context.refreshModelCatalog(true,true);second.resolve({data:[{model:'new'}],defaultModel:'new',defaultReasoningEffort:'auto'});await b;first.resolve({data:[{model:'old'}],defaultModel:'old'});await a;assert.equal(h.context.currentModel,'new');
  const third=deferred();h.context.rpcCall=()=>third.promise;const c=h.context.refreshModelCatalog(true,true);h.context.currentCwd='/remote';third.resolve({data:[{model:'late'}],defaultModel:'late'});await c;assert.equal(h.context.currentModel,'new');
});
test('a late settings save cannot replace the model shown for a newly selected host',async()=>{
  const saved=deferred(),h=harness(()=>saved.promise),work=h.context.selectModel('old-host-model');h.context.currentCwd='/other-host';saved.resolve({});await work;assert.equal(h.context.currentModel,'original');
});
test('a catalog refresh started before a model selection updates metadata without reverting that selection',async()=>{
  const loading=deferred(),h=harness(method=>method==='model/list'?loading.promise:{});
  const refresh=h.context.refreshModelCatalog(true,true);await h.context.selectModel('chosen');
  loading.resolve({data:[{model:'old'},{model:'chosen'}],defaultModel:'old',defaultReasoningEffort:'high'});await refresh;
  assert.equal(h.context.currentModel,'chosen');assert.equal(h.context.currentEffort,'auto');assert.equal(h.context.modelCatalog.length,2);
});

test('Codex reported levels include Ultra, omit synthetic Auto, and keep Luna at its reported maximum',async()=>{
  const h=harness(),model={model:'sol',reasoningMetadataKnown:true,defaultReasoningEffort:'low',supportedReasoningEfforts:['low','medium','high','xhigh','max','ultra'].map(reasoningEffort=>({reasoningEffort}))};
  h.context.modelCatalog=[model];h.context.currentModel='sol';assert.deepEqual(plain(h.context.modelEffortOptions(model)).map(v=>v.reasoningEffort),['low','medium','high','xhigh','max','ultra']);
  await h.context.selectEffort('ultra');assert.equal(h.context.currentEffort,'ultra');assert.equal(h.calls.at(-1).params.reasoningEffort,'ultra');
  h.context.modelCatalog=[{...model,model:'luna',supportedReasoningEfforts:model.supportedReasoningEfforts.slice(0,-1)}];await h.context.selectModel('luna');assert.equal(h.context.currentEffort,'low');await assert.rejects(h.context.selectEffort('ultra'),/not reported/);
});
test('Fast persists the Codex service tier and failed writes leave the existing speed unchanged',async()=>{
  const h=harness();h.context.modelCatalog=[{model:'original',serviceTiers:[{id:'priority',name:'Fast'}]}];
  await h.context.toggleFastMode();assert.equal(h.calls.at(-1).params.serviceTier,'priority');assert.equal(h.context.fastMode,true);
  await h.context.toggleFastMode();assert.equal(h.calls.at(-1).params.serviceTier,'default');assert.equal(h.context.fastMode,false);
  h.context.rpcCall=async()=>{throw new Error('disk full');};await assert.rejects(h.context.toggleFastMode(),/disk full/);assert.equal(h.context.fastMode,false);
});
test('rapid model writes are serialized so an old save cannot replace the latest disk selection',async()=>{
  const first=deferred(),h=harness(()=>h.calls.length===1?first.promise:{});const a=h.context.selectModel('first'),b=h.context.selectModel('second');
  for(let i=0;i<5;i++)await Promise.resolve();assert.equal(h.calls.length,1);assert.equal(h.calls[0].params.model,'first');
  first.resolve({});await Promise.all([a,b]);assert.equal(h.calls.at(-1).params.model,'second');assert.equal(h.context.currentModel,'second');
});
test('same-path remote hosts have separate model refresh contexts',async()=>{
  const waiting=deferred(),h=harness(()=>waiting.promise);let profile='A';h.context.remoteContext=()=>profile;const loading=h.context.refreshModelCatalog(true,true);profile='B';
  waiting.resolve({data:[{model:'host-A'}],defaultModel:'host-A'});await loading;assert.equal(h.context.currentModel,'original');
});
test('speed changes follow the same host across modes and folders',async()=>{
  const h=harness();h.context.modelCatalog=[{model:'original',serviceTiers:[{id:'priority',name:'Fast'}]}];await h.context.toggleFastMode();h.context.desktop.mode='work';h.context.currentCwd='/another';await h.context.toggleFastMode();h.context.desktop.mode='codex';h.context.currentCwd='/local';assert.equal(h.context.selectedServiceTier(),'default');assert.equal(h.context.fastMode,false);
});
