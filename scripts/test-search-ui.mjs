#!/usr/bin/env node
// Real DOM, shipped Markdown renderer and production timeline/notification functions.
// Install the locked src/vendor/editor dev dependencies in a temporary directory.
// EDITOR_TEST_NODE_MODULES=/tmp/editor/node_modules node --test scripts/test-search-ui.mjs
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import path from 'node:path';
import test from 'node:test';
const require=createRequire(import.meta.url);let JSDOM;
try{({JSDOM}=process.env.EDITOR_TEST_NODE_MODULES?require(path.join(process.env.EDITOR_TEST_NODE_MODULES,'jsdom')):createRequire(new URL('../src/vendor/editor/package.json',import.meta.url))('jsdom'));}catch{throw new Error('Install src/vendor/editor/package-lock.json in a temporary directory; set EDITOR_TEST_NODE_MODULES to its node_modules.');}
const html=readFileSync(new URL('../src/gui_app.html',import.meta.url),'utf8'),vendor=readFileSync(new URL('../src/vendor/markdown.js',import.meta.url),'utf8'),search=readFileSync(new URL('../src/gui_search.js',import.meta.url),'utf8');
const app=html.match(/<script>([\s\S]*?)<\/script>/)[1].replace('// MARKDOWN_VENDOR',()=>vendor);
const desktopSource=readFileSync(new URL('../src/gui_desktop.js',import.meta.url),'utf8');
const closeHelpers=['^function saveDraft\\(.*$','^async function savePreferences\\(.*$','^window\\.__requestClose=async function\\(\\)\\{.*$'].map(pattern=>{const match=desktopSource.match(new RegExp(pattern,'m'));assert.ok(match,'Missing production close helper');return match[0];}).join('\n');
const voiceSource=readFileSync(new URL('../src/gui_voice.js',import.meta.url),'utf8'),voiceFinishClose=voiceSource.match(/^async function finishVoiceClose\(\)\{[\s\S]*?^\}/m)?.[0];assert.ok(voiceFinishClose,'Missing production voice recovery close');
const documentsSource=readFileSync(new URL('../src/gui_documents.js',import.meta.url),'utf8'),documentLeave=['^function docsClearPrint\\(.*$','^function docsUnmount\\(\\)\\{[\\s\\S]*?^\\}','^async function docsLeave\\(\\)\\{[\\s\\S]*?^\\}'].map(pattern=>{const match=documentsSource.match(new RegExp(pattern,'m'));assert.ok(match,'Missing production document leave helper');return match[0];}).join('\n');
const plain=value=>JSON.parse(JSON.stringify(value));
function deferred(){let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};}
async function settle(){for(let i=0;i<20;i++)await Promise.resolve();await new Promise(resolve=>setTimeout(resolve,5));}
function harness(preferences={}){
  const dom=new JSDOM(html,{runScripts:'outside-only',pretendToBeVisual:true,url:'http://127.0.0.1:38471/'}),w=dom.window,calls=[],errors=[],sent=[];
  const h={window:w,calls,errors,sent,respond:null,closeSteps:[],flush:null};
  const rpc=scope=>async(method,params={})=>{calls.push({scope,method,params:plain(params)});const custom=h.respond?.(scope,method,params);if(custom!==undefined)return custom;
    if(method==='desktop/settings/update')return {settings:{preferences:{...preferences,...params.preferences}}};
    if(method==='web/search/capabilities')return {available:false,permissionsVerified:false,defaultModes:{chat:'auto',work:'auto',codex:'cached'},policy:{allowed:preferences.webSearchAllowed!==false},reason:'Connect an account on this host.'};
    if(method==='thread/webSearch/set')return {threadId:params.threadId,webSearchMode:params.mode,effectiveWebSearchMode:params.mode,webSearch:params.mode!=='disabled',webSearchVersion:1};
    throw new Error('Unexpected RPC '+method);
  };
  Object.assign(w,{console,TextEncoder,TextDecoder,desktop:{navigation:0,mode:'codex',state:{settings:{preferences:plain(preferences)}},drafts:new Map(),tabs:[]},desktopReady:Promise.resolve(),rpcLocalCall:rpc('local'),showError:error=>errors.push(error.message||String(error)),toast:()=>{},ipc:{postMessage:value=>sent.push(JSON.parse(value))},remoteUi:{active:null,threads:new Map(),byConnection:new Map(),connections:new Map()},_testRpc:rpc,_testFlush:()=>{h.closeSteps.push('flushDocument');return h.flush?.();}});
  w.eval(app+`\n
    refreshThreads=async()=>{};refreshGitStatus=async()=>{};setTurnStatus=()=>{};
    function remoteContext(){return currentThread&&remoteUi.threads.has(currentThread)?remoteUi.threads.get(currentThread):desktop.mode==='codex'?remoteUi.active:null;}
    function captureRpcContext(){const scope=remoteContext();if(!scope)return {key:'local',call:rpcLocalCall};const connection=remoteUi.connections.get(scope);if(connection?.state!=='connected')throw new Error('SSH disconnected');return {key:scope+'@'+connection.id,call:window._testRpc(scope+'@'+connection.id)};}
    function draftKey(){return currentThread||(desktop.mode==='codex'&&remoteUi.active?'codex@'+remoteUi.active:desktop.mode);}
    function restoreDraft(){$('composer-input').value=desktop.drafts.get(draftKey())||'';}
    function $(id){return document.getElementById(id);}
    const _testBaseSettings=renderSettingsSection;renderSettingsSection=async function(section){currentSettingsSection=section;$('settings-body').textContent='Other '+section;};
    function openSettingsView(section){document.body.classList.add('in-settings');return renderSettingsSection(section);}
    rpcCall=rpcLocalCall;function flushDocument(){return window._testFlush();}
    \n`+closeHelpers+'\nconst voiceBaseRequestClose=window.__requestClose;const voiceUi={unsaved:new Map(),closeRequested:true,open:false};function renderVoiceUi(){}\n'+voiceFinishClose+'\nconst baseCloseDrawer=closeDrawer;const documentsUi={current:null,navigation:0,layout:null};function docsHideFloating(){}\n'+documentLeave+'\n'+search+`\nwindow.searchHarness={
      ready:webSearchReady,items:itemEls,texts:itemTexts,state:webSearchUi,
      get thread(){return currentThread;},get mode(){return desktop.mode;},get navigation(){return desktop.navigation;},
      select(mode,threadId=null,profile=null){desktop.navigation++;closeFloatingMenu();desktop.mode=mode;currentThread=threadId;remoteUi.active=profile;if(threadId)remoteUi.threads.set(threadId,profile);clearTimeline();restoreDraft();},
      clear(){clearTimeline();},connection(profile,id,state='connected'){const c={id,profileId:profile,state};remoteUi.byConnection.set(id,c);remoteUi.connections.set(profile,c);},
      opened(thread){webSearchThreadOpened(thread);},request:webSearchRequest,set:webSearchSetMode,paint:webSearchPaint,menu:webSearchMenu,
      notify:handleServerNotification,upsert:upsertTimelineItem,started:webSearchThreadStarted,settings:webSearchSettings,
      preference(){return webSearchPreference();},capture:webSearchCapture,record(id){const s=webSearchCapture();return webSearchUi.items.get(webSearchItemKey(s.scope,s.threadId,id));},
      getSandbox(){return currentSandbox;},setSandbox(value){currentSandbox=value;},finishVoiceClose,
      get drawerTab(){return currentDrawerTab;},get drawerOpen(){return drawerOpen;},get document(){return documentsUi.current;},
      openSources:webSearchOpenSources,closeDrawer,switchDrawerTab,
      canvas(text='Unsaved document'){desktop.navigation++;documentsUi.navigation++;const shell=document.createElement('div');shell.className='canvas-test';const input=document.createElement('textarea');input.value=text;shell.append(input);const session={doc:{id:'canvas-doc',title:'Canvas'},shell,bridge:{mounted:true,unmount(){this.mounted=false;}}};documentsUi.current=session;documentsUi.layout='canvas';desktop.document=session.doc;drawerOpen=true;currentDrawerTab='document';$('inspector-drawer').classList.remove('closed');$('inspector-drawer').classList.add('document-open');$('drawer-content').replaceChildren(shell);return session;},
    };`);
  h.ui=w.searchHarness;h.node=id=>w.document.getElementById(id);h.destroy=()=>w.close();h.click=async node=>{assert.ok(node,'Missing DOM control');node.click();await settle();};return h;
}
const thread=(id,mode='auto',version=0,extra={})=>({id,webSearchMode:mode,webSearchVersion:version,effectiveWebSearchMode:mode==='auto'?'cached':mode,webSearch:mode!=='disabled',...extra});
const notify=(h,method,item,extra={})=>h.ui.notify(method,{threadId:h.ui.thread,turnId:'turn-1',...(item?{item}:{}),...extra});
function message(id='answer',text='中文😀 **bold** [normal](https://normal.example/a)'){
  const citations=[{type:'url_citation',url:'https://cited.example/a',title:'Cited 中文',startIndex:1,endIndex:2,indexUnit:'provider'},{type:'url_citation',url:'javascript:globalThis.compromised=true',title:'<img src=x onerror=alert(1)>',startIndex:0,endIndex:3,indexUnit:'provider'}];
  return {id,type:'agentMessage',text,parts:[{type:'output_text',text,citations}],citations};
}

test('Automatic is the default in all modes; drafts persist independently and restore after restart',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;
  for(const mode of ['chat','work','codex']){h.ui.select(mode);assert.deepEqual(plain(h.ui.request()),{webSearchMode:'auto'});}
  h.ui.select('chat');await h.ui.set('required');h.ui.select('work');assert.equal(h.ui.request().webSearchMode,'auto');await h.ui.set('disabled');h.ui.select('chat');assert.equal(h.ui.request().webSearchMode,'required');
  const prefs=h.calls.filter(call=>call.method==='desktop/settings/update').at(-1).params.preferences;
  const second=harness(prefs);t.after(second.destroy);await second.ui.ready;second.ui.select('work');assert.equal(second.ui.request().webSearchMode,'disabled');second.ui.select('chat');assert.equal(second.ui.request().webSearchMode,'required');
  assert.ok(h.calls.filter(call=>call.method==='desktop/settings/update').every(call=>call.scope==='local'));
});

test('preferred Automatic survives effective Off and version protects captured requests from older updates',async t=>{
  const h=harness({webSearchAllowed:false});t.after(h.destroy);await h.ui.ready;h.ui.select('codex','same');h.ui.opened(thread('same','auto',4,{effectiveWebSearchMode:'disabled',webSearch:false}));
  const captured=h.ui.request();assert.deepEqual(plain(captured),{webSearchMode:'auto',webSearchVersion:4});assert.match(h.node('web-search-control').textContent,/Automatic/);
  notify(h,'thread/webSearch/updated',null,{webSearchMode:'disabled',effectiveWebSearchMode:'disabled',webSearch:false,webSearchVersion:5});
  h.ui.opened(thread('same','cached',3));assert.deepEqual(plain(h.ui.request()),{webSearchMode:'disabled',webSearchVersion:5});assert.deepEqual(plain(captured),{webSearchMode:'auto',webSearchVersion:4});
  // A completed old turn carries per-turn search metadata, not a sticky preference update.
  notify(h,'item/completed',{id:'old-user',type:'userMessage',content:[],webSearchMode:'required',effectiveWebSearchMode:'live'});assert.equal(h.ui.request().webSearchMode,'disabled');
});

test('thread setters serialize and late response cannot override a newer preference notification',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('codex','t');h.ui.opened(thread('t','auto',7));const slow=deferred();let count=0;
  h.respond=(scope,method,params)=>method==='thread/webSearch/set'?(++count===1?slow.promise:{threadId:'t',webSearchMode:params.mode,effectiveWebSearchMode:params.mode,webSearchVersion:9}):undefined;
  const first=h.ui.set('live');const second=h.ui.set('disabled');await settle();assert.equal(count,1);assert.equal(h.ui.request().webSearchMode,'disabled');
  slow.resolve({threadId:'t',webSearchMode:'live',effectiveWebSearchMode:'live',webSearchVersion:8});await Promise.all([first,second]);assert.equal(h.ui.request().webSearchMode,'disabled');assert.equal(h.ui.request().webSearchVersion,9);
  const delayed=deferred();h.respond=(scope,method)=>method==='thread/webSearch/set'?delayed.promise:undefined;const work=h.ui.set('auto');await settle();notify(h,'thread/webSearch/updated',null,{webSearchMode:'disabled',effectiveWebSearchMode:'disabled',webSearchVersion:11});delayed.resolve({threadId:'t',webSearchMode:'auto',effectiveWebSearchMode:'cached',webSearchVersion:10});await work;assert.equal(h.ui.request().webSearchMode,'disabled');assert.equal(h.ui.request().webSearchVersion,11);
});

test('slow thread creation preserves a newer draft and never changes the view selected meanwhile; fork restores persisted mode',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat');await h.ui.set('required');const request=h.ui.request();await h.ui.set('disabled');h.ui.select('work');await h.ui.set('live');h.ui.started(thread('created','required',1),'chat',request);await settle();assert.equal(h.ui.request().webSearchMode,'live');
  h.ui.select('chat');assert.equal(h.ui.request().webSearchMode,'disabled');await h.ui.set('required');const sameMode=h.ui.request();await h.ui.set('disabled');await h.ui.set('required');h.ui.started(thread('second','required',0),'chat',sameMode);await settle();assert.equal(h.ui.request().webSearchMode,'required','a newer edit back to the same mode must stay a draft');h.ui.select('chat','created');assert.equal(h.ui.request().webSearchMode,'required');h.ui.opened(thread('forked','cached',12)); // hook for another view must not win
  assert.equal(h.ui.request().webSearchMode,'required');h.ui.select('chat','forked');h.ui.opened(thread('forked','cached',12));assert.deepEqual(plain(h.ui.request()),{webSearchMode:'cached',webSearchVersion:12});
});

test('remote modes, fixed RPC contexts and draft persistence are isolated by stable profile; reconnect rejects old notifications',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.connection('one','c1');h.ui.connection('two','c2');h.ui.select('codex',null,'one');await h.ui.set('live');h.ui.select('codex',null,'two');assert.equal(h.ui.request().webSearchMode,'auto');await h.ui.set('disabled');h.ui.select('codex',null,'one');assert.equal(h.ui.request().webSearchMode,'live');
  h.ui.select('codex','same','one');h.ui.opened(thread('same','required',3,{connectionId:'c1'}));const pending=deferred();h.respond=(scope,method)=>method==='thread/webSearch/set'?pending.promise:undefined;const update=h.ui.set('live');await settle();h.ui.select('codex','same','two');h.ui.opened(thread('same','disabled',6,{connectionId:'c2'}));pending.resolve({threadId:'same',webSearchMode:'live',effectiveWebSearchMode:'live',webSearchVersion:4});await update;assert.equal(h.ui.request().webSearchMode,'disabled');assert.equal(h.calls.filter(call=>call.method==='thread/webSearch/set').at(-1).scope,'one@c1');
  h.ui.connection('two','c2-new');notify(h,'thread/webSearch/updated',null,{connectionId:'c2',webSearchMode:'required',webSearchVersion:90});assert.equal(h.ui.request().webSearchMode,'disabled');
  const saved=h.calls.filter(call=>call.method==='desktop/settings/update').at(-1).params.preferences.webSearchDrafts;assert.equal(saved[JSON.stringify(['one','codex@one'])],'live');assert.equal(saved[JSON.stringify(['two','codex@two'])],'disabled');
});

test('Search settings uses a captured host and policy Off keeps preferred Automatic; late settings cannot replace another panel',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.connection('host','connection');const cap=deferred();h.respond=(scope,method)=>method==='web/search/capabilities'&&scope==='host@connection'?cap.promise:undefined;h.ui.select('codex',null,'host');
  const loading=h.ui.settings();await settle();assert.ok(h.calls.some(call=>call.method==='web/search/capabilities'&&call.scope==='host@connection'));
  const checkbox=h.window.document.querySelector('[aria-label="Allow web search"]');checkbox.checked=false;await checkbox.onchange();assert.equal(h.calls.filter(call=>call.method==='desktop/settings/update').at(-1).scope,'host@connection');assert.equal(h.ui.request().webSearchMode,'auto');
  await h.window.renderSettingsSection('other');cap.resolve({available:true,permissionsVerified:false,defaultModes:{codex:'cached'},policy:{allowed:false}});await loading;assert.equal(h.node('settings-body').textContent,'Other other');
  await h.ui.settings();assert.match(h.node('settings-body').textContent,/permissions and model support are checked/);assert.ok(!h.node('settings-body').textContent.includes('permissions verified'));
});

test('only real hosted items produce search activity; failed and stopped turns never show successful search completion',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat','thread');h.ui.opened(thread('thread'));
  notify(h,'item/started',{id:'user',type:'userMessage',content:[{text:'Please search the web'}]});assert.equal(h.window.document.querySelectorAll('.web-search-activity').length,0);
  notify(h,'item/started',{id:'hosted',type:'webSearch',status:'inProgress',queries:['中文 😀 query'],sources:[]});let row=h.ui.items.get('hosted');assert.equal(row.dataset.status,'running');assert.match(row.textContent,/中文 😀 query/);
  notify(h,'item/completed',{id:'hosted',type:'webSearch',status:'completed',queries:['中文 😀 query'],sources:[{url:'https://search.example',title:'Result'}]});assert.equal(row.dataset.status,'completed');
  notify(h,'turn/completed',null,{turn:{id:'turn-1',status:'failed'}});assert.equal(row.dataset.status,'failed');assert.match(row.textContent,/answer failed/);
  h.ui.clear();notify(h,'item/started',{id:'stopped',type:'webSearch',status:'inProgress'});notify(h,'turn/completed',null,{turn:{id:'turn-1',status:'interrupted'}});row=h.ui.items.get('stopped');assert.equal(row.dataset.status,'stopped');assert.match(row.textContent,/Search stopped/);
});

test('an incomplete hosted item is terminal in live events and restored history even when its outer turn completed',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat','thread');h.ui.opened(thread('thread'));
  notify(h,'item/started',{id:'partial-search',type:'webSearch',status:'inProgress',queries:['partial query']});const row=h.ui.items.get('partial-search');assert.equal(row.dataset.status,'running');
  const item={id:'partial-search',type:'webSearch',status:'incomplete',queries:['partial query'],sources:[{url:'https://partial.example',title:'Partial source'}]};notify(h,'item/completed',item);assert.equal(row.dataset.status,'incomplete');assert.equal(row.querySelector('.web-search-activity-label').textContent,'Search incomplete');
  notify(h,'turn/completed',null,{turn:{id:'turn-1',status:'completed'}});assert.equal(row.dataset.status,'incomplete');assert.ok(!row.textContent.includes('Searching'));
  await h.click(row.querySelector('.web-search-sources-button'));assert.match(h.node('web-search-sources').textContent,/Partial source/);
  h.ui.clear();h.ui.upsert(item);h.ui.opened(thread('thread','auto',1,{turns:[{id:'turn-1',status:'completed',items:[item]}]}));const restored=h.ui.items.get('partial-search');assert.equal(restored.dataset.status,'incomplete');assert.equal(restored.querySelector('.web-search-activity-label').textContent,'Search incomplete');
});

test('citations preserve complete Chinese/emoji Markdown; unsafe sources stay text, cited and consulted lists remain distinct',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat','thread');h.ui.opened(thread('thread'));notify(h,'item/completed',{id:'search',type:'webSearch',status:'completed',sources:[{url:'https://consulted.example/x',title:'Consulted only'},{url:'data:text/html,bad',title:'Bad source'}]});
  const item=message();notify(h,'item/completed',item);const element=h.ui.items.get(item.id),paragraph=element.querySelector('p');assert.equal(paragraph.textContent,'中文😀 bold normal');assert.equal(paragraph.querySelector('strong').textContent,'bold');assert.equal(paragraph.querySelector('a').href,'https://normal.example/a');
  assert.equal(element.querySelectorAll('.web-citation').length,2);assert.equal(element.querySelectorAll('a.web-citation').length,1);assert.equal(element.querySelector('.web-citation.unsafe').tagName,'SPAN');assert.equal(element.querySelectorAll('img,iframe,script').length,0);
  await h.click(element.querySelector('.web-search-sources-button'));const panel=h.node('web-search-sources');assert.deepEqual(Array.from(panel.querySelectorAll('h3'),el=>el.textContent),['Cited sources','Search sources']);assert.match(panel.textContent,/Consulted only/);assert.match(panel.textContent,/Some may not be cited/);assert.equal(panel.querySelectorAll('a').length,2);assert.equal(panel.querySelectorAll('img').length,0);assert.ok(Array.from(panel.querySelectorAll('a')).every(a=>/^https?:/.test(a.href)));
  assert.equal(h.ui.texts.get(item.id),item.text);assert.ok(!h.calls.some(call=>/favicon|http/.test(call.method)));
});

test('streaming Markdown reattaches refs safely; completed message rejects late deltas; history recovers citations and failed status',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat','thread');h.ui.opened(thread('thread'));const item=message('m','Start 中文😀');notify(h,'item/started',item);notify(h,'item/agentMessage/delta',null,{itemId:'m',delta:' **half'});assert.equal(h.ui.items.get('m').querySelectorAll('.web-citation').length,2);
  notify(h,'item/completed',{...item,text:'Final 中文😀'});notify(h,'item/agentMessage/delta',null,{itemId:'m',delta:' LATE'});assert.equal(h.ui.items.get('m').querySelector('p').textContent,'Final 中文😀');
  h.ui.clear();const call={id:'history-search',type:'webSearch',status:'completed',sources:[{url:'https://history.example'}]};h.ui.upsert(call);h.ui.upsert(item);h.ui.opened(thread('thread','auto',1,{turns:[{id:'old-turn',status:'interrupted',items:[call,item]}]}));assert.equal(h.ui.items.get('history-search').dataset.status,'stopped');assert.equal(h.ui.items.get('m').querySelectorAll('.web-citation').length,2);notify(h,'item/agentMessage/delta',null,{itemId:'m',delta:' LATE HISTORY'});assert.equal(h.ui.texts.get('m'),item.text);
});

test('same IDs on another host or a replaced connection cannot render or alter Sources; late source details refresh only owned panel',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.connection('one','c1');h.ui.connection('two','c2');h.ui.select('codex','same','one');h.ui.opened(thread('same','auto',0,{connectionId:'c1'}));notify(h,'item/completed',message('same-message','Host one'),{connectionId:'c1'});const element=h.ui.items.get('same-message');
  notify(h,'item/completed',message('same-message','Host two'),{connectionId:'c2'});assert.equal(element.querySelector('p').textContent,'Host one');await h.click(element.querySelector('.web-search-sources-button'));notify(h,'item/completed',{id:'lookup',type:'webSearch',status:'completed',sources:[{url:'https://late.example',title:'Late source'}]},{connectionId:'c1'});assert.match(h.node('web-search-sources').textContent,/Late source/);
  h.ui.connection('one','new-c1');notify(h,'item/agentMessage/delta',null,{connectionId:'c1',itemId:'same-message',delta:'STALE'});assert.equal(element.querySelector('p').textContent,'Host one');h.window.openCustomModal('Other','<p id="keep-modal">Other modal</p>');notify(h,'item/completed',{id:'lookup',type:'webSearch',status:'completed',sources:[{url:'https://new.example'}]},{connectionId:'new-c1'});assert.equal(h.node('keep-modal').textContent,'Other modal');
});

test('menu changes the real next-send mode, toggles closed, and detached menu callbacks cannot change another draft',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat');h.ui.menu({currentTarget:h.node('web-search-control'),stopPropagation(){}});await settle();const required=h.node('floating-menu').querySelector('[data-search-mode=required]');await h.click(required);assert.equal(h.ui.request().webSearchMode,'required');assert.ok(!h.node('floating-menu').classList.contains('open'));
  h.ui.menu({currentTarget:h.node('web-search-control'),stopPropagation(){}});const stale=h.node('floating-menu').querySelector('[data-search-mode=disabled]');h.ui.select('work');await h.click(stale);assert.equal(h.ui.request().webSearchMode,'auto');h.ui.menu({currentTarget:h.node('web-search-control'),stopPropagation(){}});h.ui.menu({currentTarget:h.node('web-search-control'),stopPropagation(){}});assert.ok(!h.node('floating-menu').classList.contains('open'));
});

test('a capabilities response started before policy Off cannot turn it back on; a failed thread write restores the last authoritative preference',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.connection('policy-host','pc');const oldCap=deferred();h.respond=(scope,method)=>scope==='policy-host@pc'&&method==='web/search/capabilities'?oldCap.promise:undefined;h.ui.select('codex',null,'policy-host');
  const panel=h.ui.settings();await settle();const input=h.window.document.querySelector('[aria-label="Allow web search"]');input.checked=false;await input.onchange();oldCap.resolve({available:true,permissionsVerified:false,defaultModes:{codex:'cached'},policy:{allowed:true}});await panel;assert.equal(input.checked,false);assert.match(h.node('web-search-control').title,/disabled in Search settings/);assert.equal(h.ui.request().webSearchMode,'auto');
  h.ui.select('codex','failure','policy-host');h.ui.opened(thread('failure','cached',8,{connectionId:'pc'}));h.respond=(scope,method)=>method==='thread/webSearch/set'?Promise.reject(new Error('Disconnected during save')):undefined;await assert.rejects(h.ui.set('disabled'),/Disconnected/);assert.deepEqual(plain(h.ui.request()),{webSearchMode:'cached',webSearchVersion:8});
});

test('sources stay within the actual turn and every unsafe citation protocol or credential URL stays non-clickable',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat','thread');h.ui.opened(thread('thread'));notify(h,'item/completed',{id:'first-search',type:'webSearch',status:'completed',sources:[{url:'https://first.example',title:'First turn'}]});notify(h,'item/completed',message('first-answer','First'));
  notify(h,'item/started',{id:'second-user',type:'userMessage',content:[{text:'Second'}]},{turnId:'turn-2'});const citations=['javascript:alert(1)','data:text/html,bad','file:///tmp/local','mailto:a@example.com','https://name:secret@example.com','https://good.example/\nattack'].map(url=>({type:'url_citation',url,title:'Unsafe',indexUnit:'provider'}));notify(h,'item/completed',{id:'second-answer',type:'agentMessage',text:'Second 😀 answer',parts:[{type:'output_text',text:'Second 😀 answer',citations}],citations},{turnId:'turn-2'});
  const element=h.ui.items.get('second-answer');assert.equal(element.querySelectorAll('.web-citation').length,6);assert.equal(element.querySelectorAll('a.web-citation').length,0);await h.click(element.querySelector('.web-search-sources-button'));assert.ok(!h.node('web-search-sources').textContent.includes('First turn'));assert.equal(h.node('web-search-sources').querySelectorAll('a,img').length,0);
});

test('account changes refresh only the matching fixed host and ignore stale capability replies from before the account change',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.connection('auth-host','auth-c');const old=deferred();let count=0;h.respond=(scope,method)=>scope==='auth-host@auth-c'&&method==='web/search/capabilities'?(++count===1?old.promise:{available:false,permissionsVerified:false,defaultModes:{codex:'cached'},policy:{allowed:false},reason:'Remote account signed out.'}):undefined;h.ui.select('codex',null,'auth-host');
  const settings=h.ui.settings();await settle();h.window.__onServerMessage({method:'remote/notification',params:{connectionId:'auth-c',method:'account/updated',params:{account:null}}});await settle();assert.equal(count,2);old.resolve({available:true,permissionsVerified:false,defaultModes:{codex:'live'},policy:{allowed:true}});await settings;assert.match(h.node('web-search-control').title,/disabled in Search settings/);
  // Opening the section again must use the new account result, not the stale response.
  await h.ui.settings();assert.match(h.node('settings-body').textContent,/Remote account signed out/);h.ui.connection('auth-host','replacement');h.window.__onServerMessage({method:'remote/notification',params:{connectionId:'auth-c',method:'account/updated',params:{account:{}}}});await settle();assert.equal(count,3,'a replaced connection must not start a refresh');
});

test('native close waits for a pending search draft write before delegating the production document/draft close chain',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat');h.node('composer-input').value='Keep my draft';const saved=deferred();h.respond=(scope,method,params)=>method==='desktop/settings/update'&&params.preferences?.webSearchDrafts?saved.promise:undefined;
  const changing=h.ui.set('required');await settle();const closing=h.window.__requestClose();await settle();assert.equal(h.sent.filter(message=>message.__window==='close').length,0);assert.deepEqual(h.closeSteps,[]);
  saved.resolve({settings:{preferences:{webSearchDrafts:{[JSON.stringify(['local','chat'])]:'required'}}}});await Promise.all([changing,closing]);assert.deepEqual(h.closeSteps,['flushDocument']);assert.equal(h.sent.filter(message=>message.__window==='close').length,1);assert.ok(h.calls.some(call=>call.method==='desktop/settings/update'&&call.params.preferences?.drafts?.chat==='Keep my draft'));
});

test('native close retains a failed local search draft and retries its latest snapshot on the next close',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat');const disk=deferred();h.respond=(scope,method,params)=>method==='desktop/settings/update'&&params.preferences?.webSearchDrafts?disk.promise:undefined;
  const changing=h.ui.set('required').catch(error=>error);await settle();h.window.document.body.inert=true;const closing=h.window.__requestClose();await settle();disk.reject(new Error('Disk is full'));await Promise.all([changing,closing]);assert.equal(h.sent.filter(message=>message.__window==='close').length,0);assert.equal(h.sent.filter(message=>message.__window==='closeCancelled').length,1);assert.equal(h.window.document.body.inert,false);assert.deepEqual(h.closeSteps,[]);assert.equal(h.ui.request().webSearchMode,'required');assert.equal(h.window.desktop.state.settings.preferences.webSearchDrafts,undefined);assert.ok(h.errors.some(error=>error.includes('Search preferences could not be saved')));
  h.respond=null;await h.window.__requestClose();const saves=h.calls.filter(call=>call.method==='desktop/settings/update'&&call.params.preferences?.webSearchDrafts);assert.equal(saves.length,2);assert.equal(saves[1].params.preferences.webSearchDrafts[JSON.stringify(['local','chat'])],'required');assert.equal(h.sent.filter(message=>message.__window==='close').length,1);assert.deepEqual(h.closeSteps,['flushDocument']);
});

test('native close drains a newer queued draft and still waits for the downstream document save',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('work');const first=deferred(),latest=deferred(),documentSave=deferred();let count=0;h.flush=()=>documentSave.promise;h.respond=(scope,method,params)=>method==='desktop/settings/update'&&params.preferences?.webSearchDrafts?(++count===1?first.promise:latest.promise):undefined;
  const a=h.ui.set('required');await settle();const closing=h.window.__requestClose();const b=h.ui.set('disabled');first.resolve({settings:{preferences:{}}});await settle();assert.equal(count,2);assert.equal(h.sent.filter(message=>message.__window==='close').length,0);assert.deepEqual(h.closeSteps,[]);latest.resolve({settings:{preferences:{}}});await Promise.all([a,b]);await settle();assert.deepEqual(h.closeSteps,['flushDocument']);assert.equal(h.sent.filter(message=>message.__window==='close').length,0);documentSave.resolve();await closing;assert.equal(h.sent.filter(message=>message.__window==='close').length,1);assert.equal(h.calls.filter(call=>call.params.preferences?.webSearchDrafts).at(-1).params.preferences.webSearchDrafts[JSON.stringify(['local','work'])],'disabled');
});

test('native close waits for a fixed-host thread setter after navigation; remote failure cancels close and restores the original preference',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.connection('writer','connection');h.ui.select('codex','remote-thread','writer');h.ui.opened(thread('remote-thread','cached',8,{connectionId:'connection'}));const remoteSave=deferred();h.respond=(scope,method)=>method==='thread/webSearch/set'?remoteSave.promise:undefined;
  const changing=h.ui.set('disabled').catch(error=>error);await settle();h.ui.select('chat');const closing=h.window.__requestClose();await settle();assert.equal(h.sent.filter(message=>message.__window==='close').length,0);assert.deepEqual(h.closeSteps,[]);remoteSave.reject(new Error('SSH connection lost'));await Promise.all([changing,closing]);assert.equal(h.sent.filter(message=>message.__window==='closeCancelled').length,1);assert.equal(h.sent.filter(message=>message.__window==='close').length,0);assert.equal(h.calls.filter(call=>call.method==='thread/webSearch/set').at(-1).scope,'writer@connection');h.ui.select('codex','remote-thread','writer');assert.deepEqual(plain(h.ui.request()),{webSearchMode:'cached',webSearchVersion:8});
  h.respond=(scope,method,params)=>method==='thread/webSearch/set'?{threadId:'remote-thread',webSearchMode:params.mode,effectiveWebSearchMode:params.mode,webSearchVersion:9}:undefined;await h.ui.set('disabled');await h.window.__requestClose();assert.equal(h.sent.filter(message=>message.__window==='close').length,1);assert.deepEqual(plain(h.ui.request()),{webSearchMode:'disabled',webSearchVersion:9});
});

test('native close also waits for Search policy writes and never treats a rejected remote policy as saved',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.connection('policy-writer','pc');h.ui.select('codex',null,'policy-writer');await h.ui.settings();const policySave=deferred();h.respond=(scope,method,params)=>method==='desktop/settings/update'&&params.preferences?.webSearchAllowed===false?policySave.promise:undefined;const input=h.window.document.querySelector('[aria-label="Allow web search"]');input.checked=false;const changing=input.onchange();const closing=h.window.__requestClose();await settle();assert.equal(h.sent.filter(message=>message.__window==='close').length,0);assert.deepEqual(h.closeSteps,[]);policySave.reject(new Error('Remote settings are unavailable'));await Promise.all([changing,closing]);assert.equal(input.checked,true);assert.equal(h.sent.filter(message=>message.__window==='closeCancelled').length,1);assert.equal(h.sent.filter(message=>message.__window==='close').length,0);assert.equal(h.calls.filter(call=>call.params.preferences?.webSearchAllowed===false).at(-1).scope,'policy-writer@pc');
  h.respond=null;await h.window.__requestClose();assert.equal(h.sent.filter(message=>message.__window==='close').length,1);assert.equal(h.calls.filter(call=>call.params.preferences?.webSearchAllowed===false).length,1,'failed remote writes require an explicit new choice; closing must not silently retry them');
});

test('voice recovery final close cannot bypass a newer search write through its captured production delegate',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat');const disk=deferred();h.respond=(scope,method,params)=>method==='desktop/settings/update'&&params.preferences?.webSearchDrafts?disk.promise:undefined;const changing=h.ui.set('disabled').catch(error=>error);await settle();const closing=h.ui.finishVoiceClose();await settle();assert.equal(h.sent.filter(message=>message.__window==='close').length,0);assert.deepEqual(h.closeSteps,[]);disk.reject(new Error('Search save failed after transcript recovery'));await Promise.all([changing,closing]);assert.equal(h.sent.filter(message=>message.__window==='closeCancelled').length,1);assert.equal(h.sent.filter(message=>message.__window==='close').length,0);h.respond=null;await h.ui.finishVoiceClose();assert.equal(h.sent.filter(message=>message.__window==='close').length,1);assert.deepEqual(h.closeSteps,['flushDocument']);
});

test('Sources is a real non-modal inspector tab; chat stays editable and X/Escape restore focus',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat','thread');h.ui.opened(thread('thread'));notify(h,'item/completed',message());const button=h.ui.items.get('answer').querySelector('.web-search-sources-button'),input=h.node('composer-input');input.value='Continue drafting 中文😀';button.focus();await h.click(button);
  const panel=h.node('web-search-sources'),drawer=h.node('inspector-drawer');assert.equal(panel.parentElement,h.node('drawer-content'));assert.equal(drawer.parentElement,h.node('app'));assert.equal(h.ui.drawerTab,'sources');assert.equal(h.ui.drawerOpen,true);assert.ok(!drawer.classList.contains('closed'));assert.ok(h.node('tab-btn-sources').classList.contains('active'));assert.ok(!h.node('modal-backdrop').classList.contains('open'));assert.equal(panel.closest('[aria-modal=true]'),null);assert.notEqual(h.window.document.body.inert,true);assert.equal(input.disabled,false);input.focus();input.value+=' next';assert.equal(input.value,'Continue drafting 中文😀 next');
  // Execute the actual shared X button's production inline action.
  const close=drawer.querySelector('.drawer-hdr>.icon-btn');close.focus();h.window.eval(close.getAttribute('onclick'));await settle();assert.equal(h.ui.drawerOpen,false);assert.equal(h.window.document.activeElement,button);assert.equal(input.value,'Continue drafting 中文😀 next');
  await h.click(h.node('tab-btn-sources'));assert.equal(h.ui.drawerOpen,true);const escape=new h.window.KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true});h.node('web-search-sources').dispatchEvent(escape);await settle();assert.equal(escape.defaultPrevented,true);assert.equal(h.ui.drawerOpen,false);assert.ok(!h.node('modal-backdrop').classList.contains('open'));
  await h.click(h.node('tab-btn-sources'));input.focus();input.dispatchEvent(new h.window.KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true}));await settle();assert.equal(h.ui.drawerOpen,false);assert.equal(h.window.document.activeElement,input,'closing Sources must not steal focus from the active composer');
});

test('Sources keeps canvas DOM and edits on save failure, then leaves via production docsLeave only after save succeeds',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat','thread');h.ui.opened(thread('thread'));notify(h,'item/completed',message());const session=h.ui.canvas('Keep unsaved 中文😀');h.flush=()=>Promise.reject(new Error('Canvas save failed'));await h.click(h.ui.items.get('answer').querySelector('.web-search-sources-button'));assert.equal(h.node('web-search-sources'),null);assert.equal(h.ui.document,session);assert.equal(session.shell.isConnected,true);assert.equal(session.bridge.mounted,true);assert.equal(session.shell.querySelector('textarea').value,'Keep unsaved 中文😀');assert.equal(h.ui.drawerTab,'document');assert.ok(h.node('inspector-drawer').classList.contains('document-open'));assert.ok(h.errors.includes('Canvas save failed'));
  h.flush=()=>Promise.resolve();await h.ui.openSources(h.ui.record('answer'));assert.equal(h.ui.document,null);assert.equal(session.bridge.mounted,false);assert.equal(session.shell.isConnected,false);assert.equal(session.shell.querySelector('textarea').value,'Keep unsaved 中文😀');assert.equal(h.ui.drawerTab,'sources');assert.equal(h.node('web-search-sources').parentElement,h.node('drawer-content'));assert.ok(!h.node('modal-backdrop').classList.contains('open'));
});

test('a delayed Sources open cannot replace new Settings or a replacement canvas during production docsLeave',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat','thread');h.ui.opened(thread('thread'));notify(h,'item/completed',message());const session=h.ui.canvas('Original canvas');const saved=deferred();h.flush=()=>saved.promise;const opening=h.ui.openSources(h.ui.record('answer'));await settle();h.window.desktop.navigation++;await h.window.openSettingsView('other');saved.resolve();await opening;assert.equal(h.node('settings-body').textContent,'Other other');assert.equal(h.node('web-search-sources'),null);assert.equal(h.ui.document,session);assert.equal(session.shell.querySelector('textarea').value,'Original canvas');
  h.window.document.body.classList.remove('in-settings');const leaving=deferred();let count=0;h.flush=()=>++count===1?Promise.resolve():leaving.promise;const pending=h.ui.openSources(h.ui.record('answer'));await settle();assert.equal(count,2);const replacement=h.ui.canvas('New canvas must stay');leaving.resolve();await pending;assert.equal(h.ui.document,replacement);assert.equal(replacement.shell.isConnected,true);assert.equal(replacement.bridge.mounted,true);assert.equal(replacement.shell.querySelector('textarea').value,'New canvas must stay');assert.equal(h.node('web-search-sources'),null);
});

test('a replaced SSH connection cancels a delayed Sources open without closing the local canvas',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.connection('source-host','old');h.ui.select('codex','thread','source-host');h.ui.opened(thread('thread','auto',0,{connectionId:'old'}));notify(h,'item/completed',message(),{connectionId:'old'});const session=h.ui.canvas('Local doc on SSH chat');const saved=deferred();h.flush=()=>saved.promise;const opening=h.ui.openSources(h.ui.record('answer'));await settle();h.ui.connection('source-host','new');saved.resolve();await opening;assert.equal(h.node('web-search-sources'),null);assert.equal(h.ui.document,session);assert.equal(session.shell.isConnected,true);assert.equal(session.bridge.mounted,true);assert.equal(h.ui.drawerTab,'document');
});

test('Sources switching invalidates old Files responses and preserves the production Terminal tab behavior',async t=>{
  const h=harness();t.after(h.destroy);await h.ui.ready;h.ui.select('chat','thread');h.ui.opened(thread('thread'));notify(h,'item/completed',message());const directory=deferred();h.respond=(scope,method)=>method==='fs/readDirectory'?directory.promise:undefined;h.ui.switchDrawerTab('files');await settle();await h.ui.openSources(h.ui.record('answer'));directory.resolve({cwd:'/old',files:[{path:'/old/late-file'}]});await settle();assert.equal(h.ui.drawerTab,'sources');assert.equal(h.node('web-search-sources').parentElement,h.node('drawer-content'));assert.ok(!h.node('drawer-content').textContent.includes('late-file'));
  h.ui.switchDrawerTab('terminal');await settle();assert.equal(h.ui.drawerTab,'terminal');assert.equal(h.node('web-search-sources'),null);assert.ok(h.node('quick-cmd-input'));assert.ok(!h.node('tab-btn-sources').classList.contains('active'));await h.click(h.node('tab-btn-sources'));assert.equal(h.ui.drawerTab,'sources');assert.ok(h.node('web-search-sources'));assert.equal(h.node('quick-cmd-input'),null);
});
