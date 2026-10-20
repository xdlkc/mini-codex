#!/usr/bin/env node
// Load the complete shipped HTML script. Only transport and external UI effects are faked.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const html=readFileSync(new URL('../src/gui_app.html',import.meta.url),'utf8');
const markdown=readFileSync(new URL('../src/vendor/markdown.js',import.meta.url),'utf8');
const script=html.match(/<script>([\s\S]*?)<\/script>/)[1].replace('// MARKDOWN_VENDOR',()=>markdown);
const plain=value=>JSON.parse(JSON.stringify(value));
function deferred(){let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};}
async function settle(){for(let i=0;i<6;i++)await Promise.resolve();await new Promise(resolve=>setImmediate(resolve));}
function decode(value){return value.replace(/&(#x[\da-f]+|#\d+|amp|lt|gt|quot|apos);/gi,(_,entity)=>entity[0]==='#'?String.fromCodePoint(entity[1].toLowerCase()==='x'?parseInt(entity.slice(2),16):Number(entity.slice(1))):({amp:'&',lt:'<',gt:'>',quot:'"',apos:"'"}[entity.toLowerCase()]));}

function harness(respond){
  const nodes=new Map(),listeners=[],calls=[],errors=[],cwdChanges=[],newThreads=[],gitRefreshes=[];
  class Element {
    constructor(tag='div'){this.tagName=tag.toUpperCase();this.attributes=new Map();this.children=[];this.style={};this.value='';this.checked=false;this.textContent='';this.dataset={};this.parentElement=null;this.hidden=false;this.disabled=false;this.scrollHeight=24;this.handlers=new Map();const classes=new Set();this.classList={add:name=>classes.add(name),remove:name=>classes.delete(name),contains:name=>classes.has(name),toggle(name,force=!classes.has(name)){force?classes.add(name):classes.delete(name);return force;}};this._html='';}
    set id(value){this.attributes.set('id',value);nodes.set(value,this);}get id(){return this.attributes.get('id')||'';}
    set innerHTML(value){
      for(const child of this.querySelectorAll('[id]'))if(nodes.get(child.id)===child)nodes.delete(child.id);
      this._html=String(value);this.children=[];
      for(const match of this._html.matchAll(/<([a-z][\w-]*)\b([^>]*)>/gi)){
        const attributes=new Map();for(const entry of match[2].matchAll(/([^\s=/>]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?/g))attributes.set(entry[1].toLowerCase(),decode(entry[2]??entry[3]??entry[4]??''));
        const child=new Element(match[1]);child.attributes=attributes;if(attributes.has('id'))nodes.set(child.id,child);child.value=attributes.get('value')||'';child.checked=attributes.has('checked');child.hidden=attributes.has('hidden');this.appendChild(child);
      }
    }
    get innerHTML(){return this._html;}
    appendChild(child){child.parentElement=this;this.children.push(child);return child;}append(...items){for(const item of items)this.appendChild(item);}replaceChildren(...items){this._html='';this.children=[];this.append(...items);}
    setAttribute(name,value){this.attributes.set(name,String(value));}getAttribute(name){return this.attributes.get(name)??null;}hasAttribute(name){return this.attributes.has(name);}
    querySelectorAll(selector){const result=[];function walk(node){for(const child of node.children){if(selector==='[data-ui-action]'&&child.hasAttribute('data-ui-action')||selector==='[id]'&&child.id)result.push(child);walk(child);}}walk(this);return result;}
    querySelector(selector){return this.querySelectorAll(selector)[0]||null;}
    closest(selector){for(let node=this;node;node=node.parentElement)if(selector==='[data-ui-action]'&&node.hasAttribute('data-ui-action'))return node;return null;}
    addEventListener(type,fn){this.handlers.set(type,fn);}focus(){document.activeElement=this;}
    getBoundingClientRect(){return {left:100,top:300,bottom:330,width:220,height:100};}
  }
  const node=id=>{if(!nodes.has(id)){const element=new Element();element.id=id;}return nodes.get(id);};
  const document={compatMode:'CSS1Compat',activeElement:null,body:new Element('body'),documentElement:new Element('html'),getElementById:node,createElement:tag=>new Element(tag),querySelectorAll:()=>[],addEventListener(type,fn,capture){listeners.push({type,fn,capture});}};
  const desktop={mode:'codex',navigation:0};
  const h={host:'A',connections:{A:1,B:1},respond,calls,errors,cwdChanges,newThreads,gitRefreshes,node,desktop};
  const transport=(host,method,params={})=>{calls.push({host,method,params:plain(params)});return Promise.resolve().then(()=>{
    const custom=h.respond?.(host,method,params);if(custom!==undefined)return custom;
    if(method==='settings/read')return {mcpServers:[],rawConfigToml:'model = "fixture"',globalAgentsMd:'Fixture rules',projectAgentsMd:'Project rules'};
    if(method==='fs/readDirectory')return {cwd:'/workspace',files:[]};
    if(method==='fs/readFile')return {content:'Fixture file'};
    if(method==='command/exec')return {exitCode:0,output:'Fixture output'};
    return {};
  });};
  const context=vm.createContext({console,URL,setTimeout,clearTimeout,innerWidth:1200,desktop,document,addEventListener(){},captureRpcContext(){const host=h.host;return {key:host+'@'+h.connections[host],call:(method,params)=>transport(host,method,params)};},showError:error=>errors.push(String(error?.message||error))});context.window=context;
  vm.runInContext(script,context,{filename:'complete production gui_app.html'});
  context.rpcCall=(method,params)=>transport(h.host,method,params);
  context.updateCwdUI=path=>{cwdChanges.push({host:h.host,path});vm.runInContext('currentCwd='+JSON.stringify(path),context);};
  context.newThread=async()=>newThreads.push({host:h.host,cwd:vm.runInContext('currentCwd',context)});
  context.refreshGitStatus=async()=>gitRefreshes.push(h.host);
  h.context=context;h.read=code=>vm.runInContext(code,context);h.set=(name,value)=>vm.runInContext(name+'='+JSON.stringify(value),context);
  h.switch=(host,{cwd='/workspace',threadId=null,navigate=true,close=true}={})=>{h.host=host;if(navigate)desktop.navigation++;h.set('currentCwd',cwd);h.set('currentThread',threadId);if(close)context.closeModal();};
  h.actions=root=>node(root).querySelectorAll('[data-ui-action]');
  const click=listeners.find(item=>item.type==='click'&&item.capture===true)?.fn;assert.equal(typeof click,'function');
  h.click=element=>{let prevented=false,stopped=false;click({target:{closest:()=>element},preventDefault(){prevented=true;},stopPropagation(){stopped=true;}});assert.ok(prevented&&stopped);};
  h.match=method=>calls.filter(call=>call.method===method);
  h.set('currentCwd','/workspace');h.set('currentThread',null);return h;
}

test('a late file preview cannot reopen host A content or add an A mention after switching to B',async()=>{
  const reply=deferred(),h=harness((host,method)=>host==='A'&&method==='fs/readFile'?reply.promise:undefined);
  const preview=h.context.previewWorkspaceFile('notes.txt');await settle();h.switch('B',{cwd:'/host-B-repo'});h.node('modal-body').innerHTML='Host B modal state';reply.resolve({content:'Private content from A'});await preview;
  assert.equal(h.node('modal-body').innerHTML,'Host B modal state');assert.equal(h.node('modal-backdrop').classList.contains('open'),false);assert.deepEqual(plain(h.read('mentionedFiles')),[]);assert.deepEqual(h.match('fs/readFile'),[{host:'A',method:'fs/readFile',params:{cwd:'/workspace',path:'notes.txt'}}]);
});

test('an already rendered preview mention remains bound to its original host',async()=>{
  const h=harness();await h.context.previewWorkspaceFile('notes.txt');const mention=h.actions('modal-body').find(element=>element.getAttribute('data-ui-action')==='mention-file');assert.ok(mention?._mcodexExecution);h.switch('B');h.click(mention);await settle();assert.deepEqual(plain(h.read('mentionedFiles')),[]);assert.match(h.errors[0],/execution environment changed/i);assert.equal(h.match('fs/readFile').length,1);
});

test('a same-host preview still adds the selected file through the production delegated action',async()=>{
  const h=harness();await h.context.previewWorkspaceFile('notes.txt');h.click(h.actions('modal-body').find(element=>element.getAttribute('data-ui-action')==='mention-file'));await settle();assert.deepEqual(plain(h.read('mentionedFiles')),['notes.txt']);assert.equal(h.errors.length,0);assert.equal(h.node('modal-backdrop').classList.contains('open'),false);
});

test('late select/add/create project results do not replace the new host workspace or open a new chat',async()=>{
  for(const action of ['selectProject','submitAddExistingProject','submitCreateNewProject']){
    const reply=deferred(),h=harness((host,method)=>host==='A'&&method==='projects/create'?reply.promise:undefined);
    h.node('proj-existing-path').value='/host-A-repo';h.node('proj-existing-label').value='A';h.node('proj-new-name').value='feature';h.node('proj-new-parent').value='/host-A-root';h.node('proj-new-git').checked=true;
    const pending=action==='selectProject'?h.context[action]('/host-A-repo'):h.context[action]();await settle();assert.equal(h.match('projects/create')[0].host,'A');h.switch('B',{cwd:'/host-B-repo'});reply.resolve({ok:true});await pending;assert.equal(h.read('currentCwd'),'/host-B-repo',action);assert.deepEqual(h.cwdChanges,[],action);assert.deepEqual(h.newThreads,[],action);assert.equal(h.match('projects/create').length,1);
  }
});

test('a current project creation still updates only its own host and starts its chat',async()=>{
  const h=harness();h.node('proj-new-name').value='feature';h.node('proj-new-parent').value='/host-A-root';h.node('proj-new-git').checked=true;await h.context.submitCreateNewProject();assert.deepEqual(h.match('projects/create'),[{host:'A',method:'projects/create',params:{path:'/host-A-root/feature',label:'feature',createDir:true,initGit:true}}]);assert.deepEqual(h.cwdChanges,[{host:'A',path:'/host-A-root/feature'}]);assert.deepEqual(h.newThreads,[{host:'A',cwd:'/host-A-root/feature'}]);
});

test('late directory and diff results cannot replace a newer host drawer',async()=>{
  for(const tab of ['files','diff']){
    const reply=deferred(),method=tab==='files'?'fs/readDirectory':'workspace/gitStatus',h=harness((host,name)=>host==='A'&&name===method?reply.promise:undefined);h.set('currentDrawerTab',tab);h.set('latestGitStatus',null);const loading=h.context.renderDrawerContent();await settle();h.switch('B');h.node('drawer-content').innerHTML='Current host B drawer';reply.resolve(tab==='files'?{cwd:'/workspace',files:[{path:'from-A.rs'}]}:{changedFiles:[],diff:'A-only diff'});await loading;assert.equal(h.node('drawer-content').innerHTML,'Current host B drawer');assert.equal(h.match(method)[0].host,'A');
  }
});

test('a rendered directory action cannot preview or mention a file in a different host',async()=>{
  const h=harness((_host,method)=>method==='fs/readDirectory'?{cwd:'/workspace',files:[{path:'from-A.rs'}]}:undefined);h.set('currentDrawerTab','files');await h.context.renderDrawerContent();const actions=h.actions('drawer-content');assert.equal(actions.length,2);h.switch('B');for(const action of actions)h.click(action);await settle();assert.deepEqual(plain(h.read('mentionedFiles')),[]);assert.equal(h.match('fs/readFile').length,0);assert.equal(h.errors.length,2);
});

test('a late @file search cannot reopen A suggestions after a host switch or a changed draft',async()=>{
  for(const change of ['host','draft']){
    const reply=deferred(),h=harness((_host,method)=>method==='fs/readDirectory'?reply.promise:undefined);h.node('composer-input').value='@note';const searching=h.context.handleComposerInput();await settle();if(change==='host')h.switch('B');h.node('composer-input').value='B draft remains';h.node('autocomplete-popover').innerHTML='Newer suggestions';h.node('autocomplete-popover').classList.remove('open');reply.resolve({files:[{path:'A-notes.txt'}]});await searching;assert.equal(h.node('autocomplete-popover').innerHTML,'Newer suggestions');assert.equal(h.node('autocomplete-popover').classList.contains('open'),false);assert.equal(h.node('composer-input').value,'B draft remains');
  }
});

test('an already rendered @file suggestion cannot alter the new host draft',async()=>{
  const h=harness((_host,method)=>method==='fs/readDirectory'?{files:[{path:'A-notes.txt'}]}:undefined);h.node('composer-input').value='@note';await h.context.handleComposerInput();const action=h.actions('autocomplete-popover')[0];assert.ok(action?._mcodexExecution);h.switch('B');h.node('composer-input').value='Keep B draft';h.click(action);await settle();assert.equal(h.node('composer-input').value,'Keep B draft');assert.deepEqual(plain(h.read('mentionedFiles')),[]);assert.match(h.errors[0],/execution environment changed/i);
});

test('an old terminal cannot execute after switching host or replacing the connection',async()=>{
  for(const change of ['host','connection']){
    const h=harness();h.set('currentDrawerTab','terminal');await h.context.renderDrawerContent();h.node('quick-cmd-input').value='pwd';if(change==='host')h.switch('B');else h.connections.A++;
    await assert.rejects(h.context.execQuickCmd(),/execution environment changed/i);assert.equal(h.match('command/exec').length,0);assert.equal(h.gitRefreshes.length,0);
  }
});

test('late terminal output cannot overwrite the replacement terminal or refresh the wrong host',async()=>{
  const reply=deferred(),h=harness((host,method)=>host==='A'&&method==='command/exec'?reply.promise:undefined);h.set('currentDrawerTab','terminal');await h.context.renderDrawerContent();const old=h.node('quick-cmd-out');h.node('quick-cmd-input').value='pwd';const executing=h.context.execQuickCmd();await settle();h.switch('B');await h.context.renderDrawerContent();const replacement=h.node('quick-cmd-out');assert.notEqual(replacement,old);const before=replacement.innerHTML;reply.resolve({exitCode:0,output:'Output belonging to A'});await executing;assert.equal(replacement.innerHTML,before);assert.ok(!replacement.textContent.includes('Output belonging to A'));assert.equal(h.match('command/exec')[0].host,'A');assert.deepEqual(h.gitRefreshes,[]);
});

test('a current terminal executes with its captured host and shows the real result',async()=>{
  const h=harness();h.set('currentDrawerTab','terminal');await h.context.renderDrawerContent();h.node('quick-cmd-input').value='pwd';await h.context.execQuickCmd();assert.deepEqual(h.match('command/exec'),[{host:'A',method:'command/exec',params:{cwd:'/workspace',command:'pwd',sandbox:'workspace-write'}}]);assert.match(h.node('quick-cmd-out').textContent,/Fixture output/);assert.deepEqual(h.gitRefreshes,['A']);
});

test('MCP and configuration writes reject a stale host settings view before any mutation RPC',async()=>{
  for(const [action,args] of [['addMcpServerFromSettings',[]],['removeMcpServerFromSettings',['fixture']],['saveRawConfigToml',[]],['saveAgentsMd',[]]]){
    const h=harness();await h.context.renderSettingsSection('mcp');h.node('mcp-new-name').value='fixture';h.node('mcp-new-cmd').value='fixture-command --flag';h.node('raw-config-toml').value='model = "A"';h.node('global-agents-md').value='A global';h.node('project-agents-md').value='A project';h.switch('B');await assert.rejects(h.context[action](...args),/execution environment changed/i);assert.equal(h.calls.length,1,action);assert.equal(h.calls[0].method,'settings/read');
  }
});

test('current MCP writes stay on their captured host and late settings reads do not replace newer settings',async()=>{
  const h=harness();await h.context.renderSettingsSection('mcp');h.node('mcp-new-name').value='fixture';h.node('mcp-new-cmd').value='fixture-command --flag';await h.context.addMcpServerFromSettings();assert.deepEqual(h.match('mcp/addServer'),[{host:'A',method:'mcp/addServer',params:{name:'fixture',command:'fixture-command',args:['--flag']}}]);
  const reply=deferred(),other=harness((host,method)=>method==='settings/read'?(host==='A'?reply.promise:{mcpServers:[{name:'B server'}]}):undefined);const loading=other.context.renderSettingsSection('mcp');await settle();other.switch('B');await other.context.renderSettingsSection('mcp');const body=other.node('settings-body').innerHTML;reply.resolve({mcpServers:[{name:'Old A server'}]});await loading;assert.equal(other.node('settings-body').innerHTML,body);assert.match(body,/B server/);assert.ok(!body.includes('Old A server'));assert.equal(other.node('settings-body')._mcodexExecution.key,'B@1');
});
