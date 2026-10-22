import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import path from 'node:path';
import test from 'node:test';
const require=createRequire(import.meta.url);
const {JSDOM}=process.env.EDITOR_TEST_NODE_MODULES?require(path.join(process.env.EDITOR_TEST_NODE_MODULES,'jsdom')):createRequire(new URL('../src/vendor/editor/package.json',import.meta.url))('jsdom');
const html=readFileSync(new URL('../src/gui_app.html',import.meta.url),'utf8').replace(/<style>[\s\S]*?<\/style>/g,'').replace('// MARKDOWN_VENDOR',()=>readFileSync(new URL('../src/vendor/markdown.js',import.meta.url),'utf8'));
const plain=value=>JSON.parse(JSON.stringify(value));
const deferred=()=>{let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};};
const settle=async()=>{await new Promise(resolve=>setImmediate(resolve));};
function harness(t){
  const calls=[],errors=[],h={host:'local',handler:null};
  const call=async(method,params={})=>{calls.push({host:h.host,method,params:plain(params)});return h.handler?.(method,params)??{};};
  const dom=new JSDOM(html,{url:'http://127.0.0.1/',runScripts:'dangerously',pretendToBeVisual:true,beforeParse(w){
    w.desktop={mode:'codex',navigation:0};w.captureRpcContext=()=>({key:h.host,call});w.showError=error=>errors.push(error.message);
  }});t.after(()=>dom.window.close());const w=dom.window;
  w.rpcCall=call;w.eval("currentCwd='/fixture';currentThread='fixture-chat';");
  h.w=w;h.calls=calls;h.errors=errors;h.menu=w.document.getElementById('floating-menu');h.box=w.document.getElementById('drawer-content');
  h.click=element=>{element.dispatchEvent(new w.MouseEvent('mousedown',{bubbles:true}));element.click();};
  h.open=()=>h.click(w.document.getElementById('openin-btn'));
  return h;
}
test('the shipped Open button lists backend targets and launches the selected app in this workspace',async t=>{
  const h=harness(t);h.handler=method=>method==='openIn/list'?{targets:[{id:'finder',label:'Finder'},{id:'vscode',label:'VS Code'}]}:{ok:true};
  h.open();await settle();assert.deepEqual([...h.menu.querySelectorAll('button')].map(b=>b.textContent.trim()),['Finder','VS Code']);
  h.click(h.menu.querySelector('[data-ui-value="vscode"]'));await settle();assert.deepEqual(h.calls.at(-1),{host:'local',method:'openIn/open',params:{target:'vscode',path:'/fixture'}});assert.equal(h.menu.classList.contains('open'),false);
});
test('Open closes on a second click and a late list cannot reopen it or overwrite a newer list',async t=>{
  const first=deferred(),second=deferred(),h=harness(t);let n=0;h.handler=()=>++n===1?first.promise:second.promise;
  h.open();h.open();assert.equal(h.menu.classList.contains('open'),false);h.open();second.resolve({targets:[{id:'finder',label:'Latest app'}]});await settle();first.resolve({targets:[{id:'old',label:'Stale app'}]});await settle();assert.match(h.menu.textContent,/Latest app/);assert.doesNotMatch(h.menu.textContent,/Stale app/);
});
test('Open reports unavailable hosts and failures, and an old menu cannot launch in a new host',async t=>{
  const h=harness(t);h.handler=()=>({targets:[],reason:'Remote editor launch is not connected'});h.open();await settle();assert.match(h.menu.textContent,/Remote editor/);h.open();h.handler=()=>Promise.reject(new Error('Backend unavailable'));h.open();await settle();assert.match(h.menu.textContent,/Backend unavailable/);
  h.open();h.handler=()=>({targets:[{id:'finder',label:'Finder'}]});h.open();await settle();h.host='other';h.click(h.menu.querySelector('button'));await settle();assert.equal(h.calls.filter(c=>c.method==='openIn/open').length,0);assert.match(h.errors[0],/environment changed/);
});
test('Review refreshes existing cached changes and counters each time the actual toolbar button is opened',async t=>{
  const h=harness(t);let revision=0;h.handler=()=>({changedFiles:[{path:'change-'+(++revision),status:'M',added:revision,deleted:0}],totalAdded:revision,totalDeleted:0,diff:'fresh diff '+revision});h.w.eval("latestGitStatus={changedFiles:[],diff:'obsolete cache'};latestGitStatusContext='local\\0/fixture';");
  const button=h.w.document.getElementById('diff-toggle-btn');h.click(button);await settle();assert.match(h.box.textContent,/change-1/);h.click(button);h.click(button);await settle();assert.match(h.box.textContent,/change-2/);assert.doesNotMatch(h.box.textContent,/obsolete cache/);assert.equal(h.w.document.getElementById('top-diff-add').textContent,'+2');
});
test('a closed inspector ignores a delayed result and its buttons report load errors',async t=>{
  const h=harness(t),pending=deferred();h.handler=()=>pending.promise;h.click(h.w.document.getElementById('diff-toggle-btn'));h.w.closeDrawer();h.box.textContent='Closed view';pending.resolve({changedFiles:[],diff:'late result'});await settle();assert.equal(h.box.textContent,'Closed view');
  h.handler=()=>Promise.reject(new Error('Cannot read workspace'));h.click(h.w.document.querySelector('[title="Browse workspace files"]'));await settle();assert.deepEqual(h.errors,['Cannot read workspace']);
});
test('More-style menu anchors toggle closed on a second pointer click',t=>{
  const h=harness(t),button=h.w.document.createElement('button');h.w.document.body.append(button);button.onclick=()=>h.w.openFloatingMenuAt('more',button,'<button>Rename</button>',false);h.click(button);assert.equal(h.menu.classList.contains('open'),true);h.click(button);assert.equal(h.menu.classList.contains('open'),false);
});
