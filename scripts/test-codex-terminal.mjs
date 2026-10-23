import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import path from 'node:path';
import test from 'node:test';
const require=createRequire(import.meta.url);
const {JSDOM}=process.env.EDITOR_TEST_NODE_MODULES?require(path.join(process.env.EDITOR_TEST_NODE_MODULES,'jsdom')):createRequire(new URL('../src/vendor/editor/package.json',import.meta.url))('jsdom');
const source=readFileSync(new URL('../src/gui_terminal.js',import.meta.url),'utf8');
const plain=value=>JSON.parse(JSON.stringify(value));
async function tick(){for(let i=0;i<30;i++)await Promise.resolve();}
function harness(t){
  const dom=new JSDOM('<!doctype html><main id="main-col"><textarea id="composer-input"></textarea><div id="drawer-content"></div></main>',{runScripts:'outside-only',pretendToBeVisual:true,url:'http://127.0.0.1/'}),w=dom.window;t.after(()=>dom.window.close());
  const h={w,calls:[],errors:[],records:new Map(),handler:null,number:0};
  class Terminal {constructor(){this.cols=80;this.rows=24;this.options={};this.buffer='';}loadAddon(addon){this.addon=addon;}open(node){node.getClientRects=()=>[{}];}write(text,done){this.buffer+=text;done?.();}reset(){this.buffer='';}clear(){this.buffer='';}hasSelection(){return Boolean(this.selection);}getSelection(){return this.selection||'';}paste(text){this.input(text);}onData(fn){this.input=fn;}onKey(fn){this.keyInput=fn;}onTitleChange(fn){this.changeTitle=fn;}attachCustomKeyEventHandler(fn){this.key=fn;}focus(){}dispose(){this.disposed=true;}}
  Object.assign(w,{$:id=>w.document.getElementById(id),Terminal,FitAddon:{FitAddon:class {fit(){}}},ResizeObserver:class {observe(){}disconnect(){}},
    showLibrary:async()=>{if(h.libraryHandler)return h.libraryHandler();},newThread:async()=>{w.desktop.library=false;w.desktop.navigation++;},
    desktop:{mode:'codex',navigation:1},host:'alpha',currentCwd:'/repo',currentThread:'chat-a',drawerOpen:true,currentDrawerTab:'terminal',
    codexText:en=>en,codexEnvPath:value=>String(value).replace(/\/+$/,'')||'/',remoteContext:()=>w.host,remoteUi:{byConnection:new Map([['a',{profileId:'alpha'}],['b',{profileId:'beta'}]])},
    workspaceCapture:()=>{const host=w.host,navigation=w.desktop.navigation;return {key:host,navigation,call:async(method,params={})=>{
      h.calls.push({host,method,params:plain(params)});const result=h.handler?.(method,params,host);if(result!==undefined)return result;
      if(method==='terminal/list')return {data:[...h.records.values()].filter(s=>s.host===host&&(!('threadId'in params)||s.threadId===params.threadId)&&(!params.cwd||s.cwd===params.cwd)).map(plain)};
      if(method==='terminal/create'){const state={id:'pty_'+(++h.number),host,cwd:params.cwd,threadId:params.threadId,title:params.title||'Terminal',fixedTitle:params.title||null,status:'running',cols:80,rows:24,sequence:0,output:''};h.records.set(state.id,state);return {terminal:plain(state)};}
      if(method==='terminal/status')return {terminal:plain(h.records.get(params.id))};
      if(method==='terminal/close'){const state=h.records.get(params.id);h.records.delete(params.id);return {terminal:{...state,status:'closed'}};}
      if(['terminal/write','terminal/resize','terminal/ack'].includes(method))return {};
      if(method==='workspace/environment/read')return {environment:{type:'success',environment:{actions:[{name:'Build <fixture>',command:'printf "<build>"'}]}}};
      throw new Error('Unexpected request '+method);
    }};},workspaceCurrent:context=>context.key===w.host&&context.navigation===w.desktop.navigation,workspaceRequire:context=>{if(!w.workspaceCurrent(context))throw new Error('Context changed');},
    showError:error=>h.errors.push(error.message),copyText:()=>{},codexEnvironmentContext:()=>w.workspaceCapture(),codexEnvironmentStart:async()=>({legacy:true}),renderDrawerContent:async()=>{},updateCwdUI:cwd=>{w.currentCwd=cwd;},handleServerNotification:()=>{},toggleDrawer:()=>{w.drawerOpen=true;w.currentDrawerTab='terminal';return w.codexTerminalRender();},
  });w.eval(source+'\nwindow.__terminalUi=codexTerminalUi;');h.sessions=()=>w.__terminalUi.sessions;h.session=()=>[...h.sessions().values()].at(-1);return h;
}
test('terminal input and resize stay on their captured host, and commands use text labels',async t=>{
  const h=harness(t),w=h.w;await w.codexTerminalRender();await tick();const session=h.session();session.term.input('中文\u0003');await tick();const sent=h.calls.find(c=>c.method==='terminal/write');assert.equal(sent.host,'alpha');assert.equal(sent.params.data,'中文\u0003');
  session.term.cols=120;session.term.rows=30;w.codexTerminalFit(session);await tick();assert.equal(h.calls.filter(c=>c.method==='terminal/resize').at(-1).host,'alpha');
  w.host='beta';w.desktop.navigation++;session.term.input('must not send');await tick();assert.equal(h.calls.filter(c=>c.method==='terminal/write').length,1);
});
test('environment actions open a PTY with their saved command and cannot execute after navigation changes',async t=>{
  const h=harness(t),w=h.w;await w.codexEnvironmentStart({cwd:'/repo',scriptType:'action',actionIndex:0,configPath:'/repo/environment.toml',threadId:'chat-a'});await tick();const request=h.calls.find(c=>c.method==='terminal/create');assert.equal(request.params.command,'printf "<build>"');assert.equal(request.params.title,'Build <fixture>');assert.equal(w.document.querySelector('fixture'),null);
  let resolve;h.handler=method=>method==='workspace/environment/read'?new Promise(done=>resolve=done):undefined;const start=w.codexEnvironmentStart({cwd:'/repo',scriptType:'action',actionIndex:0,configPath:'/repo/environment.toml',threadId:'chat-a'});await tick();w.currentThread='chat-b';w.desktop.navigation++;resolve({environment:{type:'success',environment:{actions:[{name:'Wrong',command:'bad'}]}}});await start;assert.equal(h.calls.filter(c=>c.method==='terminal/create').length,1);
});
test('PTY notifications do not cross hosts, and missing output packets resync from the server',async t=>{
  const h=harness(t),w=h.w;await w.codexTerminalRender();await tick();const session=h.session(),id=session.state.id;
  w.handleServerNotification('terminal/output',{connectionId:'b',id,sequence:1,delta:'wrong host'});assert.equal(session.term.buffer,'');
  w.handleServerNotification('terminal/output',{connectionId:'a',id,sequence:1,delta:'中文\u001b[31m'});assert.equal(session.term.buffer,'中文\u001b[31m');
  h.records.get(id).output='中文\u001b[31mrecovered';h.records.get(id).sequence=3;w.handleServerNotification('terminal/output',{connectionId:'a',id,sequence:3,delta:'recovered'});await tick();assert.equal(session.term.buffer,'中文\u001b[31mrecovered');
  w.handleServerNotification('terminal/updated',{connectionId:'a',terminal:{...plain(session.state),status:'exited',exitCode:0}});w.handleServerNotification('terminal/updated',{connectionId:'a',terminal:{...plain(session.state),status:'running'}});assert.equal(session.state.status,'exited');
});
test('closing one terminal preserves the other and a failed close remains retryable',async t=>{
  const h=harness(t),w=h.w;await w.codexTerminalRender();await w.codexTerminalCreate();await tick();assert.equal(w.document.querySelectorAll('[role=tab]').length,2);const session=h.session();let fail=true;h.handler=method=>method==='terminal/close'&&fail?Promise.reject(new Error('offline')):undefined;
  const close=[...w.document.querySelectorAll('button')].find(button=>button.getAttribute('aria-label')==='Close terminal Terminal');await close.onclick();assert.equal(close.disabled,false);assert.equal(h.sessions().size,2);fail=false;await close.onclick();assert.equal(h.sessions().size,1);assert.equal(w.document.querySelectorAll('[role=tab]').length,1);assert.equal(session.term?.disposed,undefined);assert.deepEqual(h.errors,['offline']);
});
test('returning to the same chat rebinds terminal input to the new navigation context',async t=>{
  const h=harness(t),w=h.w;await w.codexTerminalRender();await tick();const first=h.session(),term=first.term;
  w.currentThread='chat-b';w.desktop.navigation++;await w.codexTerminalRender();w.currentThread='chat-a';w.desktop.navigation++;await w.codexTerminalRender();await tick();
  assert.equal(first.term,term);first.term.input('after returning');await tick();assert.equal(h.calls.filter(c=>c.method==='terminal/write').at(-1).params.id,first.state.id);
  w.handleServerNotification('terminal/updated',{connectionId:'a',terminal:{...plain(first.state),status:'closed'}});assert.equal(term.disposed,true);assert.ok(!h.sessions().has(first.key));
});
test('rapid keyboard events wait for the previous write instead of racing HTTP requests',async t=>{
  const h=harness(t),w=h.w;await w.codexTerminalRender();await tick();const session=h.session();let release;h.handler=(method,params)=>method==='terminal/write'&&params.data==='first'?new Promise(done=>release=done):undefined;
  session.term.input('first');session.term.input('second');await tick();assert.equal(h.calls.filter(c=>c.method==='terminal/write').length,1);release({ok:true});await tick();assert.deepEqual(h.calls.filter(c=>c.method==='terminal/write').map(c=>c.params.data),['first','second']);
});
test('snapshot replay does not send terminal-generated device replies back to the shell',async t=>{
  const h=harness(t),w=h.w;await w.codexTerminalRender();await tick();const session=h.session();
  session.term.write=(text,done)=>{session.term.buffer+=text;session.term.input('\u001b[1;1R');done?.();};
  w.codexTerminalRemember({...plain(session.state),sequence:1,output:'\u001b[6n'},session.context);await tick();assert.equal(h.calls.filter(c=>c.method==='terminal/write').length,0);
  session.replaying=true;session.term.keyInput();session.term.input('typed');session.replaying=false;await tick();assert.equal(h.calls.filter(c=>c.method==='terminal/write').at(-1).params.data,'typed');
});
test('a page reload resumes background output before its terminal panel is opened',async t=>{
  const h=harness(t),w=h.w;h.records.set('pty_existing',{id:'pty_existing',host:'alpha',cwd:'/other',threadId:'other-chat',title:'Build',status:'running',cols:80,rows:24,sequence:8,output:'existing output'});
  await w.codexTerminalRestoreHost();await new Promise(resolve=>setTimeout(resolve,30));assert.equal(h.sessions().size,1);assert.equal(w.document.querySelector('#codex-terminal-panel'),null);
  const ack=h.calls.find(c=>c.method==='terminal/ack');assert.equal(ack.host,'alpha');assert.equal(ack.params.sequence,8);
  const session=h.session();w.handleServerNotification('terminal/output',{connectionId:'a',id:'pty_existing',sequence:9,delta:' continued'});assert.equal(session.state.output,'existing output continued');
});
test('a new background terminal announced by another window receives acknowledgements',async t=>{
  const h=harness(t),w=h.w;const state={id:'pty_background',cwd:'/other',threadId:'other-chat',title:'Build',status:'running',cols:80,rows:24,sequence:0,output:''};
  w.handleServerNotification('terminal/updated',{connectionId:'a',terminal:state});w.handleServerNotification('terminal/output',{connectionId:'a',id:state.id,sequence:1,delta:'building'});await new Promise(resolve=>setTimeout(resolve,30));
  assert.equal(h.session().state.output,'building');assert.equal(w.document.querySelector('#codex-terminal-panel'),null);assert.equal(h.calls.filter(c=>c.method==='terminal/ack').at(-1).params.sequence,1);
});
test('source Mac key mappings send line-editing controls and Command-T opens another terminal',async t=>{
  const h=harness(t),w=h.w;await w.codexTerminalRender();await tick();const session=h.session();let prevented=0;const event=key=>({type:'keydown',key,metaKey:true,ctrlKey:false,altKey:false,shiftKey:false,preventDefault(){prevented++;},stopPropagation(){}});
  for(const key of ['ArrowLeft','ArrowUp','ArrowRight','ArrowDown','Backspace','Delete'])assert.equal(session.term.key(event(key)),false);await tick();assert.deepEqual(h.calls.filter(c=>c.method==='terminal/write').map(c=>c.params.data),['\u0001','\u0001','\u0005','\u0005','\u0015','\u000b']);
  assert.equal(session.term.key({...event('ArrowLeft'),shiftKey:true}),true);assert.equal(session.term.key(event('t')),false);await tick();assert.equal(w.document.querySelectorAll('[role=tab]').length,2);assert.equal(prevented,7);
});
test('terminal clipboard shortcuts use the client platform and copy without a missing global helper',async t=>{
  const h=harness(t),w=h.w;await w.codexTerminalRender();await tick();const session=h.session(),copied=[];let reads=0;Object.defineProperty(w.navigator,'platform',{value:'MacIntel',configurable:true});Object.defineProperty(w.navigator,'clipboard',{value:{writeText:async text=>{copied.push(text);},readText:async()=>{reads++;return 'paste fixture';}},configurable:true});session.term.selection='selected fixture';
  const event={type:'keydown',key:'c',metaKey:true,ctrlKey:false,shiftKey:false,altKey:false,preventDefault(){},stopPropagation(){}};assert.equal(session.term.key(event),false);await tick();assert.deepEqual(copied,['selected fixture']);assert.equal(session.term.key({...event,key:'v'}),true);assert.equal(reads,0);
  Object.defineProperty(w.navigator,'platform',{value:'Linux x86_64',configurable:true});assert.equal(session.term.key({...event,key:'Insert',metaKey:false,shiftKey:true}),false);await tick();assert.equal(reads,1);assert.equal(h.calls.filter(c=>c.method==='terminal/write').at(-1).params.data,'paste fixture');assert.deepEqual(h.errors,[]);
});
test('actual app shortcut listeners leave terminal control keys for the shell',async t=>{
  const h=harness(t),w=h.w;await w.codexTerminalRender();await tick();let searches=0,sidebars=0;Object.assign(w,{openSearch:async()=>{searches++;},toggleSidebar:()=>{sidebars++;},codexNavigateHistory:async()=>{},closeModal(){},closeFloatingMenu(){},closeAutocomplete(){},switchMode:async()=>{}});
  for(const [file,start,end] of [['gui_desktop.js',"document.addEventListener('keydown',e=>{",'},true);'],['gui_codex.js',"  document.addEventListener('keydown',event=>{",'  },true);']]){const text=readFileSync(new URL('../src/'+file,import.meta.url),'utf8'),a=text.indexOf(start),b=text.indexOf(end,a);assert.ok(a>=0&&b>a);w.eval(text.slice(a,b+end.length));}
  const input=w.document.createElement('textarea');h.session().container.append(input);
  for(const key of ['b','s','[',']','k','1']){const event=new w.KeyboardEvent('keydown',{key,ctrlKey:true,bubbles:true,cancelable:true});input.dispatchEvent(event);assert.equal(event.defaultPrevented,false,key);}
  const clear=new w.KeyboardEvent('keydown',{key:'k',metaKey:true,bubbles:true,cancelable:true});input.dispatchEvent(clear);assert.equal(clear.defaultPrevented,false);assert.equal(searches,0);
  input.dispatchEvent(new w.KeyboardEvent('keydown',{key:'b',metaKey:true,bubbles:true,cancelable:true}));assert.equal(sidebars,1);
});

test('shell OSC titles follow the reference cwd-prefix rules without changing fixed action titles',async t=>{
  const h=harness(t),w=h.w;await w.codexTerminalRender();await tick();const session=h.session();
  for(const [raw,expected] of [['/repo: build <fixture>','build <fixture>'],['repo — test','test'],['repo - task','task'],['/repo | task','task'],['\u0007 repo: running \u007f','running'],['/repo','Terminal'],['repo','Terminal'],['','Terminal']]){session.term.changeTitle(raw);assert.equal(w.document.querySelector('[role=tab]').textContent,expected);}
  assert.equal(w.document.querySelector('fixture'),null);session.term.changeTitle('build');w.codexTerminalRemember({...plain(session.state),sequence:1},session.context);assert.equal(w.document.querySelector('[role=tab]').textContent,'build');
  await w.codexTerminalCreate(w.codexTerminalCapture(),{name:'Saved action',command:'build'});await tick();h.session().term.changeTitle('/repo: shell replacement');assert.equal(w.document.querySelector('[role=tab][aria-selected=true]').textContent,'Saved action');
});

test('closing Terminal before its session list arrives does not create a PTY or reopen the inspector',async t=>{
  const h=harness(t),w=h.w;let resolve;h.handler=method=>method==='terminal/list'?new Promise(done=>resolve=done):undefined;
  const opening=w.codexTerminalRender();await tick();w.codexTerminalClose();resolve({data:[]});await opening;
  assert.equal(w.__terminalUi.open,false);assert.equal(w.document.querySelector('#codex-terminal-dock').hidden,true);assert.equal(h.calls.filter(c=>c.method==='terminal/create').length,0);
});
test('a pending new terminal preserves a subsequent Files selection and remains available on return',async t=>{
  const h=harness(t),w=h.w;await w.codexTerminalRender();await tick();let resolve;const state={id:'delayed-pty',host:'alpha',cwd:'/repo',threadId:'chat-a',title:'Delayed terminal',status:'running',sequence:0,output:''};
  h.handler=method=>method==='terminal/create'?new Promise(done=>resolve=done):undefined;
  const opening=w.document.querySelector('[aria-label="New terminal"]').onclick();await tick();w.currentDrawerTab='files';w.document.querySelector('#drawer-content').textContent='Files view';h.records.set(state.id,state);resolve({terminal:state});await opening;
  assert.equal(w.currentDrawerTab,'files');assert.equal(w.document.querySelector('#drawer-content').textContent,'Files view');assert.equal(h.sessions().size,2);
  h.handler=null;w.currentDrawerTab='terminal';await w.codexTerminalRender();await tick();assert.equal(w.document.querySelector('[role=tab][aria-selected=true]').textContent,'Delayed terminal');
});

test('Terminal opens below the conversation without replacing the file inspector, and closing its panel keeps its PTY',async t=>{
  const h=harness(t),w=h.w;w.currentDrawerTab='files';w.drawerOpen=true;w.document.querySelector('#drawer-content').textContent='Files stay open';
  await w.toggleDrawer('terminal');await tick();const session=h.session(),term=session.term;
  assert.equal(w.document.querySelector('#codex-terminal-dock').parentElement.id,'main-col');assert.equal(w.document.querySelector('#drawer-content').textContent,'Files stay open');assert.equal(w.currentDrawerTab,'files');assert.equal(w.drawerOpen,true);
  await w.toggleDrawer('terminal');assert.equal(w.__terminalUi.open,false);assert.equal(h.sessions().size,1);assert.equal(h.records.size,1);
  await w.toggleDrawer('terminal');await tick();assert.equal(h.session().term,term);assert.equal(h.calls.filter(c=>c.method==='terminal/create').length,1);
});
test('the bottom terminal hides in other views and resumes the same session when returning',async t=>{
  const h=harness(t),w=h.w;await w.codexTerminalRender();await tick();const term=h.session().term;
  w.document.body.classList.add('in-settings');w.codexTerminalSyncDock();assert.equal(w.document.querySelector('#codex-terminal-dock').hidden,true);
  w.document.body.classList.remove('in-settings');w.desktop.navigation++;w.codexTerminalSyncDock();await tick();assert.equal(w.document.querySelector('#codex-terminal-dock').hidden,false);assert.equal(h.session().term,term);assert.equal(h.calls.filter(c=>c.method==='terminal/create').length,1);
});

test('an auxiliary view hides the terminal while loading and a later chat navigation can restore it',async t=>{
 const h=harness(t),w=h.w;await w.codexTerminalRender();await tick();let resolve;h.libraryHandler=()=>new Promise(done=>resolve=done);const library=w.showLibrary();await tick();assert.equal(w.document.querySelector('#codex-terminal-dock').hidden,true);await w.newThread();await tick();assert.equal(w.document.querySelector('#codex-terminal-dock').hidden,false);resolve();await library;assert.equal(w.document.querySelector('#codex-terminal-dock').hidden,false);
});
