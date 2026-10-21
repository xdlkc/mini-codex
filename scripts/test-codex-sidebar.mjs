#!/usr/bin/env node
// Execute the production sidebar with real async ordering and a small DOM model.
import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFileSync} from 'node:fs';

const source=readFileSync(new URL('../src/gui_codex_sidebar.js',import.meta.url),'utf8');
const plain=value=>JSON.parse(JSON.stringify(value));
const deferred=()=>{let resolve,reject;const promise=new Promise((a,b)=>{resolve=a;reject=b;});return {promise,resolve,reject};};
async function settle(){for(let i=0;i<15;i++)await Promise.resolve();}
function harness({preferences={},read=async()=>({data:[]})}={}){
  let document;
  class Node{
    constructor(tag){this.tagName=tag;this.children=[];this.dataset={};this.attributes={};this.events={};this.parentElement=null;this._text='';this._classes=new Set();this.classList={add:(...names)=>names.forEach(n=>this._classes.add(n)),remove:(...names)=>names.forEach(n=>this._classes.delete(n)),contains:n=>this._classes.has(n)};}
    set className(value){this._classes=new Set(value.split(/\s+/).filter(Boolean));}get className(){return [...this._classes].join(' ');}
    set textContent(value){this._text=String(value);this.children=[];}get textContent(){return this._text+this.children.map(c=>c.textContent).join('');}
    append(...nodes){for(const node of nodes){node.parentElement=this;this.children.push(node);}}
    replaceChildren(...nodes){for(const child of this.children)child.parentElement=null;this.children=[];this._text='';this.append(...nodes);}
    remove(){if(this.parentElement){this.parentElement.children=this.parentElement.children.filter(node=>node!==this);this.parentElement=null;}}
    setAttribute(name,value){this.attributes[name]=String(value);}getAttribute(name){return this.attributes[name];}removeAttribute(name){delete this.attributes[name];}
    addEventListener(name,callback){(this.events[name]??=[]).push(callback);}dispatch(name,event){for(const callback of this.events[name]||[])callback(event);}
    focus(){document.activeElement=this;}
    contains(target){return this===target||this.children.some(child=>child.contains(target));}
    querySelectorAll(selector){const found=[];const matches=node=>{
      if(selector==='button:not(:disabled)')return node.tagName==='button'&&!node.disabled;
      if(selector==='button')return node.tagName==='button';
      if(selector.startsWith('.'))return node.classList.contains(selector.slice(1));
      if(selector==='[data-codex-group]')return node.dataset.codexGroup!==undefined;
      if(selector==='[data-codex-thread-key]')return node.dataset.codexThreadKey!==undefined;
      if(selector==='[aria-checked="true"]')return node.attributes['aria-checked']==='true';
      return false;
    };const visit=node=>{for(const child of node.children){if(matches(child))found.push(child);visit(child);}};visit(this);return found;}
    querySelector(selector){return this.querySelectorAll(selector)[0]||null;}
  }
  const body=new Node('body');
  document={body,activeElement:null,createElement:tag=>new Node(tag),getElementById:id=>{
    let found;const visit=node=>{if(node.id===id)found=node;for(const child of node.children)visit(child);};visit(body);return found||null;
  },querySelectorAll:selector=>body.querySelectorAll(selector),querySelector:selector=>body.querySelector(selector)};
  const create=(id,parent=body)=>{const node=new Node('div');node.id=id;parent.append(node);return node;};
  const heading=create('heading');create('sidebar-section-title',heading);create('thread-count',heading);const list=create('thread-list'),menu=create('floating-menu'),modalBody=create('modal-body');
  const calls=[],errors=[],writes=[],data=new Map(),menuCalls=[],openedProjects=[],modals=[],remoteUi={active:null,profiles:[],connections:new Map(),projects:new Map(),threads:new Map(),ambiguous:new Set()};let baseFailure=null;
  const desktop={mode:'codex',threads:[],state:{projects:[],settings:{preferences:{codexSidebar:preferences}}}};
  const context=vm.createContext({document,desktop,remoteUi,Promise,Map,Set,JSON,Number,Date,
    $:id=>document.getElementById(id),remoteContext:()=>remoteUi.active,
    showError:error=>errors.push(error.message),
    async savePreferences(patch){writes.push(plain(patch));desktop.state.settings.preferences={...desktop.state.settings.preferences,...patch};},
    async remoteLocalRpc(method,params,timeout){calls.push({host:'local',method,params,timeout});return read('local',method,params);},
    async remoteCall(connection,method,params,timeout){calls.push({host:connection.profileId,method,params,timeout});const result=await read(connection.profileId,method,params);for(const t of result.data||[])remoteUi.threads.set(t.id,connection.profileId);return result;},
    remoteBind(result,host){for(const t of result.data||[])remoteUi.threads.set(t.id,host);},
    threadRow(thread){const row=new Node('div');row.className='thread-row';row.textContent=thread.name||thread.id;row.threadId=thread.id;return row;},
    threadMenu(event,thread){menuCalls.push(thread.id);menu.replaceChildren();const button=new Node('button');button.textContent='Original chat action';menu.append(button);menu.classList.add('open');return 'original menu';},
    openCustomModal(title){modals.push(title);modalBody.replaceChildren();},closeModal(){modalBody.replaceChildren();},
    async openProject(project){openedProjects.push(project.id);},
    codexProjectMenu(project){menu.replaceChildren();const button=new Node('button');button.textContent='Original project action';menu.append(button);menu.classList.add('open');return menu;},
    openFloatingMenuAt(){menu.replaceChildren();menu.classList.add('open');},closeFloatingMenu(){menu.classList.remove('open');},
    async refreshThreads(){if(baseFailure)throw baseFailure;list.replaceChildren();const base=new Node('span');base.textContent='base '+desktop.mode;list.append(base);desktop.threads=data.get(remoteUi.active||'local')||[];return 'base result';},
  });
  vm.runInContext(source,context,{filename:'src/gui_codex_sidebar.js'});
  return {context,desktop,remoteUi,data,document,list,menu,calls,errors,writes,menuCalls,openedProjects,modals,run:code=>vm.runInContext(code,context),node:id=>document.getElementById(id),
    connect(id,name=id){remoteUi.profiles.push({id,name});remoteUi.connections.set(id,{id:'conn-'+id,profileId:id,state:'connected'});},
    failRefresh(error){baseFailure=error;},
    groups:()=>list.children.filter(node=>node.dataset.codexGroup!==undefined),
  };
}
const thread=(id,patch={})=>({id,name:id,mode:'codex',cwd:'/same/project',createdAt:1,updatedAt:1,...patch});

test('sorting handles timestamps and keeps pinned order independent from chat order',async()=>{
  const h=harness({preferences:{grouping:'list',sort:'created_at',pinnedSort:'manual',pinnedOrder:['["local","p2"]','["local","p1"]']}});
  h.data.set('local',[thread('old',{createdAt:1,updatedAt:400}),thread('new',{createdAt:2,updatedAt:3}),thread('p1',{pinned:true,updatedAt:100}),thread('p2',{pinned:true,updatedAt:2}),thread('work',{mode:'work'}),thread('missing',{mode:undefined})]);
  await h.context.refreshThreads();
  const groups=h.groups();assert.deepEqual(groups.map(g=>g.dataset.codexGroup),['pinned','chats']);
  assert.deepEqual(groups[0].querySelectorAll('[data-codex-thread-key]').map(n=>n.threadId),['p2','p1']);
  assert.deepEqual(groups[1].querySelectorAll('[data-codex-thread-key]').map(n=>n.threadId),['new','old']);
  assert.equal(h.run('codexSidebarTimestamp(1700000000)'),1700000000000);
  assert.equal(h.run('codexSidebarTimestamp("2024-01-01T00:00:00Z")'),1704067200000);
});

test('connection and project grouping never merge hosts with the same cwd',()=>{
  const h=harness({preferences:{grouping:'project'}});h.connect('a','Build host');
  h.run(`codexSidebarRemember('local',[${JSON.stringify(thread('local'))}]);codexSidebarRemember('a',[${JSON.stringify(thread('remote'))}]);renderCodexSidebar();`);
  assert.equal(h.groups().length,2);assert.notEqual(h.groups()[0].dataset.codexGroup,h.groups()[1].dataset.codexGroup);
  const grouped=Object.fromEntries(h.groups().map(g=>[JSON.parse(g.dataset.codexGroup)[0],g.querySelectorAll('[data-codex-thread-key]').map(n=>n.threadId)]));assert.deepEqual(grouped,{local:['local'],a:['remote']});
  h.run(`codexSidebarSave({grouping:'connection'});`);
  assert.deepEqual(h.groups().map(g=>g.dataset.codexGroup),['connection:local','connection:a']);
});

test('Chat and Work keep the original refresh result and do not query other hosts',async()=>{
  for(const mode of ['chat','work']){const h=harness();h.connect('a');h.desktop.mode=mode;const result=await h.context.refreshThreads();assert.equal(result,'base result');assert.equal(h.list.textContent,'base '+mode);assert.equal(h.calls.length,0);assert.equal(h.node('codex-sidebar-options'),null);}
});

test('local rows render before a slow remote finishes and failed hosts retain cached rows',async()=>{
  const slow=deferred(),h=harness({preferences:{grouping:'connection'},read:host=>host==='a'?slow.promise:Promise.reject(new Error('Host unavailable'))});h.connect('a');h.connect('b');
  h.run(`codexSidebarRemember('b',[${JSON.stringify(thread('cached-b'))}]);`);h.data.set('local',[thread('local')]);
  await h.context.refreshThreads();assert.equal(h.groups().find(g=>g.dataset.codexGroup==='connection:local').querySelector('[data-codex-thread-key]').threadId,'local');
  assert.equal(h.calls.filter(c=>c.host==='a').length,1);assert.ok(h.calls.every(c=>c.timeout<=2500));
  slow.resolve({data:[thread('remote-a'),thread('work-a',{mode:'work'})]});await settle();
  assert.deepEqual(h.groups().map(g=>g.dataset.codexGroup),['connection:b','connection:local','connection:a']);
  const cached=h.groups().find(g=>g.dataset.codexGroup==='connection:b');assert.equal(cached.querySelector('[data-codex-thread-key]').threadId,'cached-b');assert.match(cached.textContent,/Could not refresh/);
  assert.equal(h.desktop.threads.some(t=>t.id==='work-a'),false);assert.equal(h.remoteUi.threads.get('remote-a'),'a');
});

test('stale host reads cannot replace a newer navigation result',async()=>{
  const remote=deferred(),h=harness({preferences:{grouping:'connection'},read:()=>remote.promise});h.connect('a');h.data.set('local',[thread('local')]);
  await h.context.refreshThreads();h.desktop.mode='chat';await h.context.refreshThreads();remote.resolve({data:[thread('too-late')]});await settle();
  assert.equal(h.list.textContent,'base chat');assert.equal(h.run(`codexSidebarUi.cache.has('a')`),false);
});

test('collapse and Show more survive refresh and do not mutate thread lifecycle metadata',async()=>{
  const h=harness({preferences:{grouping:'list'}});h.data.set('local',Array.from({length:14},(_,i)=>thread('t'+i,{updatedAt:i+1})));
  await h.context.refreshThreads();assert.equal(h.list.querySelectorAll('[data-codex-thread-key]').length,5);
  h.list.querySelector('.codex-sidebar-show-more').onclick();await settle();assert.equal(h.list.querySelectorAll('[data-codex-thread-key]').length,10);
  h.list.querySelector('.codex-sidebar-show-more').onclick();await settle();assert.equal(h.list.querySelectorAll('[data-codex-thread-key]').length,14);
  h.groups()[0].querySelector('button').onclick();await settle();assert.equal(h.list.querySelectorAll('[data-codex-thread-key]').length,0);
  await h.context.refreshThreads();assert.equal(h.groups()[0].querySelector('button').getAttribute('aria-expanded'),'false');
  assert.equal(h.writes.at(-1).codexSidebar.collapsed.chats,true);assert.equal(h.writes.at(-1).codexSidebar.limits.chats,15);assert.equal(h.calls.length,0);
});

test('keyboard reordering changes manual chat order without changing pinned order',async()=>{
  const h=harness({preferences:{grouping:'list',sort:'manual',pinnedSort:'manual'}});h.data.set('local',[thread('a',{updatedAt:3}),thread('b',{updatedAt:2}),thread('p',{pinned:true})]);await h.context.refreshThreads();
  const row=h.groups()[1].querySelectorAll('[data-codex-thread-key]')[0];let prevented=false;row.dispatch('keydown',{target:row,key:'ArrowDown',altKey:true,preventDefault(){prevented=true;}});await settle();
  assert.ok(prevented);assert.deepEqual(h.groups()[1].querySelectorAll('[data-codex-thread-key]').map(n=>n.threadId),['b','a']);
  assert.deepEqual(h.writes.at(-1).codexSidebar.pinnedOrder,[]);assert.equal(h.document.activeElement.threadId,'a');
});

test('radio menus support keyboard focus and restore shared menu semantics',async()=>{
  const h=harness({preferences:{grouping:'project'}});h.data.set('local',[thread('a')]);await h.context.refreshThreads();const anchor=h.node('codex-sidebar-options');anchor.onclick();
  assert.equal(h.menu.getAttribute('role'),'menu');const radios=h.menu.querySelectorAll('button').filter(button=>button.getAttribute('role')==='menuitemradio');assert.equal(radios.filter(button=>button.getAttribute('aria-checked')==='true').length,2);assert.deepEqual(radios.map(button=>button.textContent.replace(/^✓/,'')),['By project','By connection','In one list','Last updated','Created','Manual order']);assert.equal(h.document.activeElement.textContent,'✓By project');
  let prevented=false;h.menu.onkeydown({key:'ArrowDown',preventDefault(){prevented=true;}});assert.ok(prevented);assert.equal(h.document.activeElement.textContent,'By connection');
  h.document.activeElement.onclick();await settle();assert.equal(h.writes.at(-1).codexSidebar.grouping,'connection');assert.equal(h.document.activeElement,h.node('codex-sidebar-options'));
  anchor.onclick();h.context.openFloatingMenuAt('other',anchor,'',false);assert.equal(h.menu.getAttribute('role'),undefined);assert.equal(h.menu.onkeydown,null);
});

test('Chinese labels follow the shared codexText function and Show less restores the row limit',async()=>{
  const h=harness({preferences:{grouping:'connection'}});h.context.codexText=(_en,zh)=>zh;h.data.set('local',Array.from({length:11},(_,i)=>thread('t'+i)));
  await h.context.refreshThreads();assert.equal(h.node('sidebar-section-title').textContent,'连接');assert.match(h.groups()[0].textContent,/本地/);
  h.list.querySelector('.codex-sidebar-show-more').onclick();await settle();h.list.querySelector('.codex-sidebar-show-more').onclick();await settle();assert.equal(h.list.querySelector('.codex-sidebar-show-more').textContent,'显示更少');
  h.list.querySelector('.codex-sidebar-show-more').onclick();await settle();assert.equal(h.list.querySelectorAll('[data-codex-thread-key]').length,5);
  h.node('codex-sidebar-options').onclick();assert.match(h.menu.textContent,/按项目/);assert.match(h.menu.textContent,/最后更新/);assert.match(h.menu.textContent,/手动排序/);
  h.context.closeFloatingMenu();assert.equal(h.document.activeElement,h.node('codex-sidebar-options'),'the document Escape handler can close the menu before its own handler');
});

test('the reference defaults to connection grouping and reuses existing icon assets',async()=>{
  const h=harness();h.context.CHEV_SVG='<svg data-source="chevron"></svg>';h.context.FOLDER_SVG='<svg data-source="folder"></svg>';h.context.ICONS={term:'<svg data-source="terminal"></svg>'};h.context.shellSvg=name=>'<svg data-source="'+name+'"></svg>';
  h.data.set('local',[thread('a')]);await h.context.refreshThreads();assert.equal(h.run('codexSidebarPreferences().grouping'),'connection');assert.equal(h.groups()[0].dataset.codexGroup,'connection:local');
  assert.equal(h.run(`codexSidebarIcon('folder').innerHTML`),h.context.FOLDER_SVG);assert.equal(h.run(`codexSidebarIcon('chevron').innerHTML`),h.context.CHEV_SVG);assert.equal(h.run(`codexSidebarIcon('connection').innerHTML`),h.context.ICONS.term);assert.match(h.run(`codexSidebarIcon('options').innerHTML`),/data-source="more"/);
});

test('an active host refresh failure keeps its last rows and reports that host error',async()=>{
  const h=harness();h.data.set('local',[thread('kept')]);await h.context.refreshThreads();h.failRefresh(new Error('Local backend unavailable'));await assert.rejects(h.context.refreshThreads(),/Local backend unavailable/);
  const group=h.groups().find(g=>g.dataset.codexGroup==='connection:local');assert.equal(group.querySelector('[data-codex-thread-key]').threadId,'kept');assert.match(group.textContent,/Could not refresh/);assert.equal(h.run(`codexSidebarUi.cache.get('local').error`),'Local backend unavailable');
});

test('section create, rename, chat placement and collapse survive a complete sidebar reload',async()=>{
  const h=harness();h.data.set('local',[thread('a'),thread('b')]);await h.context.refreshThreads();
  const id=await h.context.codexSidebarCreateSection('  Sprint  ');await h.context.codexSidebarMoveToSection({kind:'thread',key:'["local","a"]'},id);await h.context.codexSidebarRenameSection(id,'Shipping');
  const section=h.groups().find(group=>group.dataset.codexGroup==='custom:'+id);assert.equal(section.querySelector('[data-codex-thread-key]').threadId,'a');assert.match(section.textContent,/Shipping/);
  section.querySelector('button').onclick();await settle();
  const saved=h.writes.at(-1).codexSidebar,restarted=harness({preferences:saved});restarted.data.set('local',[thread('a'),thread('b')]);await restarted.context.refreshThreads();
  const restored=restarted.groups().find(group=>group.dataset.codexGroup==='custom:'+id);assert.match(restored.textContent,/Shipping/);assert.equal(restored.querySelector('button').getAttribute('aria-expanded'),'false');
  assert.equal(restarted.run(`codexSidebarPreferences().threadSections['["local","a"]']`),id);assert.equal(h.calls.length,0,'placement uses local preferences for an unpinned chat');
});

test('removing a section keeps its chats and project and returns them to the normal grouping',async()=>{
  const h=harness({preferences:{grouping:'project'}});h.desktop.state.projects=[{id:'project-one',name:'Project',mode:'codex',hostId:'local',path:'/one'}];h.data.set('local',[thread('a',{projectId:'project-one',cwd:'/one'}),thread('b',{cwd:'/two'})]);await h.context.refreshThreads();
  const id=await h.context.codexSidebarCreateSection('Temporary'),projectKey='["local","project:project-one"]';await h.context.codexSidebarMoveToSection({kind:'project',key:projectKey},id);await h.context.codexSidebarMoveToSection({kind:'thread',key:'["local","b"]'},id);
  const section=h.groups().find(group=>group.dataset.codexGroup==='custom:'+id);assert.deepEqual(section.querySelectorAll('[data-codex-thread-key]').map(row=>row.threadId).sort(),['a','b']);
  const before=plain(h.desktop.threads);await h.context.codexSidebarDeleteSection(id);
  assert.equal(h.groups().some(group=>group.dataset.codexGroup==='custom:'+id),false);assert.deepEqual(plain(h.desktop.threads),before);assert.equal(h.desktop.state.projects.length,1);assert.deepEqual(h.list.querySelectorAll('[data-codex-thread-key]').map(row=>row.threadId).sort(),['a','b']);assert.equal(h.calls.length,0,'no archive/delete or project mutation RPC');
  assert.deepEqual(h.writes.at(-1).codexSidebar.threadSections,{});assert.deepEqual(h.writes.at(-1).codexSidebar.projectSections,{});
});

test('same thread id and same project cwd on two hosts keep separate persisted section keys',async()=>{
  const h=harness();h.connect('a');h.run(`codexSidebarRemember('local',[${JSON.stringify(thread('same'))}]);codexSidebarRemember('a',[${JSON.stringify(thread('same'))}]);renderCodexSidebar();`);
  const local=await h.context.codexSidebarCreateSection('Local work'),remote=await h.context.codexSidebarCreateSection('Remote work');
  await h.context.codexSidebarMoveToSection({kind:'thread',key:'["local","same"]'},local);await h.context.codexSidebarMoveToSection({kind:'thread',key:'["a","same"]'},remote);
  const saved=h.writes.at(-1).codexSidebar;assert.equal(saved.threadSections['["local","same"]'],local);assert.equal(saved.threadSections['["a","same"]'],remote);
  await h.context.codexSidebarMoveToSection({kind:'project',key:'["local","cwd:/same/project"]'},local);await h.context.codexSidebarMoveToSection({kind:'project',key:'["a","cwd:/same/project"]'},remote);
  const placements=h.writes.at(-1).codexSidebar.projectSections;assert.equal(placements['["local","cwd:/same/project"]'],local);assert.equal(placements['["a","cwd:/same/project"]'],remote);
  const sectionRows=id=>h.groups().find(group=>group.dataset.codexGroup==='custom:'+id).querySelectorAll('[data-codex-thread-key]').map(row=>row.dataset.codexThreadKey);assert.deepEqual(sectionRows(local),['["local","same"]']);assert.deepEqual(sectionRows(remote),['["a","same"]']);
});

test('moving a chat out of its assigned project section overrides only that chat',async()=>{
  const h=harness();h.data.set('local',[thread('a'),thread('b')]);await h.context.refreshThreads();const id=await h.context.codexSidebarCreateSection('Project section');
  await h.context.codexSidebarMoveToSection({kind:'project',key:'["local","cwd:/same/project"]'},id);await h.context.codexSidebarMoveToSection({kind:'thread',key:'["local","a"]'},null);
  const section=h.groups().find(group=>group.dataset.codexGroup==='custom:'+id);assert.deepEqual(section.querySelectorAll('[data-codex-thread-key]').map(row=>row.threadId),['b']);
  const local=h.groups().find(group=>group.dataset.codexGroup==='connection:local');assert.deepEqual(local.querySelectorAll('[data-codex-thread-key]').map(row=>row.threadId),['a']);assert.equal(h.writes.at(-1).codexSidebar.threadSections['["local","a"]'],null);
});

test('thread menus preserve the original actions and move the clicked host item',async()=>{
  const h=harness();h.connect('a');h.run(`codexSidebarRemember('a',[${JSON.stringify(thread('remote'))}]);renderCodexSidebar();`);h.remoteUi.threads.set('remote','a');const id=await h.context.codexSidebarCreateSection('Remote section');
  const row=h.list.querySelector('[data-codex-thread-key]'),event={currentTarget:row};assert.equal(h.context.threadMenu(event,thread('remote')),'original menu');assert.deepEqual(h.menuCalls,['remote']);assert.match(h.menu.textContent,/Original chat action/);
  await h.menu.querySelectorAll('button').find(button=>button.textContent==='Move to section').onclick();const destination=h.menu.querySelectorAll('button').find(button=>button.dataset.sectionId===id);await destination.onclick();
  assert.equal(h.writes.at(-1).codexSidebar.threadSections['["a","remote"]'],id);assert.equal(h.writes.at(-1).codexSidebar.threadSections['["local","remote"]'],undefined);assert.equal(h.errors.length,0);
});

test('pinned chat section moves unpin on the captured remote host before saving placement',async()=>{
  const unpin=deferred(),h=harness({read:(_host,method)=>method==='desktop/threadMetadata/set'?unpin.promise:Promise.resolve({data:[]})});h.connect('a');h.run(`codexSidebarRemember('a',[${JSON.stringify(thread('pinned',{pinned:true}))}]);renderCodexSidebar();`);h.remoteUi.threads.set('pinned','a');const id=await h.context.codexSidebarCreateSection('Section');
  const before=h.writes.length,moving=h.context.codexSidebarMoveToSection({kind:'thread',key:'["a","pinned"]'},id);await settle();assert.equal(h.writes.length,before);assert.equal(h.calls.at(-1).host,'a');assert.equal(h.calls.at(-1).method,'desktop/threadMetadata/set');assert.deepEqual(plain(h.calls.at(-1).params),{threadId:'pinned',pinned:false});
  unpin.resolve({metadata:{pinned:false}});await moving;assert.equal(h.writes.at(-1).codexSidebar.threadSections['["a","pinned"]'],id);assert.equal(h.desktop.threads[0].pinned,false);
});

test('disconnected and conflicting host ownership fail without saving placement or running actions',async()=>{
  const h=harness();h.connect('a');h.run(`codexSidebarRemember('a',[${JSON.stringify(thread('remote',{pinned:true}))}]);renderCodexSidebar();`);h.remoteUi.threads.set('remote','a');const id=await h.context.codexSidebarCreateSection('Section'),before=h.writes.length;
  h.remoteUi.connections.get('a').state='disconnected';await assert.rejects(h.context.codexSidebarMoveToSection({kind:'thread',key:'["a","remote"]'},id),/disconnected/);assert.equal(h.writes.length,before);assert.equal(h.calls.length,0);
  const row=h.list.querySelector('[data-codex-thread-key]');h.context.threadMenu({currentTarget:row},thread('remote'));assert.match(h.errors.at(-1),/disconnected/);assert.equal(h.menuCalls.length,0);
  h.remoteUi.connections.get('a').state='connected';h.remoteUi.threads.set('remote',null);await assert.rejects(h.context.codexSidebarMoveToSection({kind:'thread',key:'["a","remote"]'},id),/conflicting host/);assert.equal(h.writes.length,before);assert.equal(h.calls.length,0);
});

test('section forms create and rename through the actual UI submit handlers',async()=>{
  const h=harness();h.data.set('local',[thread('a')]);await h.context.refreshThreads();h.node('codex-sidebar-options').onclick();await h.menu.querySelectorAll('button').find(button=>button.textContent==='New section').onclick();
  h.node('codex-section-name').value='<Sprint>';await h.node('modal-body').querySelector('.codex-sidebar-section-form').onsubmit({preventDefault(){}});assert.equal(h.writes.at(-1).codexSidebar.sections[0].name,'<Sprint>');const id=h.writes.at(-1).codexSidebar.sections[0].id;
  const group=h.groups().find(group=>group.dataset.codexGroup==='custom:'+id);await group.querySelector('.codex-sidebar-group-options').onclick();await h.menu.querySelectorAll('button').find(button=>button.textContent==='Rename section').onclick();h.node('codex-section-name').value='Renamed';await h.node('modal-body').querySelector('.codex-sidebar-section-form').onsubmit({preventDefault(){}});assert.equal(h.writes.at(-1).codexSidebar.sections[0].name,'Renamed');
});

test('bound project buttons open the real project and append section actions to its existing menu',async()=>{
  const h=harness({preferences:{grouping:'project'}});h.desktop.state.projects=[{id:'p',name:'Bound',mode:'codex',hostId:'local',path:'/real'}];h.data.set('local',[thread('a',{projectId:'p',cwd:'/real'})]);await h.context.refreshThreads();
  const group=h.groups().find(group=>group.dataset.codexGroup==='["local","project:p"]');await group.querySelector('.codex-sidebar-open-project').onclick();assert.deepEqual(h.openedProjects,['p']);await group.querySelector('.codex-sidebar-group-options').onclick();assert.match(h.menu.textContent,/Original project action/);assert.match(h.menu.textContent,/Move to section/);
});

test('sidebar renders synchronize the shared unread notification state after aggregation',async()=>{
  const h=harness();let latest;h.context.codexSyncChrome=()=>{latest=h.desktop.threads.some(thread=>thread.unread);};h.data.set('local',[thread('a',{unread:true})]);await h.context.refreshThreads();assert.equal(latest,true);
});

test('remote project section identity survives a later local-only project metadata snapshot',async()=>{
  const h=harness();h.connect('a');h.desktop.state.projects=[{id:'remote-project',name:'Remote repository',mode:'codex',hostId:'a',path:'/same/project'}];h.run(`codexSidebarRemember('a',[${JSON.stringify(thread('remote',{projectId:'remote-project'}))}]);renderCodexSidebar();`);
  const id=await h.context.codexSidebarCreateSection('Remote project section');await h.context.codexSidebarMoveToSection({kind:'project',key:'["a","project:remote-project"]'},id);
  h.desktop.state.projects=[];h.context.renderCodexSidebar();const section=h.groups().find(group=>group.dataset.codexGroup==='custom:'+id);assert.equal(section.querySelector('[data-codex-thread-key]').threadId,'remote');assert.match(section.textContent,/project/);assert.equal(h.list.querySelectorAll('[data-codex-thread-key]').length,1);
});
