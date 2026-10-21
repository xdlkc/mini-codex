#!/usr/bin/env node
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
const sources=['gui_account.js','gui_workspaces.js'].map(file=>readFileSync(new URL('../src/'+file,import.meta.url),'utf8')).join('\n');
const plain=value=>JSON.parse(JSON.stringify(value));
function deferred(){let resolve;const promise=new Promise(yes=>resolve=yes);return {promise,resolve};}
function harness(respond=()=>({})){
  const nodes=new Map(),elements=[],calls=[],errors=[],opened=[];
  function make(tag='div'){
    let id='',markup='';const classes=new Set();
    const node={tag,children:[],attributes:{},value:'',checked:false,style:{},textContent:'',dataset:{},
      classList:{contains:key=>classes.has(key),add:key=>classes.add(key),remove:key=>classes.delete(key),toggle(key,yes){yes?classes.add(key):classes.delete(key);}},
      append(...items){this.children.push(...items);},replaceChildren(...items){this.children=items;},
      setAttribute(key,value){this.attributes[key]=value;},focus(){},after(){},
    };
    Object.defineProperty(node,'id',{get:()=>id,set:value=>{id=value;nodes.set(value,node);}});
    Object.defineProperty(node,'innerHTML',{get:()=>markup,set:value=>{markup=value;node.children=[];for(const match of value.matchAll(/<(\w+)\b[^>]*\bid="([^"]+)"[^>]*>/g)){const child=make(match[1]);child.id=match[2];node.children.push(child);}}});
    elements.push(node);return node;
  }
  const node=id=>{if(!nodes.has(id)){const element=make();element.id=id;}return nodes.get(id);};
  const desktop={navigation:0};
  const context=vm.createContext({
    console,desktop,desktopReady:new Promise(()=>{}),currentSettingsSection:'account',currentCwd:'/original',latestGitStatus:null,drawerOpen:false,currentDrawerTab:null,
    $:node,baseName:path=>path.split('/').at(-1),document:{body:make(),getElementById:id=>nodes.get(id),createElement:make,createTextNode:text=>({textContent:text}),querySelector:()=>make(),querySelectorAll:()=>[]},
    rpcCall(method,params={}){calls.push({method,params});return Promise.resolve(respond(method,params));},
    renderSettingsSection:async()=>{},handleServerNotification(){},appendErrorBanner(){},showError:error=>errors.push(error),toast(){},
    openCustomModal(title,html){node('modal-body').innerHTML=html;},closeModal(){},closeFloatingMenu(){},openSettingsView:async()=>{},
    flushDocument:async()=>{},saveDraft(){},newThread:async()=>opened.push('new-thread'),updateCwdUI:cwd=>{context.currentCwd=cwd;opened.push(cwd);},refreshGitStatus:async()=>{},
    openCreateProjectDialog(){},renderDrawerContent(){},menuButton(){return make('button');},openFloatingMenuAt(){},
  });context.window=context;
  vm.runInContext(sources,context,{filename:'production connection/workspace views'});
  return {context,node,elements,calls,errors,opened,desktop,state:vm.runInContext('accountUi',context)};
}
const disconnected={account:null,credentialSource:null,canChangeCredentials:true,login:null};
const connected={account:{type:'apiKey'},credentialSource:'local',canChangeCredentials:true,login:null};

test('API key form sends the entered key once, clears the password field and never renders it as markup',async()=>{
  let saved=false;const h=harness((method)=>method==='account/read'?(saved?connected:disconnected):saved=true);
  h.state.keyForm=true;await h.context.renderAccountSettings();
  const input=h.node('account-api-key');input.value='local-test-key-only';
  const form=h.elements.find(element=>element.tag==='form');await form.onsubmit({preventDefault(){}});
  const calls=h.calls.filter(call=>call.method==='account/login/start');assert.equal(calls.length,1);
  assert.deepEqual(plain(calls[0].params),{type:'apiKey',apiKey:'local-test-key-only'});assert.equal(input.value,'');
  assert.ok(h.elements.every(element=>!element.innerHTML.includes('local-test-key-only')));assert.equal(h.state.metadata.account.type,'apiKey');
});

test('environment credentials show their source and expose no account mutation controls',async()=>{
  const h=harness(()=>({...connected,credentialSource:'environment',canChangeCredentials:false}));
  await h.context.renderAccountSettings();
  assert.ok(h.elements.some(element=>element.textContent.includes('OPENAI_API_KEY')));
  assert.ok(!h.elements.some(element=>['Sign out','Change API key','Switch ChatGPT account'].includes(element.textContent)));
});

test('pending OAuth shows the returned browser link and cancels only its exact login',async()=>{
  const pending={...disconnected,login:{id:'login-A',status:'pending',authUrl:'https://auth.openai.com/oauth/authorize?state=test'}};
  const h=harness(method=>method==='account/read'?pending:{});await h.context.renderAccountSettings();
  const link=h.elements.find(element=>element.tag==='a');assert.equal(link.href,pending.login.authUrl);assert.equal(link.target,'_blank');
  await h.elements.find(element=>element.textContent==='Cancel sign in').onclick();
  assert.deepEqual(plain(h.calls.find(call=>call.method==='account/login/cancel').params),{loginId:'login-A'});
});

test('worktree creation uses its captured repository and does not replace a newer navigation',async()=>{
  const creation=deferred(),h=harness(method=>method==='workspace/worktree/create'?creation.promise:{});
  await h.context.createWorktreeDialog('/chosen-repository');h.node('worktree-create-name').value='Feature A';h.node('worktree-create-base').value='release';h.node('worktree-create-branch').value='codex/feature-A';
  const request=h.elements.find(element=>element.textContent==='Create worktree').onclick();
  const call=h.calls.find(call=>call.method==='workspace/worktree/create');assert.deepEqual(plain(call.params),{cwd:'/chosen-repository',name:'Feature A',baseRef:'release',branch:'codex/feature-A'});
  h.desktop.navigation++;h.context.currentCwd='/newer';creation.resolve({worktree:{path:'/worktree-A',cwd:'/worktree-A',name:'Feature A'}});await request;
  assert.equal(h.context.currentCwd,'/newer');assert.deepEqual(h.opened,[]);
});

test('archive requires an explicit ignored-file choice and never silently sends discardIgnored',async()=>{
  for(const includeIgnored of [false,true]){
    const h=harness();h.context.archiveWorktreeDialog({id:'worktree-A',path:'/managed/A',repoRoot:'/repo',sourceCwd:'/repo'},'/repo');h.node('worktree-include-ignored').checked=includeIgnored;
    await h.elements.find(element=>element.textContent==='Archive').onclick();
    const call=h.calls.find(call=>call.method==='workspace/worktree/archive');assert.deepEqual(plain(call.params),{cwd:'/repo',id:'worktree-A',includeIgnored});assert.equal(Object.hasOwn(call.params,'discardIgnored'),false);
  }
});

test('worktrees start clean by default and local code changes require an explicit choice',async()=>{
  for(const includeLocalChanges of [false,true]){
    const h=harness(()=>({worktree:{path:'/copied',cwd:'/copied'}}));
    await h.context.createWorktreeDialog('/source');
    const choice=h.node('worktree-include-local-changes');assert.equal(choice.checked,false);
    assert.match(h.node('worktree-local-state-note').textContent,/clean files/);
    choice.checked=includeLocalChanges;choice.onchange();
    if(includeLocalChanges)assert.match(h.node('worktree-local-state-note').textContent,/current HEAD.*Ignored files/);
    await h.elements.find(element=>element.textContent==='Create worktree').onclick();
    const params=plain(h.calls.find(call=>call.method==='workspace/worktree/create').params);
    assert.equal(params.includeLocalChanges,includeLocalChanges?true:undefined);
    assert.equal(params.cwd,'/source');assert.equal(params.baseRef,'HEAD');
  }
});

test('opening a worktree waits for the active document save and respects navigation during that save',async()=>{
  const saving=deferred(),h=harness();h.context.flushDocument=()=>saving.promise;
  const opening=h.context.useWorkspace({path:'/managed/A',cwd:'/managed/A',name:'A'});
  assert.deepEqual(h.opened,[]);h.desktop.navigation++;h.context.currentCwd='/newer';saving.resolve();await opening;
  assert.deepEqual(h.opened,[]);assert.equal(h.context.currentCwd,'/newer');
});

test('same-path hosts cannot publish an old branch list or send its branch switch to the new host',async()=>{
  const listed=deferred(),h=harness();let host='host-A';const remoteCalls=[];
  h.context.captureRpcContext=()=>{const origin=host;return {key:origin,call(method,params){remoteCalls.push({host:origin,method,params});return method==='workspace/branch/list'?listed.promise:Promise.resolve({});}};};
  const showing=h.context.showBranches({});host='host-B';listed.resolve({data:[{name:'feature-from-A',isRemote:false,current:false}]});await showing;
  assert.equal(remoteCalls.length,1);assert.ok(!h.elements.some(element=>element.textContent==='Switch branch'));
});

test('a worktree dialog rejects stale host actions before sending a mutation',async()=>{
  const h=harness();let host='host-A';const remoteCalls=[];
  h.context.captureRpcContext=()=>({key:host,call:async(method,params)=>{remoteCalls.push({method,params});return {};}});
  await h.context.createWorktreeDialog('/workspace');host='host-B';await h.elements.find(element=>element.textContent==='Create worktree').onclick();
  assert.equal(remoteCalls.length,0);assert.match(h.errors[0].message,/environment changed/);
});

test('a workspace cannot install a remote cwd after new-chat navigation changes its host',async()=>{
  const h=harness();let host='host-A';h.context.captureRpcContext=()=>({key:host,call:async()=>({})});h.context.newThread=async()=>{host='local';};
  await assert.rejects(h.context.useWorkspace({cwd:'/remote/worktree'}),/environment changed/);assert.equal(h.context.currentCwd,'/original');
});
