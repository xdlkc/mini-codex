import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
const source=readFileSync(new URL('../src/gui_codex_forks.js',import.meta.url),'utf8');
const core=source.slice(0,source.indexOf('const codexForkBaseThreadMenu='));
const plain=value=>JSON.parse(JSON.stringify(value));
function deferred(){let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};}
async function settle(){for(let i=0;i<15;i++)await Promise.resolve();}
function harness(respond=()=>({})){
  const nodes=new Map(),calls=[],opened=[],notices=[],rendered=[];let context;
  class Node{
    constructor(tag='div'){this.tag=tag;this.children=[];this.attrs={};this.style={};this.disabled=false;this.isConnected=true;this.textContent='';}
    append(...nodes){for(const node of nodes){node.parent=this;this.children.push(node);}}
    setAttribute(name,value){this.attrs[name]=value;}
    remove(){if(this.parent)this.parent.children=this.parent.children.filter(node=>node!==this);this.isConnected=false;}
    focus(){context.document.activeElement=this;}
    querySelectorAll(){return this.children.filter(node=>node.tag==='button'&&!node.disabled);}
    getBoundingClientRect(){return {left:50,right:250,top:100,width:300,height:120};}
  }
  const node=id=>{if(!nodes.has(id))nodes.set(id,new Node());return nodes.get(id);};
  context=vm.createContext({Map,Date,JSON,Promise,Error,innerWidth:1200,innerHeight:900,
    document:{body:new Node(),createElement:tag=>new Node(tag)},$:node,
    desktop:{mode:'codex',navigation:1,projectId:null},codexProjectsUi:{intent:1},currentThread:'source',currentModel:'selected-model',currentEffort:'high',currentCwd:'/source',drawerOpen:true,currentDrawerTab:'files',
    codexText:en=>en,
    codexThreadContext:id=>{const host=id==='remote-source'?'alpha':'local';return {host,call:async(method,params,timeout)=>{calls.push({host,method,params:plain(params),timeout});return respond(host,method,params);}};},
    closeFloatingMenu(){},
    openThread:async id=>{context.codexProjectsUi.intent++;context.currentThread=id;opened.push(id);},
    newThread:async()=>{context.codexProjectsUi.intent++;await Promise.resolve();context.desktop.navigation++;context.currentThread=null;context.currentCwd='/unrelated-default';opened.push('new-chat');},
    updateCwdUI:cwd=>context.currentCwd=cwd,updateModelAndEffortUI:()=>rendered.push(context.currentModel),renderDrawerContent:async()=>rendered.push('drawer'),remoteRemember:()=>{},codexRecordView:()=>{},
    toast:(message,action)=>notices.push({message,action}),
    menuButton:(label,run)=>{const button=new Node('button');button.textContent=label;button.run=run;return button;},
  });vm.runInContext(core,context);return {context,node,calls,opened,notices,rendered,read:expression=>vm.runInContext(expression,context)};
}
const result={thread:{id:'forked',mode:'codex',cwd:'/isolated/nested',model:'chosen-model',reasoningEffort:'high'},worktree:{path:'/isolated',cwd:'/isolated/nested'}};
test('a fork uses the selected source model, restores its inspector, and does not move the source draft',async()=>{
  const h=harness(()=>result);h.node('composer-input').value='source draft';await h.context.codexForkChat({id:'source',cwd:'/source'},'newWorktree');
  assert.deepEqual(h.calls[0],{host:'local',method:'thread/fork',params:{threadId:'source',destination:'newWorktree',titleMode:'fork',model:'selected-model',reasoningEffort:'high'},timeout:180000});
  assert.deepEqual(h.opened,['forked']);assert.equal(h.context.currentModel,'chosen-model');assert.equal(h.node('composer-input').value,'source draft');
  assert.deepEqual(h.rendered,['chosen-model','drawer']);assert.equal(h.read('codexForkUi.pending.size'),0);assert.equal(h.node('toast-host').children.length,0);
});
test('a late remote fork remains on its captured host and offers an explicit Open action',async()=>{
  const waiting=deferred(),h=harness(()=>waiting.promise);const pending=h.context.codexForkChat({id:'remote-source',cwd:'/remote'},'newWorktree');
  h.context.codexProjectsUi.intent++;h.context.currentThread='user-selected';h.context.currentModel='other-model';waiting.resolve(result);await pending;
  assert.equal(h.calls[0].host,'alpha');assert.equal(h.calls[0].params.model,undefined);assert.equal(h.context.currentThread,'user-selected');assert.equal(h.context.currentModel,'other-model');assert.deepEqual(h.opened,[]);
  assert.equal(h.notices.length,1);await h.notices[0].action.run();assert.deepEqual(h.opened,['forked']);
});
test('duplicate clicks start one job, and a failed job can be retried',async()=>{
  const waiting=deferred(),h=harness(()=>waiting.promise);
  const first=h.context.codexForkChat({id:'source'}),second=h.context.codexForkChat({id:'source'});assert.equal(h.calls.length,1);
  waiting.reject(new Error('folder unavailable'));const settled=await Promise.allSettled([first,second]);assert.ok(settled.every(item=>item.status==='rejected'));assert.equal(h.read('codexForkUi.pending.size'),0);assert.equal(h.node('toast-host').children.length,0);
  await assert.rejects(h.context.codexForkChat({id:'source'}),/folder unavailable/);assert.equal(h.calls.length,2);
});
test('New chat in this worktree preserves its folder and project without a history-copy RPC',async()=>{
  const h=harness(()=>({thread:{id:'source',cwd:'/worktree/nested',projectId:'bound-project'}}));h.context.currentThread='other';
  await h.context.codexNewChatInWorktree({id:'source'});
  assert.deepEqual(h.calls.map(call=>call.method),['thread/read']);assert.deepEqual(h.opened,['source','new-chat']);assert.equal(h.context.currentThread,null);assert.equal(h.context.currentCwd,'/worktree/nested');assert.equal(h.context.desktop.projectId,'bound-project');
});
test('a slow folder read cannot replace a newer chat or mode',async()=>{
  const waiting=deferred(),h=harness(()=>waiting.promise);const pending=h.context.codexNewChatInWorktree({id:'source'});
  h.context.codexProjectsUi.intent++;h.context.desktop.mode='chat';h.context.currentThread='newer';waiting.resolve({thread:{id:'source',cwd:'/old-worktree'}});await pending;
  assert.deepEqual(h.opened,[]);assert.equal(h.context.currentThread,'newer');
});
test('workspace capabilities distinguish the primary checkout, linked worktree and non-Git folder',async()=>{
  const h=harness((host,method,params)=>method==='workspace/gitStatus'?{isGit:params.cwd!=='/plain'}:{repository:{repoRoot:'/linked'},data:[{path:'/primary',primary:true},{path:'/linked',primary:false}]});
  const context=h.context.codexThreadContext('source');
  assert.equal((await h.context.codexForkWorkspace({cwd:'/linked'},context)).isWorktree,true);
  assert.equal((await h.context.codexForkWorkspace({cwd:'/plain'},context)).isGit,false);
  assert.equal(h.calls.filter(call=>call.method==='workspace/worktree/list').length,1);
  await h.context.codexForkWorkspace({cwd:'/linked'},context);assert.equal(h.calls.length,3);
});
test('closing a fork submenu ignores late workspace lookup results',async()=>{
  const waiting=deferred(),h=harness(()=>waiting.promise),trigger=h.node('trigger');h.context.codexForkSubmenu(trigger,{id:'source',cwd:'/source'});
  assert.equal(trigger.attrs['aria-expanded'],'true');h.context.codexCloseForkSubmenu(true);waiting.resolve({isGit:false});await settle();
  assert.equal(trigger.attrs['aria-expanded'],'false');assert.equal(h.read('codexForkUi.submenu'),null);assert.equal(h.context.document.activeElement,trigger);
});
