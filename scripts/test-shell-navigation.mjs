import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import path from 'node:path';
const require=createRequire(import.meta.url);
const {JSDOM}=process.env.EDITOR_TEST_NODE_MODULES?require(path.join(process.env.EDITOR_TEST_NODE_MODULES,'jsdom')):createRequire(new URL('../src/vendor/editor/package.json',import.meta.url))('jsdom');
const desktopSource=readFileSync(new URL('../src/gui_desktop.js',import.meta.url),'utf8');
const slice=(from,to)=>desktopSource.slice(desktopSource.indexOf(from),desktopSource.indexOf(to,desktopSource.indexOf(from)));
const defer=()=>{let resolve;const promise=new Promise(done=>resolve=done);return {promise,resolve};};
const settle=async()=>new Promise(resolve=>setImmediate(resolve));
function harness(t){
  const dom=new JSDOM('<!doctype html><body><div id="app"><div id="sidebar"><div class="mode-switcher"></div></div><div id="topbar"></div><div id="topbar-title"></div><div id="topbar-sidebar-btn"></div><div id="inspector-drawer" class="closed"></div><div id="home-hero"></div><div id="conversation-wrap"></div><div id="composer-host"><textarea id="composer-input"></textarea></div><div id="nav-home"></div><div id="nav-library"></div><div id="work-library" class="hidden"></div><div id="settings-body"></div><div id="settings-view"></div><div id="account-avatar">M</div></div></body>',{runScripts:'outside-only',pretendToBeVisual:true});t.after(()=>dom.window.close());
  const w=dom.window,h={w,calls:[],errors:[],handler:null};
  Object.assign(w,{$:id=>w.document.getElementById(id),desktop:{navigation:0,documentNavigation:0,library:false,mode:'codex',state:{settings:{preferences:{}}}},currentSettingsSection:'general',settingsRenderRequest:0,currentCwd:'/fixture',openInApps:[],
    titleCase:text=>text,updateModeUI(){},maybeDragWindow(){},saveDraft(){},flushDocument:async()=>{},closeFloatingMenu(){},closeSettingsView(){w.document.body.classList.remove('in-settings');},hideLibrary(){w.desktop.library=false;w.$('work-library').classList.add('hidden');},baseCloseDrawer(){},
    escapeHtml:text=>String(text),uiIcon:()=>'',showError:error=>h.errors.push(error.message),createDocument(){},openDocument(){},newThread:async()=>{w.desktop.navigation++;w.closeSettingsView();w.hideLibrary();},switchMode:async mode=>{w.desktop.mode=mode;},openThread:async()=>{},openScheduledView:async()=>{},openSettingsView:async section=>w.renderSettingsSection(section),renderSettingsSection:async section=>{w.currentSettingsSection=section;w.$('settings-body').textContent=section;},
    captureExecutionUi:()=>({navigation:w.desktop.navigation}),executionUiCurrent:context=>context.navigation===w.desktop.navigation,
    rpcCall:async(method,params={})=>{h.calls.push({method,params});return h.handler?.(method,params)??{targets:[],items:[]};},
  });w.rpcLocalCall=w.rpcCall;
  w.eval(slice('async function showLibrary(){','async function createDocument('));
  w.eval(slice('const baseOpenSettings=openSettingsView;','const baseCloseSettings=closeSettingsView;'));
  w.eval(slice('const baseRenderSettings=renderSettingsSection;','const baseToggleSidebar=toggleSidebar;'));
  return h;
}
test('the Library rail button preserves Codex mode, and Account selects its own rail entry',async t=>{
  const h=harness(t),w=h.w;Object.assign(w,{desktopReady:Promise.resolve(),scheduledUiReady:Promise.resolve(),activeFloatingMenuKey:null,openFloatingMenuAt(){},menuButton(){return w.document.createElement('button');},showLibrary:async()=>{w.desktop.library=true;},openSettingsView:async section=>{w.currentSettingsSection=section;w.document.body.classList.add('in-settings');}});
  w.eval(readFileSync(new URL('../src/gui_shell.js',import.meta.url),'utf8'));await settle();
  w.document.querySelector('[data-rail-view="library"]').click();await settle();assert.equal(w.desktop.mode,'codex');assert.equal(w.document.querySelector('[data-rail-view="library"]').getAttribute('aria-current'),'page');
  w.document.querySelector('.rail-account').click();await settle();assert.equal(w.document.querySelector('.rail-account').getAttribute('aria-current'),'page');assert.equal(w.document.querySelector('[data-rail-view="more"]').hasAttribute('aria-current'),false);
});
test('a slow General response cannot replace Skills selected afterwards',async t=>{
  const h=harness(t),w=h.w,pending=defer();h.handler=method=>method==='openIn/list'?pending.promise:undefined;
  const first=w.renderSettingsSection('general');await settle();await w.renderSettingsSection('skills');pending.resolve({targets:[{id:'finder',label:'Finder'}]});await first;assert.equal(w.$('settings-body').textContent,'skills');
});
test('a settings navigation waiting for document save cannot reopen after Home',async t=>{
  const h=harness(t),w=h.w,pending=defer();w.flushDocument=()=>pending.promise;
  const first=w.openSettingsView('general');await settle();await w.newThread();pending.resolve();await first;assert.equal(w.document.body.classList.contains('in-settings'),false);
});
test('an older Library response cannot overwrite a new Library request',async t=>{
  const h=harness(t),w=h.w,first=defer(),second=defer();let requests=0;h.handler=()=>++requests===1?first.promise:second.promise;
  const a=w.showLibrary();await settle();const b=w.showLibrary();await settle();second.resolve({items:[{id:'new',title:'Latest page',kind:'document',updatedAt:0}]});await b;first.resolve({items:[{id:'old',title:'Old page',kind:'document',updatedAt:0}]});await a;assert.match(w.$('work-library').textContent,/Latest page/);assert.doesNotMatch(w.$('work-library').textContent,/Old page/);
});
test('the global Library keeps the selected project and lists local pages across projects',async t=>{
  const h=harness(t),w=h.w;w.desktop.projectId='codex-project';await w.showLibrary();assert.equal(w.desktop.projectId,'codex-project');assert.equal(w.desktop.mode,'codex');assert.equal(h.calls[0].method,'desktop/document/list');assert.equal(Object.keys(h.calls[0].params).length,0);
});
test('the local page tree renders the same single Library response without another fetch',async t=>{
  const h=harness(t),w=h.w,local=readFileSync(new URL('../src/gui_work_local.js',import.meta.url),'utf8');const from=local.indexOf('renderLibraryDocuments=function('),to=local.indexOf('const docsLocalChoice=',from);assert.ok(from>=0);
  Object.assign(w,{docsButton:(text,run)=>{const b=w.document.createElement('button');b.textContent=text;b.onclick=run;return b;}});w.eval(local.slice(from,to));h.handler=()=>({items:[{id:'parent',title:'Parent page',kind:'document',updatedAt:0},{id:'child',parentId:'parent',title:'Child page',kind:'document',updatedAt:0}]});await w.showLibrary();assert.equal(h.calls.filter(c=>c.method==='desktop/document/list').length,1);assert.equal(w.document.querySelector('[aria-label="Subpage: Child page"]').textContent,'Child page');
});
