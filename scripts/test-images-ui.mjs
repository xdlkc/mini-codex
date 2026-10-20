#!/usr/bin/env node
// Run the shipped image controller with isolated DOM/RPC stubs. No provider calls.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
const source=readFileSync(new URL('../src/gui_images.js',import.meta.url),'utf8');
const plain=value=>JSON.parse(JSON.stringify(value));
function deferred(){let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};}
async function settle(){for(let i=0;i<15;i++)await Promise.resolve();await new Promise(resolve=>setImmediate(resolve));}
const capabilities={available:true,defaultModel:'gpt-image-2.5-sunburst',models:['gpt-image-2.5-sunburst','gpt-image-1'],sizes:['auto','1024x1024','1536x1024','1024x1536'],qualities:['auto','low','medium','high','xhigh','max'],backgrounds:['auto','opaque','transparent'],outputFormats:['png','jpeg','webp'],modelOptions:{'gpt-image-2.5-sunburst':{qualities:['auto','low','medium','high','xhigh','max'],customSizes:true,transparentBackground:'preview',outputFormats:['png','jpeg','webp']},'gpt-image-1':{qualities:['auto','low','medium','high'],customSizes:false,transparentBackground:'supported',outputFormats:['png','jpeg','webp']}},customSizes:{models:['gpt-image-2.5-sunburst'],step:16,maxEdge:3840,minPixels:655360,maxPixels:8294400,maxAspectRatio:3},limits:{maxImages:4,maxInputImages:8,maxImageBytes:10485760,maxPromptCharacters:32000,maxActiveJobs:2},cancelNote:'Only local waiting stops. The provider may continue and charge.'};
const file={id:'att_reference',kind:'image',name:'Reference "quoted".png',mimeType:'image/png',size:20};
function job(overrides={}){return {id:'img_job',revision:1,status:'queued',kind:'generate',prompt:'A quiet lake',model:capabilities.defaultModel,size:'auto',quality:'auto',background:'auto',outputFormat:'png',n:1,imageIds:[],maskId:null,attachmentIds:[],attachments:[],createdAt:100,updatedAt:100,...overrides};}
function harness(options={}){
  const nodes=new Map(),calls=[],errors=[],events=[],ready=deferred();let document;
  function matches(node,selector){if(selector.startsWith('#'))return node.id===selector.slice(1);if(selector.startsWith('.'))return node.classList.contains(selector.slice(1));return node.tagName===selector.toUpperCase();}
  function make(tag='div'){
    const classes=new Set();let id='',content='',value='';
    const node={tagName:tag.toUpperCase(),children:[],parentElement:null,attributes:{},style:{},dataset:{},hidden:false,disabled:false,checked:false,
      classList:{add(...values){values.forEach(v=>classes.add(v));},remove(...values){values.forEach(v=>classes.delete(v));},contains:v=>classes.has(v),toggle(v,force){const added=force??!classes.has(v);if(added)classes.add(v);else classes.delete(v);return added;}},
      append(...children){for(const child of children){if(child.parentElement)child.remove();child.parentElement=this;this.children.push(child);if(this.tagName==='SELECT'&&this.children.length===1)value=child.value;}},
      replaceChildren(...children){for(const child of this.children)child.parentElement=null;this.children=[];content='';this.append(...children);},
      remove(){if(this.parentElement)this.parentElement.children=this.parentElement.children.filter(child=>child!==this);this.parentElement=null;},
      contains(other){for(let current=other;current;current=current.parentElement)if(current===this)return true;return false;},
      setAttribute(name,val){this.attributes[name]=String(val);if(name==='id')this.id=val;if(name==='class')this.className=val;},getAttribute(name){return this.attributes[name]??null;},
      querySelectorAll(selector){const result=[];function walk(parent){for(const child of parent.children){if(selector.split(',').some(part=>matches(child,part.trim())))result.push(child);walk(child);}}walk(this);return result;},
      querySelector(selector){return this.querySelectorAll(selector)[0]||null;},focus(){document.activeElement=this;},click(){this.clicks=(this.clicks||0)+1;return this.onclick?.({target:this,currentTarget:this});},addEventListener(){},
    };
    Object.defineProperty(node,'id',{get:()=>id,set:v=>{id=String(v);nodes.set(id,node);}});
    Object.defineProperty(node,'className',{get:()=>[...classes].join(' '),set:v=>{classes.clear();String(v).split(/\s+/).filter(Boolean).forEach(c=>classes.add(c));}});
    Object.defineProperty(node,'value',{get:()=>value,set:v=>{value=String(v);}});
    Object.defineProperty(node,'options',{get:()=>node.children.filter(child=>child.tagName==='OPTION')});
    Object.defineProperty(node,'isConnected',{get:()=>document?.body===node||Boolean(document?.body.contains(node))});
    Object.defineProperty(node,'textContent',{get:()=>content+node.children.map(c=>c.textContent).join(''),set:v=>{node.replaceChildren();content=String(v);}});
    Object.defineProperty(node,'innerHTML',{get:()=>content,set:html=>{node.replaceChildren();const stack=[node];for(const token of String(html).match(/<[^>]+>|[^<]+/g)||[]){if(token.startsWith('</')){const close=token.slice(2,-1).trim().toUpperCase();while(stack.length>1){if(stack.pop().tagName===close)break;}continue;}if(token.startsWith('<')){const match=token.match(/^<([\w-]+)([^>]*)>/);if(!match)continue;const child=make(match[1]);for(const attr of match[2].matchAll(/([\w-]+)(?:="([^"]*)"|='([^']*)')?/g)){const name=attr[1],v=attr[2]??attr[3]??'';child.setAttribute(name,v);if(name==='value')child.value=v;if(name==='hidden')child.hidden=true;if(name==='type')child.type=v;}stack.at(-1).append(child);if(!['INPUT','IMG','BR','HR'].includes(child.tagName))stack.push(child);}else stack.at(-1).append(Object.assign(make('#text'),{textContent:token}));}}});return node;
  }
  document={body:make('body'),activeElement:null,createElement:make,getElementById:id=>{const n=nodes.get(id);return n?.isConnected?n:null;},querySelector:selector=>document.body.querySelector(selector),querySelectorAll:selector=>document.body.querySelectorAll(selector)};
  for(const id of ['modal-title','modal-body','modal-backdrop','settings-body','home-hero','conversation-wrap','composer-host','work-library','topbar-title','floating-menu']){const n=make();n.id=id;document.body.append(n);}
  for(const name of ['settings-nav','library-header']){const n=make();n.className=name;document.body.append(n);}
  const desktop={navigation:0,library:false,mode:'chat'};const h={calls,errors,events,nodes,desktop,respond:options.respond};
  const context=vm.createContext({document,desktop,desktopReady:ready.promise,console,Date,Map,Set,Promise,JSON,Number,String,Array,Math,queueMicrotask,
    $:id=>document.getElementById(id),currentSettingsSection:'general',
    rpcCall:()=>{throw new Error('Images must not use the active remote transport');},rpcLocalCall:async(method,params={})=>{calls.push({method,params:plain(params)});const result=h.respond?.(method,params);if(result!==undefined)return result;if(method==='images/capabilities')return plain(options.capabilities||capabilities);if(method==='files/list')return {data:plain(options.files||[file])};if(method==='images/job/list')return {data:plain(options.jobs||[])};if(method==='images/job/read')return {job:job()};if(method==='images/generate'||method==='images/edit')return {job:job({kind:method.endsWith('/edit')?'edit':'generate',...params})};if(method==='files/read')return {attachment:file,content:{dataBase64:'aW1hZ2U='}};if(method==='images/job/cancel')return {job:job({revision:3,status:'cancelled',cancelNote:capabilities.cancelNote})};throw new Error('Unexpected RPC '+method);},
    openCustomModal(title,html){nodes.get('modal-title').textContent=title;nodes.get('modal-body').innerHTML=html;nodes.get('modal-backdrop').classList.add('open');},closeModal(){nodes.get('modal-backdrop').classList.remove('open');},
    openSettingsView:async section=>{desktop.navigation++;document.body.classList.add('in-settings');await context.renderSettingsSection(section);},renderSettingsSection:async section=>{context.currentSettingsSection=section;nodes.get('settings-body').textContent=section;},
    showAttachmentMenu(){nodes.get('floating-menu').replaceChildren();},menuButton:(label,fn)=>{const n=make('button');n.textContent=label;n.onclick=fn;return n;},
    showLibrary:async()=>{const navigation=++desktop.navigation;await h.libraryWait;if(navigation!==desktop.navigation)return;desktop.library=true;nodes.get('work-library').innerHTML='<div class="library-header">Documents</div>';},showFilesLibrary:async()=>{const navigation=++desktop.navigation;await h.libraryWait;if(navigation!==desktop.navigation)return;desktop.library=true;nodes.get('work-library').innerHTML='<div class="library-header">Files</div>';},newThread:async()=>{desktop.navigation++;desktop.library=false;nodes.get('work-library').innerHTML='';events.push('newThread');},openThread:async()=>{desktop.navigation++;desktop.library=false;},switchMode:async()=>{desktop.navigation++;},openScheduledView:async()=>{desktop.navigation++;desktop.library=false;},
    saveDraft:()=>events.push('saveDraft'),flushDocument:async()=>events.push('flushDocument'),closeSettingsView:()=>document.body.classList.remove('in-settings'),closeScheduledView(){},closeFloatingMenu(){},closeDrawer(){},syncShellNavigation(){},handleServerNotification:(method)=>events.push(method),
    showError:error=>errors.push(error),openAttachment:async(id,options)=>events.push({action:'open',id,options:plain(options)}),downloadAttachment:async(id,options)=>events.push({action:'download',id,options:plain(options)}),fileBase64:async()=> 'aW1hZ2U=',
  });context.window=context;vm.runInContext(source+'\nglobalThis.imageTest={state:imagesUi};',context);h.context=context;h.state=context.imageTest.state;h.node=id=>document.getElementById(id);h.document=document;h.ready=async()=>{ready.resolve();await settle();};return h;
}
function fill(h,values={}){for(const [key,value] of Object.entries({prompt:'A quiet lake',...values}))h.node('images-'+key).value=value;}

test('production request validation follows model options and enforces edit/custom/transparency limits',()=>{
  const h=harness();const draft={kind:'generate',prompt:'A lake',model:capabilities.defaultModel,size:'custom',width:1280,height:1024,quality:'max',background:'transparent',outputFormat:'png',n:'2'};
  const request=plain(h.context.imagesRequest(draft,capabilities));assert.equal(request.method,'images/generate');assert.equal(request.params.size,'1280x1024');assert.equal(request.params.n,2);assert.ok(!('imageIds' in request.params));
  assert.throws(()=>h.context.imagesRequest({...draft,model:'gpt-image-1'},capabilities),/quality/);
  assert.throws(()=>h.context.imagesRequest({...draft,model:'gpt-image-1',quality:'high'},capabilities),/Custom size/);
  assert.throws(()=>h.context.imagesRequest({...draft,width:1279},capabilities),/multiples of 16/);
  assert.throws(()=>h.context.imagesRequest({...draft,width:3840,height:256},capabilities),/aspect ratio/);
  assert.throws(()=>h.context.imagesRequest({...draft,outputFormat:'jpeg'},capabilities),/Transparent/);
  assert.throws(()=>h.context.imagesRequest({...draft,kind:'edit',imageIds:[]},capabilities),/reference images/);
  assert.throws(()=>h.context.imagesRequest({...draft,n:5},capabilities),/image count/);
  const edit=plain(h.context.imagesRequest({...draft,kind:'edit',imageIds:[file.id,file.id],maskId:'att_mask'},capabilities));assert.equal(edit.method,'images/edit');assert.deepEqual(edit.params.imageIds,[file.id]);assert.equal(edit.params.maskId,'att_mask');
});

test('Create image submits one real request; terminal notification wins over late queued response',async()=>{
  const submitted=deferred();const h=harness({respond:method=>method==='images/generate'?submitted.promise:undefined});await h.ready();await h.context.openImageEditor();fill(h);const dialog=h.state.dialog;
  const pending=h.context.imagesSubmit(dialog);await settle();await h.context.imagesSubmit(dialog);assert.equal(h.calls.filter(c=>c.method==='images/generate').length,1);
  h.context.handleServerNotification('images/job/updated',{job:job({revision:3,status:'completed'})});submitted.resolve({job:job()});await pending;
  assert.equal(h.state.jobs.get('img_job').status,'completed');assert.equal(h.state.jobs.get('img_job').revision,3);assert.match(h.node('images-job-detail').textContent,/Completed/);assert.deepEqual(plain(h.calls.find(c=>c.method==='images/generate').params),{prompt:'A quiet lake',model:capabilities.defaultModel,size:'auto',quality:'auto',background:'auto',outputFormat:'png',n:1});
});

test('late create response preserves a new navigation and does not replace a different modal',async()=>{
  for(const navigate of [false,true]){const response=deferred();const h=harness({respond:method=>method==='images/generate'?response.promise:undefined});await h.ready();await h.context.openImageEditor();fill(h);const pending=h.context.imagesSubmit(h.state.dialog);await settle();
    if(navigate)await h.context.newThread();else h.context.openCustomModal('Other','<p id="other-modal">Other content</p>');response.resolve({job:job()});await pending;
    assert.equal(h.state.jobs.get('img_job').status,'queued');assert.equal(h.node('images-job-detail'),null);if(navigate)assert.ok(h.events.includes('newThread'));else assert.equal(h.node('other-modal').textContent,'Other content');}
});

test('missing API key disables provider submission and offers real Account settings',async()=>{
  const h=harness({capabilities:{...capabilities,available:false,reason:'Connect an OpenAI API key.'}});await h.ready();await h.context.openImageEditor();assert.equal(h.node('images-submit').disabled,true);fill(h);await h.context.imagesSubmit(h.state.dialog);
  assert.equal(h.calls.filter(c=>['images/generate','images/edit'].includes(c.method)).length,0);assert.match(h.node('images-form-error').textContent,/API key/);
  await h.node('images-connect-actions').children[0].click();assert.equal(h.context.currentSettingsSection,'account');
});

test('edit uses selected stored images/mask and model switch removes unsupported quality and custom size',async()=>{
  const h=harness();await h.ready();await h.context.openImageEditor({kind:'edit',prompt:'Replace the sky',imageIds:[file.id],maskId:file.id,quality:'max',size:'1280x1024'});
  assert.equal(h.node('images-size').value,'custom');assert.equal(h.node('images-quality').value,'max');await h.context.imagesSubmit(h.state.dialog);const call=h.calls.find(c=>c.method==='images/edit');assert.deepEqual(call.params.imageIds,[file.id]);assert.equal(call.params.maskId,file.id);assert.equal(call.params.size,'1280x1024');
  await h.context.openImageEditor({quality:'max',size:'1280x1024'});h.node('images-model').value='gpt-image-1';h.node('images-model').onchange();assert.equal(h.node('images-quality').value,'auto');assert.equal(h.node('images-size').value,'auto');assert.ok(!h.node('images-quality').options.some(option=>option.value==='max'));
});

test('failed Retry opens a new prefilled request without automatic provider call; cancel confirms local-only semantics',async()=>{
  const h=harness();await h.ready();await h.context.openImageJob('img_job',job({status:'failed',revision:2,error:'Provider rejected this request'}));assert.equal(h.calls.filter(c=>c.method==='images/generate').length,0);
  const retry=h.node('images-job-detail').querySelectorAll('button').find(b=>b.textContent==='Retry as new request');await retry.click();assert.equal(h.node('images-prompt').value,'A quiet lake');assert.match(h.node('images-editor').textContent,/new API request.*charges/);assert.equal(h.calls.filter(c=>c.method==='images/generate').length,0);
  await h.context.imagesSubmit(h.state.dialog);assert.equal(h.calls.filter(c=>c.method==='images/generate').length,1);
  h.context.imagesCancelDialog(job());assert.match(h.node('images-cancel-note').textContent,/local waiting|waiting on this device/i);assert.match(h.node('images-cancel-note').textContent,/charge/);assert.equal(h.calls.filter(c=>c.method==='images/job/cancel').length,0);
  await h.node('images-cancel-actions').children[1].click();assert.equal(h.calls.filter(c=>c.method==='images/job/cancel').length,1);assert.match(h.node('images-job-detail').textContent,/Local waiting stopped/);
});

test('Library restores persisted jobs, ignores stale load after navigation, filters and pages actual saved images',async()=>{
  const h=harness({jobs:[job({status:'failed',revision:2}),job({id:'img_second',prompt:'Other image',status:'completed',revision:3})],files:Array.from({length:26},(_,i)=>({...file,id:'att_'+i,name:'Image '+i}))});await h.ready();await h.context.showImagesLibrary();await settle();
  assert.equal(h.node('images-jobs').children.length,2);assert.equal(h.node('images-saved').children.length,24);assert.equal(h.node('images-saved-pages').children.length,3);await h.node('images-saved-pages').children[2].click();assert.equal(h.node('images-saved').children.length,2);
  h.node('images-job-filter').value='failed';h.node('images-job-filter').onchange();assert.equal(h.node('images-jobs').children.length,1);assert.match(h.node('images-jobs').textContent,/A quiet lake/);
  const response=deferred();h.respond=method=>method==='images/job/list'?response.promise:undefined;const pending=h.context.showImagesLibrary();await settle();await h.context.newThread();response.resolve({data:[job({id:'late'})]});await pending;assert.equal(h.state.jobs.has('late'),false);assert.equal(h.node('images-jobs'),null);
});

test('preview RPCs are bounded, late bytes cannot mutate detached image or inject a non-image URL',async()=>{
  const requests=[];const h=harness({files:Array.from({length:8},(_,i)=>({...file,id:'att_'+i})),respond:(method,params)=>{if(method==='files/read'){const response=deferred();requests.push({response,params});return response.promise;}}});await h.ready();await h.context.showImagesLibrary();await settle();assert.equal(requests.length,3);assert.equal(h.state.previewActive,3);
  const first=h.node('images-saved').children[0].querySelector('img');await h.context.newThread();for(const request of requests)request.response.resolve({attachment:{...file,mimeType:'text/html'},content:{dataBase64:'aW1hZ2U='}});await settle();assert.equal(first.src,undefined);assert.equal(h.state.previewActive,0);assert.equal(requests.length,3);
});

test('late capabilities and notifications preserve replacement settings/modal surfaces',async()=>{
  const response=deferred();const h=harness({respond:method=>method==='images/capabilities'?response.promise:undefined});await h.ready();const opening=h.context.openImageEditor();await settle();h.context.openCustomModal('Other','<p id="other">Keep me</p>');response.resolve(capabilities);await opening;assert.equal(h.node('images-form'),null);assert.equal(h.node('other').textContent,'Keep me');
  h.context.handleServerNotification('images/job/updated',{job:job({revision:8,status:'completed'})});h.context.handleServerNotification('images/job/updated',{job:job({revision:2,status:'running'})});assert.equal(h.state.jobs.get('img_job').status,'completed');assert.equal(h.node('other').textContent,'Keep me');
  const settings=deferred();h.respond=method=>method==='images/capabilities'?settings.promise:undefined;const pending=h.context.openSettingsView('images');await settle();await h.context.renderSettingsSection('account');settings.resolve(capabilities);await pending;assert.equal(h.node('settings-body').textContent,'account');
});

 test('all image files use local transport and result helpers cannot attach to a remote thread',async()=>{
  const h=harness();await h.ready();await h.context.showImagesLibrary();await settle();const card=h.node('images-saved').children[0];const buttons=card.querySelectorAll('button');await buttons[0].click();await buttons[1].click();
  assert.deepEqual(h.events.filter(event=>event?.action),[{action:'open',id:file.id,options:{local:true,allowAttach:false}},{action:'download',id:file.id,options:{local:true}}]);assert.equal(h.errors.length,0);
 });

 test('a completed live job adds saved results, while deleted historic attachments stay absent',async()=>{
  const deleted={...file,id:'att_deleted'},newFile={...file,id:'att_new',name:'New result.png'};const h=harness({files:[],jobs:[job({status:'completed',revision:3,attachments:[deleted]})]});await h.ready();await h.context.showImagesLibrary();assert.equal(h.node('images-saved').querySelectorAll('img').length,0);
  h.context.handleServerNotification('images/job/updated',{job:job({id:'img_new',revision:4,status:'completed',attachments:[newFile]})});await settle();assert.equal(h.node('images-saved').querySelectorAll('img').length,1);assert.match(h.node('images-saved').textContent,/New result/);
 });

 test('Library Images entry survives base navigation increments and never appends after a later navigation',async()=>{
  const h=harness();await h.ready();for(const name of ['showLibrary','showFilesLibrary']){const before=h.desktop.navigation;await h.context[name]();assert.equal(h.desktop.navigation,before+1);const header=h.node('work-library').querySelector('.library-header');assert.equal(header.querySelectorAll('button').filter(button=>button.textContent==='Images').length,1);}
  const response=deferred();h.libraryWait=response.promise;const pending=h.context.showLibrary();await settle();await h.context.newThread();response.resolve();await pending;assert.equal(h.node('work-library').querySelector('.library-header'),null);
 });
