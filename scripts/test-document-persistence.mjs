// Exercise production persistence functions. DOM/editor rendering and transport are
// controlled here; the JSONL smoke validates the matching real Rust transaction.
import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFileSync} from 'node:fs';
import {webcrypto} from 'node:crypto';

const source=readFileSync(new URL('../src/gui_documents.js',import.meta.url),'utf8');
const desktopSource=readFileSync(new URL('../src/gui_desktop.js',import.meta.url),'utf8');
const navigationSource=desktopSource.slice(desktopSource.indexOf('async function switchMode(mode){'),desktopSource.indexOf('function fillPrompt('));
const vendorSource=readFileSync(new URL('../src/vendor/editor.js',import.meta.url),'utf8');
const plain=value=>JSON.parse(JSON.stringify(value));
const body=text=>({type:'doc',content:[{type:'paragraph',...(text?{content:[{type:'text',text}]}:{})}]});
const textOf=body=>body.content.map(node=>(node.content||[]).map(child=>child.text||'').join('')).join('\n');
const defer=()=>{let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};};
async function until(predicate,label){for(let i=0;i<100;i++){if(predicate())return;await new Promise(setImmediate);}assert.fail('Did not reach '+label);}

// Run the bundled, frozen Markdown parser itself. No npm install or network is
// needed, and no simulated "lossless" answer can hide a migration regression.
const vendorContext=vm.createContext({console,URL,TextEncoder,TextDecoder,crypto:webcrypto,window:{},setTimeout,clearTimeout,setInterval,clearInterval});
vm.runInContext(vendorSource,vendorContext,{filename:'vendor/editor.js'});
const vendor=vendorContext.window.EditorBridge;

class Element {
  constructor(tag='div'){
    this.tagName=tag.toUpperCase();this.children=[];this.dataset={};this.style={};this.value='';this.textContent='';this.hidden=false;this._connected=false;this.attributes={};
    const classes=new Set();this.classList={add:(...values)=>values.forEach(x=>classes.add(x)),remove:(...values)=>values.forEach(x=>classes.delete(x)),contains:value=>classes.has(value),toggle(value,on){const active=on??!classes.has(value);active?classes.add(value):classes.delete(value);return active;}};
  }
  get isConnected(){return this._connected||Boolean(this.parentNode?.isConnected);}set isConnected(value){this._connected=Boolean(value);}
  setAttribute(key,value){this.attributes[key]=String(value);}removeAttribute(key){delete this.attributes[key];}getAttribute(key){return this.attributes[key]??null;}
  append(...children){for(const child of children){child.parentNode=this;this.children.push(child);}}appendChild(child){this.append(child);return child;}
  insertBefore(child,before){const at=this.children.indexOf(before);if(at<0)this.append(child);else this.children.splice(at,0,child);}
  prepend(...children){for(const child of children)child.parentNode=this;this.children.unshift(...children);}
  replaceChildren(...children){for(const child of this.children)child.parentNode=null;this.children=[];this.append(...children);}addEventListener(){}removeEventListener(){}
  querySelectorAll(selector){const result=[];const matches=node=>selector.split(',').some(value=>value.startsWith('.')?String(node.className||'').split(' ').includes(value.slice(1)):value.startsWith('#')?node.id===value.slice(1):node.tagName===value.toUpperCase());const visit=node=>{for(const child of node.children||[]){if(matches(child))result.push(child);visit(child);}};visit(this);return result;}
  querySelector(selector){return this.querySelectorAll(selector)[0]||null;}closest(){return null;}contains(child){return this===child||this.children.some(node=>node.contains?.(child));}
  remove(){if(this.parentNode){this.parentNode.children=this.parentNode.children.filter(child=>child!==this);this.parentNode=null;}this.isConnected=false;this.removed=true;}focus(){}scrollIntoView(){}getBoundingClientRect(){return {top:0,bottom:10,left:0,right:10,width:10,height:10};}
}
function bridge(json){
  const pmDoc=value=>{const captured=plain(value);return {textBetween:(from,to)=>textOf(captured).slice(Math.max(0,from-1),Math.max(0,to-1))};};
  let value=plain(json);const instance={destroyed:false,mounted:false,appliedRanges:[],getJSON:()=>plain(value),setJSON(next){value=plain(next);instance.editor.state.doc=pmDoc(value);},mount(){instance.mounted=true;},unmount(){instance.mounted=false;},destroy(){instance.destroyed=true;instance.mounted=false;},canUndo:()=>false,canRedo:()=>false};
  instance.editor={on(){},isActive:()=>false,getAttributes:()=>({}),setEditable(editable){instance.editable=editable;},state:{selection:{from:1,to:1,empty:true},doc:pmDoc(value)},chain(){let replace;const chain={focus:()=>chain,insertContentAt(range,nodes){replace={range:plain(range),nodes:plain(nodes)};return chain;},run(){if(replace){instance.appliedRanges.push(replace);const old=textOf(value);instance.setJSON(body(old.slice(0,replace.range.from-1)+textOf({content:replace.nodes})+old.slice(replace.range.to-1)));}return true;}};return chain;}};return instance;
}
function fixture(id,text,extra={}){return {id,title:id,kind:'document',format:'richtext',schemaVersion:1,body:body(text),content:text+'\n\n',sourceMarkdown:null,revision:1,projectId:null,...extra};}
function harness(respond,options={}){
  const calls=[],errors=[],timers=new Map(),nodes=new Map(),store=new Map(),modals=[];let timerId=0;
  const node=id=>{if(!nodes.has(id))nodes.set(id,new Element());return nodes.get(id);};
  const c=vm.createContext({console,Map,Set,Date,JSON,Promise,Error,Array,Object,Number,URL,Blob,TextEncoder,TextDecoder,
    setTimeout(fn,ms){const id=++timerId;timers.set(id,{fn,ms});return id;},clearTimeout(id){timers.delete(id);},setInterval(){return ++timerId;},clearInterval(){},requestAnimationFrame(){return ++timerId;},cancelAnimationFrame(){},addEventListener(){},innerHeight:1000,innerWidth:1200,
    desktop:{navigation:0,documentNavigation:0,document:null,mode:'work',library:true,projectId:null,drafts:new Map()},currentThread:null,currentTurn:null,drawerOpen:false,currentDrawerTab:null,
    document:{getElementById:id=>nodes.get(id)||null,createElement:tag=>new Element(tag),createTextNode:text=>({textContent:text}),querySelector:()=>null,querySelectorAll:()=>[],addEventListener(){},body:new Element('body')},$:node,
    remoteUi:{modalRevision:0},navigator:{clipboard:{writeText:async()=>{}}},renderMarkdown:()=>'<p>Preview</p>',downloadText(){},
    titleCase:value=>value,escapeHtml:value=>String(value),uiIcon:()=>'',showError:error=>errors.push(error),toast(){},saveDraft(){},syncShellNavigation(){},closeFloatingMenu(){},
    closeModal(){c.remoteUi.modalRevision++;node('modal-body').replaceChildren();},baseCloseDrawer(){},hideLibrary(){},closeSettingsView(){},closeScheduledView(){},
    openCustomModal(title){c.remoteUi.modalRevision++;const parent=node('modal-body');parent.isConnected=true;const root=new Element();root.className='page-owned-modal';parent.replaceChildren(root);modals.push({title,root});},rpcSend(){},desktopReady:new Promise(()=>{}),
    flushDocument:async()=>{},openDocument:async()=>{},createDocument:async()=>{},showLibrary:async()=>{},closeDrawer(){},renderDrawerContent:async()=>{},openSettingsView:async()=>{},switchMode:async()=>{},newThread:async()=>{},openThread:async()=>{},openProject:async()=>{},
    mentionedFiles:[],desktopAttachments:[],renderComposerChips(){},clearTimeline(){},setTurnStatus(){},restoreDraft(){},renderTabs(){},updateModeUI(){},refreshThreads:async()=>{},
    EditorBridge:{importMarkdown:markdown=>plain(vendor.importMarkdown(markdown)),create:options=>bridge(options.json)},
    fetch(){throw Error('Document persistence tests must not use the network');},
    async rpcLocalCall(method,params={}){
      calls.push({method,params:plain(params)});
      if(respond){const result=respond(method,params,{c,store,calls});if(result!==undefined)return result;}
      const current=store.get(params.id);
      if(method==='desktop/document/read'){assert.ok(current,'Fixture not found '+params.id);return {document:plain(current)};}
      if(method==='desktop/document/update'){
        assert.ok(current,'Fixture not found '+params.id);assert.equal(typeof params.expectedRevision,'number','Save omitted its CAS token');
        if(params.expectedRevision!==current.revision)return {conflict:{expectedRevision:params.expectedRevision,actualRevision:current.revision},document:plain(current)};
        const next={...plain(current),...plain(params),revision:current.revision+1};delete next.expectedRevision;
        if(params.body)next.content=textOf(params.body)+'\n\n';store.set(params.id,next);return {document:plain(next)};
      }
      throw Error('Unexpected RPC '+method);
    },
  });c.window=c;
  c.rpcCall=(...args)=>c.rpcLocalCall(...args);
  if(options.productionNavigation)vm.runInContext(navigationSource,c,{filename:'gui_desktop.js navigation'});
  // These are existing base-app bindings, not functions under test. New wrappers
  // can capture a base function without requiring a whole rendered application.
  for(const match of source.matchAll(/const\s+\w*[Bb]ase\w*\s*=\s*(\w+)\s*;/g)){if(!(match[1] in c))c[match[1]]=async()=>{};}
  vm.runInContext(source,c,{filename:'gui_documents.js'});
  const state=vm.runInContext('documentsUi',c);
  const originalConflict=c.docsShowConflict;
  c.docsBuildFind=()=>new Element();c.docsShowConflict=session=>{session.conflictShown=true;};
  const originalBuild=c.docsBuildSession;
  function session(document,{cache=true,actual=false}={}){
    const value=plain(document);let result;
    if(actual)result=originalBuild(value);
    else{
      result={doc:value,title:Object.assign(new Element('input'),{value:value.title}),bridge:bridge(value.body),source:null,shell:new Element(),queue:Promise.resolve(),timer:null,saving:false,error:'',conflict:null,loading:false,deleted:false,sideRequest:0};
      result.savedFingerprint=c.docsSnapshot(result).fingerprint;
    }
    if(cache)state.sessions.set(value.id,result);return result;
  }
  c.docsBuildSession=document=>session(document);
  c.docsMountSession=(session,layout)=>{state.current=session;state.layout=layout;c.desktop.document=session.doc;};
  return {c,state,store,calls,errors,timers,session,originalBuild,originalConflict,modals};
}
function button(root,label){const found=root.querySelectorAll('button').find(node=>node.textContent===label);assert.ok(found,'Button not found: '+label);return found;}

test('lossless legacy Markdown is really upgraded on the first flush, before selection actions',async()=>{
  const h=harness(),legacy=fixture('legacy','abc',{format:'markdown',body:null,content:'abc',sourceMarkdown:'abc'});h.store.set(legacy.id,plain(legacy));
  const session=h.session(legacy,{actual:true});h.state.current=session;
  assert.equal(Boolean(session.bridge),true,'Real lossless Markdown parser did not open the rich editor');
  await h.c.flushDocument();
  assert.equal(h.store.get('legacy').format,'richtext','UI displayed rich content but left the persisted format as Markdown');
  assert.equal(h.store.get('legacy').revision,2);assert.deepEqual(plain(h.store.get('legacy').body),plain(vendor.importMarkdown('abc').json));
  assert.equal(h.store.get('legacy').sourceMarkdown,'abc');assert.equal(h.c.docsDirty(session),false);
});

test('unsupported Markdown keeps exact source and does not get silently upgraded',async()=>{
  const h=harness(),source='<script>original</script>\n\nA[^1]\n\n[^1]: note',legacy=fixture('source','',{format:'markdown',body:null,content:source,sourceMarkdown:source});h.store.set(legacy.id,plain(legacy));
  const session=h.session(legacy,{actual:true});h.state.current=session;assert.ok(session.source);await h.c.flushDocument();
  assert.equal(h.store.get('source').format,'markdown');assert.equal(h.store.get('source').content,source);assert.equal(h.calls.filter(x=>x.method==='desktop/document/update').length,0);
});

test('typing into A while B loads remains saved when navigation or native close flushes',async()=>{
  const gate=defer(),h=harness((method,params)=>method==='desktop/document/read'&&params.id==='B'?gate.promise:undefined);
  const aDoc=fixture('A','saved A'),bDoc=fixture('B','saved B');h.store.set('A',plain(aDoc));h.store.set('B',plain(bDoc));
  const a=h.session(aDoc);h.state.current=a;const opening=h.c.openDocument('B');
  await until(()=>h.calls.some(x=>x.method==='desktop/document/read'&&x.params.id==='B'),'B read in flight');
  a.bridge.setJSON(body('NEW INPUT WHILE B LOADS'));h.c.docsChanged(a);
  gate.resolve({document:plain(bDoc)});await opening;await h.c.flushDocument();
  assert.equal(textOf(h.store.get('A').body),'NEW INPUT WHILE B LOADS','A had only a pending timer when closing B; those edits would be lost');
  assert.equal(h.c.docsDirty(a),false);assert.equal(a.bridge.destroyed,false);
});

test('a dirty cached session is preserved when another window has a newer revision',async()=>{
  const h=harness(),old=fixture('A','original'),external=fixture('A','external newer',{revision:2}),bDoc=fixture('B','B');
  h.store.set('A',plain(external));h.store.set('B',plain(bDoc));const a=h.session(old),b=h.session(bDoc);h.state.current=b;
  a.bridge.setJSON(body('UNSAVED LOCAL TEXT'));
  let rejected;try{await h.c.openDocument('A');}catch(error){rejected=error;}
  const preserved=h.state.sessions.get('A');
  assert.equal(a.bridge.destroyed,false,'Opening a changed revision destroyed unsaved editor state');
  assert.equal(preserved,a,'The dirty cache was discarded before CAS/conflict review');
  assert.equal(textOf(a.bridge.getJSON()),'UNSAVED LOCAL TEXT');assert.equal(textOf(h.store.get('A').body),'external newer','A stale cached document overwrote another window');
  assert.ok(rejected||a.conflict||h.state.current===a,'No visible or blocking resolution path remains for local unsaved edits');
});

test('saving in flight captures later input and uses the next revision for its second write',async()=>{
  const pending=[],h=harness((method,params)=>{if(method!=='desktop/document/update')return;const gate=defer();pending.push({gate,params:plain(params)});return gate.promise;});
  const original=fixture('A','initial');h.store.set('A',plain(original));const a=h.session(original);h.state.current=a;a.bridge.setJSON(body('first edit'));
  const saving=h.c.docsFlush(a);await until(()=>pending.length===1,'first save');a.bridge.setJSON(body('SECOND INPUT DURING SAVE'));
  pending[0].gate.resolve({document:{...plain(original),...pending[0].params,revision:2}});await until(()=>pending.length===2,'follow-up save');
  assert.equal(pending[0].params.expectedRevision,1);assert.equal(pending[1].params.expectedRevision,2);assert.equal(textOf(pending[1].params.body),'SECOND INPUT DURING SAVE');
  pending[1].gate.resolve({document:{...plain(original),...pending[1].params,revision:3}});await saving;
  assert.equal(a.doc.revision,3);assert.equal(h.c.docsDirty(a),false);
});

test('a CAS conflict leaves the local editor dirty and rejects subsequent close flushes',async()=>{
  const h=harness(),original=fixture('A','initial'),external=fixture('A','another window',{revision:2});h.store.set('A',plain(external));const a=h.session(original);h.state.current=a;a.bridge.setJSON(body('KEEP MY EDITS'));
  await assert.rejects(h.c.docsFlush(a));assert.equal(textOf(a.bridge.getJSON()),'KEEP MY EDITS');assert.equal(h.c.docsDirty(a),true);assert.equal(a.conflict.revision,2);
  const count=h.calls.length;await assert.rejects(h.c.flushDocument());assert.equal(h.calls.length,count,'A conflicting document was automatically retried or ignored');
  assert.equal(textOf(h.store.get('A').body),'another window');assert.equal(a.bridge.destroyed,false);
});

test('conflict copy cannot destroy input typed after the copy request started',async()=>{
  const gate=defer(),h=harness((method)=>method==='desktop/document/create'?gate.promise:undefined);
  const external=fixture('A','external',{revision:2}),original=fixture('A','original');h.store.set('A',plain(external));
  const a=h.session(original);a.bridge.setJSON(body('FIRST COPY SNAPSHOT'));a.conflict=plain(external);h.state.current=a;h.c.docsShowConflict=h.originalConflict;h.c.docsShowConflict(a);
  const dialog=h.modals.at(-1).root,copying=button(dialog,'Keep my edits as a new document').onclick();
  await until(()=>h.calls.some(call=>call.method==='desktop/document/create'),'copy request');
  // The actual UI must either prevent this edit, or retain it after the older
  // snapshot was saved. A test bridge can bypass an editor lock, so only inject
  // the user edit when production code leaves editing enabled.
  const editable=!a.locked;
  if(editable)a.bridge.setJSON(body('LATEST INPUT AFTER COPY CLICK'));
  const request=h.calls.find(call=>call.method==='desktop/document/create').params;
  const copy=fixture('copy','',{...request,id:'copy',revision:1});h.store.set('copy',plain(copy));gate.resolve({document:plain(copy)});await copying;
  if(editable){const saved=[...h.store.values()].some(doc=>textOf(doc.body)==='LATEST INPUT AFTER COPY CLICK');const retained=!a.deleted&&!a.bridge.destroyed&&h.state.sessions.get('A')===a;assert.ok(saved||retained,'Copy completion discarded newer input that was not in the saved copy');}
  assert.equal(h.errors.length,0,h.errors.map(error=>error.message).join('; '));
});

test('a committed restore is reconciled even if its dialog was closed while waiting',async()=>{
  const gate=defer(),original=fixture('A','CURRENT'),restored=fixture('A','RESTORED',{revision:2});
  const h=harness((method,params)=>{
    if(method==='desktop/document/history/read')return {version:{title:'Earlier',content:'RESTORED'}};
    if(method==='desktop/document/restore'){assert.equal(params.expectedRevision,1);h.store.set('A',plain(restored));return gate.promise;}
  });h.store.set('A',plain(original));const a=h.session(original);h.state.current=a;h.c.docsOpenSide=async()=>{};
  await h.c.docsReviewVersion(a,1);const dialog=h.modals.at(-1).root,restoring=button(dialog,'Restore this revision').onclick();
  await until(()=>h.calls.some(call=>call.method==='desktop/document/restore'),'restore request');h.c.closeModal();gate.resolve({document:plain(restored)});await restoring;
  const current=h.state.sessions.get('A');assert.ok(current&&!current.deleted);assert.equal(textOf(current.bridge.getJSON()),'RESTORED','Restore committed but the still-editable UI retained an older body');assert.equal(current.doc.revision,2);assert.equal(h.c.docsDirty(current),false);
});

test('committed delete clears its editor even if the confirmation dialog was closed',async()=>{
  const gate=defer(),h=harness((method)=>{if(method==='desktop/document/delete'){h.store.delete('A');return gate.promise;}}),original=fixture('A','CURRENT');h.store.set('A',plain(original));const a=h.session(original);h.state.current=a;
  h.c.docsDelete(a);const dialog=h.modals.at(-1).root,deleting=button(dialog,'Delete document').onclick();await until(()=>h.calls.some(call=>call.method==='desktop/document/delete'),'delete request');h.c.closeModal();gate.resolve({id:'A',deleted:true});await deleting;
  assert.ok(a.deleted,'A successfully deleted document remained an editable live session');assert.equal(h.state.sessions.has('A'),false);assert.notEqual(h.state.current,a);assert.equal(h.store.has('A'),false);
});

for(const kind of ['history','suggestion']){
  test('late '+kind+' read cannot replace a newer dialog in the same document',async()=>{
    const gate=defer(),h=harness((method)=>method===(kind==='history'?'desktop/document/history/read':'desktop/document/ai/read')?gate.promise:undefined),original=fixture('A','abc');h.store.set('A',plain(original));const a=h.session(original);h.state.current=a;
    const reading=kind==='history'?h.c.docsReviewVersion(a,1):h.c.docsReviewProposal(a,'proposal');await until(()=>h.calls.length>0,'owned read');h.c.docsModal('New comment being typed');const latest=h.modals.at(-1).root;latest.append(Object.assign(new Element('textarea'),{value:'UNSAVED COMMENT'}));
    gate.resolve(kind==='history'?{version:{title:'Earlier',content:'old'}}:{proposal:{proposalId:'proposal',status:'ready',baseRevision:1,selection:{from:1,to:4,quote:'abc'},proposedMarkdown:'new'}});await reading;
    assert.ok(h.modals.at(-1).root===latest,'Late '+kind+' read replaced a newer owned dialog');assert.equal(latest.isConnected,true);
  });
}

test('a comment mutation in flight preserves newer editor input and advances CAS before saving it',async()=>{
  const gate=defer(),h=harness(method=>method==='desktop/document/comments/create'?gate.promise:undefined),original=fixture('A','abc');h.store.set('A',plain(original));const a=h.session(original);h.state.current=a;
  const mutation=h.c.docsMutate(a,'desktop/document/comments/create',{body:'Note'});await until(()=>h.calls.some(call=>call.method==='desktop/document/comments/create'),'comment mutation');
  a.bridge.setJSON(body('abc plus live input'));const committed={...plain(original),revision:2,commentCount:1};h.store.set('A',plain(committed));gate.resolve({document:plain(committed),comment:{id:'comment'}});await mutation;
  assert.equal(textOf(a.bridge.getJSON()),'abc plus live input');assert.equal(a.doc.revision,2);await h.c.flushDocument();assert.equal(h.calls.at(-1).params.expectedRevision,2);assert.equal(textOf(h.store.get('A').body),'abc plus live input');assert.equal(h.store.get('A').commentCount,1);
});

test('AI apply refuses changed editor state while its final proposal check is in flight',async()=>{
  let reads=0;const gate=defer(),proposal={proposalId:'p',status:'ready',baseRevision:1,selection:{from:1,to:4,quote:'abc'},proposedMarkdown:'replacement'},h=harness(method=>method==='desktop/document/ai/read'?(++reads===1?{proposal:plain(proposal)}:gate.promise):undefined),original=fixture('A','abc');h.store.set('A',plain(original));const a=h.session(original);h.state.current=a;h.c.docsOpenSide=async()=>{};
  await h.c.docsReviewProposal(a,'p');const applying=button(h.modals.at(-1).root,'Apply suggestion').onclick();await until(()=>reads===2,'final AI read');a.bridge.setJSON(body('abc USER INPUT'));gate.resolve({proposal:plain(proposal)});await applying;
  assert.equal(a.bridge.appliedRanges.length,0);assert.equal(textOf(a.bridge.getJSON()),'abc USER INPUT');assert.ok(h.errors.some(error=>error.message.includes('changed')));assert.equal(textOf(h.store.get('A').body),'abc');
});

test('AI apply sends only the selected range and saves with its current revision',async()=>{
  const proposal={proposalId:'p',status:'ready',baseRevision:1,selection:{from:6,to:9,quote:'abc'},proposedMarkdown:'**new**'},h=harness(method=>method==='desktop/document/ai/read'?{proposal:plain(proposal)}:undefined),original=fixture('A','left abc right');h.store.set('A',plain(original));const a=h.session(original);h.state.current=a;h.c.docsOpenSide=async()=>{};
  await h.c.docsReviewProposal(a,'p');await button(h.modals.at(-1).root,'Apply suggestion').onclick();assert.equal(h.errors.length,0,h.errors.map(error=>error.message).join('; '));
  assert.deepEqual(a.bridge.appliedRanges.map(value=>value.range),[{from:6,to:9}]);assert.equal(textOf(h.store.get('A').body),'left new right');assert.equal(h.store.get('A').revision,2);assert.equal(h.c.docsDirty(a),false);
});

test('an older pending mode switch cannot cancel a newer document opening',async()=>{
  const settings=defer(),reading=defer(),h=harness((method,params)=>method==='desktop/settings/update'?settings.promise:method==='desktop/document/read'&&params.id==='B'?reading.promise:undefined,{productionNavigation:true});
  const original=fixture('A','A'),target=fixture('B','B');h.store.set('A',plain(original));h.store.set('B',plain(target));const a=h.session(original);h.state.current=a;
  const switching=h.c.switchMode('chat');await until(()=>h.calls.some(call=>call.method==='desktop/settings/update'),'mode persistence');
  const opening=h.c.openDocument('B');await until(()=>h.calls.some(call=>call.method==='desktop/document/read'&&call.params.id==='B'),'newer document read');
  settings.resolve({});await switching;reading.resolve({document:plain(target)});await opening;
  assert.equal(h.state.current?.doc.id,'B','Old mode switch called an internal New chat and invalidated the newer document navigation');
});

for(const kind of ['change request','export']){
  test('opening '+kind+' after a slow flush cannot replace a newer dialog',async()=>{
    const gate=defer(),h=harness(method=>method==='desktop/document/update'?gate.promise:undefined),original=fixture('A','abc');h.store.set('A',plain(original));const a=h.session(original);h.state.current=a;a.title.value='Edited title';
    const opening=kind==='change request'?h.c.docsAskChange(a,{from:1,to:4,empty:false,quote:'abc',revision:1}):h.c.docsExportDialog(a);
    await until(()=>h.calls.some(call=>call.method==='desktop/document/update'),'flush before '+kind);h.c.docsModal('New comment');const latest=h.modals.at(-1).root;
    gate.resolve({document:{...plain(original),title:'Edited title',revision:2}});await opening;assert.ok(h.modals.at(-1).root===latest,'Late '+kind+' setup replaced a newer dialog');assert.equal(latest.isConnected,true);
  });
}

test('Word download preserves binary bytes and MIME, pins its saved revision and reports export limits',async()=>{
  const zipBytes=Buffer.from([0x50,0x4b,3,4,0,255,128,1]);
  const h=harness(method=>method==='desktop/document/export'?{filename:'Word/中文.docx',mimeType:'application/vnd.openxmlformats-officedocument.wordprocessingml.document',dataBase64:zipBytes.toString('base64'),warnings:['Equations use LaTeX.']}:undefined);
  const saved=fixture('A','中文');h.store.set('A',saved);const session=h.session(saved);h.state.current=session;
  const blobs=[],links=[];h.c.atob=value=>Buffer.from(value,'base64').toString('binary');
  h.c.URL={createObjectURL(blob){blobs.push(blob);return 'blob:test';},revokeObjectURL(){}};
  const create=h.c.document.createElement;h.c.document.createElement=tag=>{const node=create(tag);if(tag==='a'){node.click=()=>links.push(node);}return node;};
  await h.c.docsExportDialog(session);const root=h.modals.at(-1).root;
  await button(root,'Download Word (.docx)').onclick();
  const request=h.calls.find(call=>call.method==='desktop/document/export');
  assert.deepEqual(request.params,{id:'A',format:'docx',revision:1});
  assert.equal(blobs.length,1);assert.deepEqual(Buffer.from(await blobs[0].arrayBuffer()),zipBytes);
  assert.equal(blobs[0].type,'application/vnd.openxmlformats-officedocument.wordprocessingml.document');
  assert.equal(links[0].download,'Word_中文.docx');assert.ok(root.children.some(node=>node.textContent==='Equations use LaTeX.'));
});

test('legacy NUL text falls back to exact source after the real vendor importer rejects it',async()=>{
  const text='Before\0after\r\nOriginal source';assert.throws(()=>vendor.importMarkdown(text));
  const h=harness(),legacy=fixture('nul','',{format:'markdown',body:null,content:text,sourceMarkdown:text});h.store.set('nul',plain(legacy));const a=h.session(legacy,{actual:true});h.state.current=a;
  assert.equal(a.bridge,null);assert.ok(a.source);assert.equal(a.source.value,text);await h.c.flushDocument();assert.equal(h.calls.filter(call=>call.method==='desktop/document/update').length,0);assert.equal(h.store.get('nul').content,text);
  a.source.value=text+' explicit edit';h.c.docsChanged(a);await h.c.flushDocument();assert.equal(h.store.get('nul').content,text+' explicit edit');assert.equal(h.store.get('nul').sourceMarkdown,text);assert.equal(h.store.get('nul').revision,2);
});

test('code documents bypass Markdown parsing and preserve arbitrary original source',async()=>{
  const h=harness(),text='let value = "\0";\r\n<script>literal</script>',legacy=fixture('code','',{kind:'code',format:'markdown',body:null,content:text,sourceMarkdown:text});h.store.set('code',plain(legacy));let imports=0;h.c.EditorBridge.importMarkdown=()=>{imports++;throw Error('Code must not be parsed as Markdown');};
  const a=h.session(legacy,{actual:true});h.state.current=a;assert.equal(imports,0);assert.equal(a.source.value,text);await h.c.flushDocument();assert.equal(h.calls.filter(call=>call.method==='desktop/document/update').length,0);assert.equal(h.store.get('code').content,text);
});

test('locked document actions reject comments but permit their owning restore or delete mutation',async()=>{
  for(const owner of ['desktop/document/restore','desktop/document/delete']){
    const h=harness((method,params)=>method===owner?(owner.endsWith('restore')?{document:fixture('A','restored',{revision:2})}:{id:'A',deleted:true}):undefined),original=fixture('A','abc');h.store.set('A',plain(original));const a=h.session(original);h.state.current=a;
    h.c.docsLock(a,true,owner);await assert.rejects(h.c.docsMutate(a,'desktop/document/comments/create',{body:'Must not interleave'}),/finish/);
    assert.equal(h.calls.length,0,'A locked comment reached the transport');await h.c.docsMutate(a,owner,owner.endsWith('restore')?{revision:1}:{});assert.equal(h.calls.length,1);assert.equal(h.calls[0].method,owner);assert.equal(h.calls[0].params.expectedRevision,1);h.c.docsLock(a,false);
  }
  const pending=defer(),h=harness(),original=fixture('A','abc');h.store.set('A',plain(original));const a=h.session(original);h.state.current=a;a.queue=pending.promise;
  const comment=h.c.docsMutate(a,'desktop/document/comments/create',{body:'Queued before restore lock'});h.c.docsLock(a,true,'desktop/document/restore');pending.resolve();await assert.rejects(comment,/finish/);assert.equal(h.calls.length,0,'A comment queued before the lock bypassed the queue-time owner check');
});
