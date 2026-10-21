#!/usr/bin/env node
import assert from 'node:assert/strict';
import {readFileSync,mkdtempSync,mkdirSync,realpathSync,statSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import test from 'node:test';
import vm from 'node:vm';

const source=readFileSync(new URL('../src/gui_codex_projects.js',import.meta.url),'utf8');
const plain=value=>JSON.parse(JSON.stringify(value));
const deferred=()=>{let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};};
async function settle(){for(let n=0;n<20;n++)await Promise.resolve();}
function harness(t,{respond}={}){
  const directory=mkdtempSync(join(tmpdir(),'mini-codex-project-ui-'));t.after(()=>rmSync(directory,{recursive:true,force:true}));
  const paths=Object.fromEntries(['local','alpha','beta'].map(host=>{const path=join(directory,host);mkdirSync(path);return [host,realpathSync(path)];}));
  let document;
  class Node{
    constructor(tag){this.tagName=tag;this.children=[];this.attributes={};this.value='';this.textContent='';this.disabled=false;this.hidden=false;}
    append(...nodes){this.children.push(...nodes);}
    replaceChildren(...nodes){this.children=nodes;}
    setAttribute(name,value){this.attributes[name]=String(value);}removeAttribute(name){delete this.attributes[name];}
    focus(){document.activeElement=this;}select(){}
  }
  const body=new Node('body'),modal=new Node('div'),menu=new Node('div');menu.id='floating-menu';body.append(modal,menu);
  document={activeElement:null,createElement:tag=>new Node(tag),getElementById(id){let found;const visit=node=>{if(node.id===id)found=node;node.children.forEach(visit);};visit(body);return found||null;}};
  const node=id=>document.getElementById(id);
  for(const id of ['composer-input','topbar-title']){const item=new Node('input');item.id=id;body.append(item);}
  const calls=[],errors=[],projects=new Map(['local','alpha','beta'].map(host=>[host,[]])),legacy=new Map();let sequence=0,refreshes=0;
  const remoteUi={active:null,projects:new Map(),threads:new Map(),profiles:[{id:'alpha',name:'Alpha host'},{id:'beta',name:'Beta host'}],connections:new Map()};
  for(const id of ['alpha','beta'])remoteUi.connections.set(id,{id:'connection-'+id,profileId:id,state:'connected'});
  const desktop={mode:'codex',navigation:0,projectId:null,state:{projects:[],settings:{preferences:{}}}};
  async function rpc(host,method,params){
    calls.push({host,method,params:plain(params)});
    const override=respond?.(host,method,params);if(override!==undefined)return override;
    if(method==='workspace/gitStatus'){
      if(!statSync(params.cwd).isDirectory())throw new Error('Project path is not a directory');
      return {cwd:realpathSync(params.cwd),isGit:false};
    }
    if(method==='desktop/project/list')return {items:projects.get(host)};
    if(method==='projects/list')return {projects:legacy.get(host)||[]};
    if(method==='desktop/project/create'){
      const project={...plain(params),id:'project_'+host+'_'+(++sequence)};projects.get(host).push(project);return {project};
    }
    if(method==='desktop/project/update'){
      const project=projects.get(host).find(item=>item.id===params.id);Object.assign(project,plain(params));return {project};
    }
    if(method==='desktop/project/delete'){projects.set(host,projects.get(host).filter(item=>item.id!==params.id));return {};}
    return {};
  }
  const context=vm.createContext({document,desktop,remoteUi,console,Promise,Map,Set,JSON,
    currentCwd:paths.local,currentThread:'original',projectsList:[],
    $:node,baseName:path=>path?.replace(/\/+$/,'').split('/').pop()||'',
    remoteContext:()=>remoteUi.active,
    remoteConnection(host){const connection=remoteUi.connections.get(host);if(connection?.state!=='connected')throw new Error('SSH connection is closed');return connection;},
    remoteLocalRpc:(method,params)=>rpc('local',method,params),
    remoteCall(connection,method,params){if(remoteUi.connections.get(connection.profileId)?.id!==connection.id||connection.state!=='connected')throw new Error('SSH connection is closed or replaced');return rpc(connection.profileId,method,params);},
    remoteMark:(map,id,host)=>map.set(id,host),remoteBind(){},remoteRemember(){},codexRecordView(){},
    saveDraft(){},flushDocument:async()=>{},updateCwdUI:cwd=>{context.currentCwd=cwd;},
    async newThread(keep=true,skip=false){desktop.navigation++;await Promise.resolve();if(!skip)desktop.navigation++;if(!keep)desktop.projectId=null;context.currentThread=null;context.currentCwd=paths[remoteUi.active||'local'];},
    async remoteSelect(host){const navigation=++desktop.navigation;await Promise.resolve();if(navigation!==desktop.navigation)return;desktop.navigation++;remoteUi.active=host;context.currentThread=null;context.currentCwd=paths[host||'local'];},
    async openThread(id){desktop.navigation++;context.currentThread=id;},
    async switchMode(mode){desktop.navigation++;desktop.mode=mode;},
    async refreshThreads(){refreshes++;},
    createDesktopProject:()=>legacy.set('base-create',true),openCreateProjectDialog:()=>legacy.set('base-folder',true),
    openProject:()=>legacy.set('base-open',true),refreshProjects:()=>legacy.set('base-refresh',true),selectProject:()=>legacy.set('base-select',true),moveThreadDialog:()=>legacy.set('base-move',true),
    openCustomModal(title,html){modal.replaceChildren();const match=html.match(/<(form|div) id="([^"]+)"/);if(match){const item=new Node(match[1]);item.id=match[2];modal.append(item);}},
    closeModal(){modal.replaceChildren();},closeFloatingMenu(){},openFloatingMenuAt(){menu.replaceChildren();},
    menuButton(label,run){const button=new Node('button');button.textContent=label;button.onclick=run;return button;},
    showError:error=>errors.push(error.message),
  });
  context.window=context;vm.runInContext(source,context,{filename:'src/gui_codex_projects.js'});
  return {context,desktop,remoteUi,calls,errors,projects,paths,legacy,node,menu,refreshes:()=>refreshes,
    project(host,name='Display name'){const project={id:'project_'+host,name,mode:'codex',path:paths[host],hostId:host};projects.get(host).push(project);remoteUi.projects.set(project.id,host==='local'?null:host);return project;},
    submit(){return node('codex-project-form').onsubmit({preventDefault(){}});},
  };
}

test('opening a project uses its real folder after navigation resets and keeps the project id',async t=>{
  const h=harness(t),project=h.project('local','A label that is not a path');await h.context.openProject(project);
  assert.equal(h.context.currentCwd,h.paths.local);assert.equal(h.desktop.projectId,project.id);assert.equal(h.context.currentThread,null);
  assert.equal(h.node('topbar-title').textContent,project.name);
  assert.deepEqual(h.calls.filter(call=>call.method==='workspace/gitStatus').map(call=>call.params.cwd),[h.paths.local]);
  assert.equal(h.calls.some(call=>call.method==='projects/create'),false);
});

test('opening a remote project validates and selects that host instead of the active host',async t=>{
  const h=harness(t);h.remoteUi.active='beta';const project=h.project('alpha');await h.context.openProject(project);
  assert.equal(h.remoteUi.active,'alpha');assert.equal(h.context.currentCwd,h.paths.alpha);assert.equal(h.desktop.projectId,project.id);
  assert.deepEqual(h.calls.map(call=>[call.host,call.method]),[['alpha','workspace/gitStatus']]);
});

test('a late folder validation cannot replace a newer thread or a different host',async t=>{
  const checking=deferred(),h=harness(t,{respond:(host,method)=>method==='workspace/gitStatus'?checking.promise:undefined});
  const opening=h.context.openProject(h.project('local'));await settle();await h.context.openThread('newer');h.remoteUi.active='beta';h.context.currentCwd=h.paths.beta;
  checking.resolve({cwd:h.paths.local});await opening;
  assert.equal(h.context.currentThread,'newer');assert.equal(h.context.currentCwd,h.paths.beta);assert.equal(h.desktop.projectId,null);
});

test('missing folders report an explicit error and create no project',async t=>{
  const h=harness(t);h.context.createDesktopProject();h.node('codex-project-path').value=join(h.paths.local,'missing');await h.submit();
  assert.match(h.node('codex-project-error').textContent,/ENOENT/);assert.equal(h.node('codex-project-error').hidden,false);
  assert.equal(h.calls.some(call=>call.method==='desktop/project/create'),false);assert.equal(h.projects.get('local').length,0);
});

test('the project dialog captures its chosen connected host and saves its absolute folder',async t=>{
  const h=harness(t);h.context.createDesktopProject();const hosts=h.node('codex-project-host');hosts.value='alpha';hosts.onchange();
  h.node('codex-project-name').value='Remote source';h.node('codex-project-path').value=h.paths.alpha;await h.submit();
  const saved=h.calls.find(call=>call.method==='desktop/project/create');assert.equal(saved.host,'alpha');
  assert.deepEqual(saved.params,{name:'Remote source',mode:'codex',path:h.paths.alpha,hostId:'alpha'});
  assert.equal(h.context.currentCwd,h.paths.alpha);assert.equal(h.remoteUi.active,'alpha');assert.equal(h.desktop.projectId,h.projects.get('alpha')[0].id);
  assert.equal(h.remoteUi.projects.get(h.desktop.projectId),'alpha');
});

test('a late create response remains on its original host without changing the next view',async t=>{
  const creating=deferred(),h=harness(t,{respond:(host,method)=>method==='desktop/project/create'?creating.promise:undefined});
  h.context.createDesktopProject();h.node('codex-project-path').value=h.paths.local;const saving=h.submit();await settle();
  assert.ok(h.calls.some(call=>call.method==='desktop/project/create'));await h.context.remoteSelect('beta');h.context.currentThread='beta-thread';
  creating.resolve({project:{id:'project_saved_local',name:'Local',mode:'codex',path:h.paths.local,hostId:'local'}});await saving;
  assert.equal(h.remoteUi.active,'beta');assert.equal(h.context.currentCwd,h.paths.beta);assert.equal(h.context.currentThread,'beta-thread');assert.equal(h.desktop.projectId,null);
  assert.equal(h.calls.filter(call=>call.method==='desktop/project/create').length,1);
});

test('the project picker reuses a saved binding and never registers a folder on another host',async t=>{
  const h=harness(t);h.remoteUi.active='alpha';const project=h.project('alpha');await h.context.refreshProjects();
  assert.equal(h.context.projectsList[0].id,project.id);await h.context.selectProject(h.paths.alpha);
  assert.equal(h.desktop.projectId,project.id);assert.equal(h.context.currentCwd,h.paths.alpha);
  assert.equal(h.calls.some(call=>call.method==='desktop/project/create'||call.method==='projects/create'),false);
  assert.ok(h.calls.every(call=>call.host==='alpha'));
});

test('moving an aggregated remote thread only lists and updates projects from the thread host',async t=>{
  const h=harness(t),alpha=h.project('alpha');h.project('local');h.remoteUi.threads.set('alpha-thread','alpha');
  await h.context.moveThreadDialog({id:'alpha-thread',mode:'codex'});
  const options=h.node('codex-move-project-options').children;assert.deepEqual(options.map(option=>option.textContent),['No project',alpha.name]);
  await options[1].onclick();const mutation=h.calls.find(call=>call.method==='desktop/threadMetadata/set');
  assert.equal(mutation.host,'alpha');assert.deepEqual(mutation.params,{threadId:'alpha-thread',projectId:alpha.id});
});

test('Chat and Work keep their existing project flows',async t=>{
  const h=harness(t);for(const mode of ['chat','work']){
    h.desktop.mode=mode;await h.context.createDesktopProject();await h.context.openCreateProjectDialog('new');await h.context.openProject({mode,name:'Original'});
    await h.context.refreshProjects();await h.context.selectProject('/unused');await h.context.moveThreadDialog({mode});
  }
  assert.equal(h.calls.length,0);for(const name of ['base-create','base-folder','base-open','base-refresh','base-select','base-move'])assert.equal(h.legacy.get(name),true);
});
