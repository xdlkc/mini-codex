// Hosted search preferences and provenance. All provider activity comes from Rust.
const webSearchUi={threads:new Map(),pending:new Map(),writes:new Map(),drafts:null,draftVersions:new Map(),draftWrite:Promise.resolve(),draftRevision:0,draftSavedRevision:0,draftPending:false,policyWrites:new Set(),closing:null,capabilities:new Map(),capPending:new Map(),policy:new Map(),policyVersions:new Map(),items:new Map(),blocks:new WeakMap(),renderContext:null,panel:null,panelRequest:0,sequence:0};
const webSearchModes=['auto','required','cached','live','disabled'];
const webSearchLabels={auto:'Automatic',required:'Search web',cached:'Cached',live:'Live',disabled:'Off'};
function webSearchMode(value){return webSearchModes.includes(value)?value:'auto';}
function webSearchText(tag,text,className=''){const el=document.createElement(tag);el.textContent=text;el.className=className;return el;}
function webSearchButton(label,action,className='btn-pill'){const button=webSearchText('button',label,className);button.type='button';button.onclick=()=>Promise.resolve().then(action).catch(showError);return button;}
function webSearchScope(connectionId){
  if(!connectionId)return 'local';
  if(typeof remoteUi==='undefined')return null;
  return remoteUi.byConnection.get(connectionId)?.profileId||null;
}
function webSearchCapture(){
  const context=captureRpcContext(),scope=context.key==='local'?'local':(typeof remoteContext==='function'?remoteContext():context.key.split('@')[0]);
  return {context,scope:scope||'local',threadId:currentThread||null,draft:draftKey(),navigation:desktop.navigation,mode:desktop.mode,sandbox:currentSandbox};
}
function webSearchCurrent(snapshot){
  try{return snapshot.navigation===desktop.navigation&&snapshot.threadId===(currentThread||null)&&snapshot.context.key===captureRpcContext().key;}catch{return false;}
}
function webSearchThreadKey(scope,id){return JSON.stringify([scope,id]);}
function webSearchDraftKey(snapshot){return JSON.stringify([snapshot.scope,snapshot.draft]);}
function webSearchDrafts(){
  if(!webSearchUi.drafts){const saved=desktop.state?.settings?.preferences?.webSearchDrafts;webSearchUi.drafts=new Map(Object.entries(saved&&typeof saved==='object'&&!Array.isArray(saved)?saved:{}).filter(([,mode])=>webSearchModes.includes(mode)));}
  return webSearchUi.drafts;
}
function webSearchPersistDrafts(changed=true){
  if(changed)webSearchUi.draftRevision++;
  const work=webSearchUi.draftWrite.catch(()=>{}).then(async()=>{
    const revision=webSearchUi.draftRevision,drafts=Object.fromEntries(webSearchDrafts());
    const result=await rpcLocalCall('desktop/settings/update',{preferences:{webSearchDrafts:drafts}});
    webSearchUi.draftSavedRevision=Math.max(webSearchUi.draftSavedRevision,revision);
    if(desktop.state?.settings){desktop.state.settings.preferences={...desktop.state.settings.preferences,webSearchDrafts:drafts};}
    return result;
  }).finally(()=>{if(webSearchUi.draftWrite===work)webSearchUi.draftPending=false;});webSearchUi.draftWrite=work;webSearchUi.draftPending=true;return work;
}
async function webSearchFlushWrites(){
  for(;;){
    // A failed local write retains the latest draft. A later close retries that
    // snapshot with a new promise instead of waiting forever on the old failure.
    const draft=!webSearchUi.draftPending&&webSearchUi.draftSavedRevision<webSearchUi.draftRevision?webSearchPersistDrafts(false):webSearchUi.draftWrite,mustWait=webSearchUi.draftPending||webSearchUi.draftSavedRevision<webSearchUi.draftRevision;
    const results=await Promise.allSettled([mustWait?draft:Promise.resolve(),...webSearchUi.writes.values(),...webSearchUi.policyWrites]);
    const failure=results.find((result,index)=>result.status==='rejected'&&(index!==0||draft===webSearchUi.draftWrite));if(failure)throw failure.reason;
    if(draft===webSearchUi.draftWrite&&!webSearchUi.draftPending&&webSearchUi.draftSavedRevision>=webSearchUi.draftRevision&&!webSearchUi.writes.size&&!webSearchUi.policyWrites.size)return;
  }
}
function webSearchRemember(thread,scope){
  if(!thread?.id||!scope)return null;const key=webSearchThreadKey(scope,thread.id),old=webSearchUi.threads.get(key);
  const version=Number.isSafeInteger(thread.webSearchVersion)&&thread.webSearchVersion>=0?thread.webSearchVersion:null;
  if(old?.version!==null&&old?.version!==undefined&&(version===null||version<old.version))return old;
  const preferred=webSearchModes.includes(thread.webSearchMode)?thread.webSearchMode:(old?.mode||'auto');
  const record={mode:preferred,effective:webSearchModes.includes(thread.effectiveWebSearchMode)?thread.effectiveWebSearchMode:(old?.effective||null),version};
  webSearchUi.threads.set(key,record);return record;
}
function webSearchPreference(snapshot=webSearchCapture()){
  if(snapshot.threadId){const key=webSearchThreadKey(snapshot.scope,snapshot.threadId),pending=webSearchUi.pending.get(key),record=webSearchUi.threads.get(key);return {mode:pending?.mode||record?.mode||'auto',effective:record?.effective||null,version:record?.version??null};}
  return {mode:webSearchDrafts().get(webSearchDraftKey(snapshot))||'auto',effective:null,version:null};
}
const webSearchRequests=new WeakMap();
function webSearchRequest(){
  const snapshot=webSearchCapture(),preference=webSearchPreference(snapshot),request={webSearchMode:preference.mode};
  snapshot.draftVersion=webSearchUi.draftVersions.get(webSearchDraftKey(snapshot))||0;
  if(snapshot.threadId&&Number.isSafeInteger(preference.version))request.webSearchVersion=preference.version;
  webSearchRequests.set(request,snapshot);return request;
}
// Compatibility for older native bundles; typed requests are the authoritative API.
function webSearchEnabled(){return webSearchRequest().webSearchMode!=='disabled';}
function webSearchThreadStarted(thread,draft,request){
  const captured=typeof request==='object'&&request?webSearchRequests.get(request):null;
  const scope=captured?.scope||webSearchScope(thread?.connectionId);if(!thread?.id||!scope)return;
  const mode=webSearchMode(request?.webSearchMode);
  webSearchRemember({...thread,webSearchMode:thread.webSearchMode||mode},scope);
  const key=JSON.stringify([scope,draft]);
  if(webSearchDrafts().get(key)===mode&&captured&&(webSearchUi.draftVersions.get(key)||0)===captured.draftVersion){webSearchDrafts().delete(key);webSearchPersistDrafts().catch(showError);}
  queueMicrotask(()=>{try{const now=webSearchCapture();if(now.scope===scope&&now.threadId===thread.id)webSearchPaint();}catch{}});
}
function webSearchThreadOpened(thread){
  const snapshot=webSearchCapture(),scope=webSearchScope(thread?.connectionId);
  if(!thread?.id||scope!==snapshot.scope||thread.id!==snapshot.threadId)return;
  webSearchRemember(thread,scope);webSearchPaint();webSearchLoadCapabilities(snapshot).catch(()=>{});
  // History restoration uses the same item renderer. Apply the outer turn result after its items.
  for(const turn of thread.turns||[])for(const item of turn.items||[]){const record=webSearchUi.items.get(webSearchItemKey(scope,thread.id,item.id));if(record){record.turnId=turn.id;record.turnStatus=turn.status;record.completed=!['inProgress','running','active'].includes(turn.status);webSearchRefreshRecord(record);}}
}
async function webSearchSetMode(mode,snapshot=webSearchCapture()){
  if(!webSearchModes.includes(mode))throw new Error('Unknown search mode');
  if(!snapshot.threadId){const key=webSearchDraftKey(snapshot);webSearchDrafts().set(key,mode);webSearchUi.draftVersions.set(key,(webSearchUi.draftVersions.get(key)||0)+1);if(webSearchCurrent(snapshot))webSearchPaint();return webSearchPersistDrafts();}
  const key=webSearchThreadKey(snapshot.scope,snapshot.threadId),token=++webSearchUi.sequence;webSearchUi.pending.set(key,{token,mode});if(webSearchCurrent(snapshot))webSearchPaint();
  const work=(webSearchUi.writes.get(key)||Promise.resolve()).catch(()=>{}).then(()=>snapshot.context.call('thread/webSearch/set',{threadId:snapshot.threadId,mode}));webSearchUi.writes.set(key,work);
  try{const result=await work;webSearchRemember({id:snapshot.threadId,...result},snapshot.scope);return result;}
  finally{if(webSearchUi.pending.get(key)?.token===token)webSearchUi.pending.delete(key);if(webSearchUi.writes.get(key)===work)webSearchUi.writes.delete(key);if(webSearchCurrent(snapshot))webSearchPaint();}
}
function webSearchCapabilityKey(snapshot){return snapshot.context.key+'\0'+snapshot.sandbox;}
async function webSearchLoadCapabilities(snapshot=webSearchCapture(),refresh=false){
  const key=webSearchCapabilityKey(snapshot);if(!refresh&&webSearchUi.capabilities.has(key))return webSearchUi.capabilities.get(key);if(webSearchUi.capPending.has(key))return webSearchUi.capPending.get(key);
  const policyVersion=webSearchUi.policyVersions.get(snapshot.scope)||0;
  const work=snapshot.context.call('web/search/capabilities',{sandboxMode:snapshot.sandbox}).then(cap=>{if((webSearchUi.policyVersions.get(snapshot.scope)||0)===policyVersion){webSearchUi.capabilities.set(key,cap);webSearchUi.policy.set(snapshot.scope,cap.policy?.allowed!==false);}if(webSearchCurrent(snapshot))webSearchPaint();return cap;}).finally(()=>{if(webSearchUi.capPending.get(key)===work)webSearchUi.capPending.delete(key);});webSearchUi.capPending.set(key,work);return work;
}
function webSearchInvalidateCapabilities(scope,contextKey){
  webSearchUi.policy.delete(scope);webSearchUi.policyVersions.set(scope,(webSearchUi.policyVersions.get(scope)||0)+1);
  for(const map of [webSearchUi.capabilities,webSearchUi.capPending])for(const key of map.keys())if(key.startsWith(contextKey+'\0'))map.delete(key);
  try{const snapshot=webSearchCapture();if(snapshot.scope===scope)webSearchLoadCapabilities(snapshot,true).catch(()=>{});}catch{}
}
function webSearchAllowed(snapshot){
  if(webSearchUi.policy.has(snapshot.scope))return webSearchUi.policy.get(snapshot.scope);
  return snapshot.scope==='local'?desktop.state?.settings?.preferences?.webSearchAllowed!==false:true;
}
function webSearchPaint(){
  webSearchUpdateSourcesTab();
  const button=$('web-search-control');if(!button)return;let snapshot;try{snapshot=webSearchCapture();}catch{button.disabled=true;button.title='Connect to this SSH host to change search settings.';return;}
  const preference=webSearchPreference(snapshot),allowed=webSearchAllowed(snapshot);button.disabled=false;button.textContent='◎ '+webSearchLabels[preference.mode];button.dataset.mode=preference.mode;button.classList.toggle('selected',preference.mode==='required');
  const cap=webSearchUi.capabilities.get(webSearchCapabilityKey(snapshot)),effective=preference.mode==='auto'?(preference.effective||cap?.defaultModes?.[snapshot.mode]):preference.mode;
  button.title=!allowed?'Web search is disabled in Search settings. The selected preference is kept.':webSearchLabels[preference.mode]+(effective&&effective!==preference.mode?' · '+webSearchLabels[effective]:'')+' · Applies to the next message';button.setAttribute('aria-label','Web search: '+webSearchLabels[preference.mode]);
}
function webSearchMenu(event){
  event?.stopPropagation();const snapshot=webSearchCapture(),preference=webSearchPreference(snapshot);openFloatingMenuAt('web-search',event?.currentTarget||$('web-search-control'),'',true);const root=$('floating-menu');if(!root.classList.contains('open'))return;
  const note=webSearchText('p','Choose how the next message can search the web.','web-search-note');root.append(note);
  for(const mode of webSearchModes){const button=webSearchButton((preference.mode===mode?'✓ ':'')+webSearchLabels[mode],()=>{if(!webSearchCurrent(snapshot)||!root.contains(button)||activeFloatingMenuKey!=='web-search')return;closeFloatingMenu();return webSearchSetMode(mode,snapshot);},'menu-item');button.dataset.searchMode=mode;root.append(button);}
  root.append(webSearchButton('Search settings…',()=>{if(webSearchCurrent(snapshot)){closeFloatingMenu();return openSettingsView('search');}},'menu-item'));
  webSearchLoadCapabilities(snapshot).then(cap=>{if(webSearchCurrent(snapshot)&&root.classList.contains('open')&&activeFloatingMenuKey==='web-search'&&note.isConnected){note.textContent=!webSearchAllowed(snapshot)?'Web search is disabled in Search settings.':!cap.available?(cap.reason||'Connect an account on this host to use web search.'):'Automatic follows this chat’s mode and configuration. Search access depends on the model and account.';}}).catch(error=>{if(webSearchCurrent(snapshot)&&root.classList.contains('open')&&note.isConnected)note.textContent=error.message||'Search settings are unavailable.';});
}
async function webSearchSettings(){
  currentSettingsSection='search';document.querySelectorAll('.settings-nav-btn').forEach(button=>button.classList.toggle('active',button.dataset.sec==='search'));const box=$('settings-body');box.replaceChildren();const root=webSearchText('section','','web-search-settings');box.append(root);const snapshot=webSearchCapture();
  root.append(webSearchText('h1','Search','settings-h1'),webSearchText('p',snapshot.scope==='local'?'Search preferences for this device.':'Search preferences for the selected SSH host.','settings-sub'));
  const current=()=>root.isConnected&&$('settings-body')?.contains(root)&&currentSettingsSection==='search'&&webSearchCurrent(snapshot);
  const row=webSearchText('label','','web-search-setting-row'),input=document.createElement('input');input.type='checkbox';input.setAttribute('aria-label','Allow web search');input.checked=webSearchAllowed(snapshot);row.append(webSearchText('span','Allow web search'),input);root.append(row,webSearchText('p','When off, Automatic, Cached and Live will not search. A message that explicitly requires Search web will ask you to enable this setting.','web-search-note'));
  const status=webSearchText('p','Checking this host’s account…','web-search-note');root.append(status);
  input.onchange=async()=>{if(!current())return;const allowed=input.checked,previous=webSearchAllowed(snapshot);webSearchUi.policyVersions.set(snapshot.scope,(webSearchUi.policyVersions.get(snapshot.scope)||0)+1);input.disabled=true;let saving;try{saving=snapshot.context.call('desktop/settings/update',{preferences:{webSearchAllowed:allowed}});webSearchUi.policyWrites.add(saving);await saving;webSearchUi.policy.set(snapshot.scope,allowed);for(const key of webSearchUi.capabilities.keys())if(key.startsWith(snapshot.context.key+'\0'))webSearchUi.capabilities.delete(key);if(snapshot.scope==='local'&&desktop.state?.settings)desktop.state.settings.preferences.webSearchAllowed=allowed;webSearchPaint();}catch(error){if(current())input.checked=previous;showError(error);}finally{webSearchUi.policyWrites.delete(saving);if(current())input.disabled=false;}};
  try{const result=await webSearchLoadCapabilities(snapshot,true);if(!current())return;const cap=webSearchUi.capabilities.get(webSearchCapabilityKey(snapshot))||result;input.checked=webSearchAllowed(snapshot);status.textContent=cap.available?'An account is connected. Search permissions and model support are checked when a request runs.':cap.reason||'Connect an account on this host to use web search.';if(!cap.available)root.append(webSearchButton(snapshot.scope==='local'?'Connect account':'SSH connections',()=>{if(current())return openSettingsView(snapshot.scope==='local'?'account':'remote');}));}
  catch(error){if(current())status.textContent=error.message||'Search capabilities are unavailable.';}
}

function webSearchSafeUrl(value){
  if(typeof value!=='string'||!value||/[\u0000-\u0020\u007f]/.test(value))return null;
  try{const url=new URL(value);return ['http:','https:'].includes(url.protocol)&&!url.username&&!url.password?url.href:null;}catch{return null;}
}
function webSearchSource(value){return value&&typeof value.url==='string'?{url:value.url,title:typeof value.title==='string'?value.title:''}:null;}
function webSearchUniqueSources(values){const seen=new Set();return values.map(webSearchSource).filter(source=>source&&!seen.has(source.url)&&seen.add(source.url));}
function webSearchCitations(item){return webSearchUniqueSources([...(item.parts||[]).flatMap(part=>Array.isArray(part.citations)?part.citations:[]),...(Array.isArray(item.citations)?item.citations:[])].filter(citation=>citation?.type==='url_citation'));}
function webSearchItemKey(scope,threadId,itemId){return JSON.stringify([scope,threadId,itemId]);}
function webSearchItemContext(item){
  if(webSearchUi.renderContext)return webSearchUi.renderContext;
  const snapshot=webSearchCapture(),scope=item.connectionId?webSearchScope(item.connectionId):snapshot.scope;return {scope,threadId:snapshot.threadId,connectionId:item.connectionId||null,turnId:null};
}
function webSearchEventCurrent(context){
  if(!context?.scope)return false;let snapshot;try{snapshot=webSearchCapture();}catch{return false;}
  if(context.scope!==snapshot.scope||context.threadId!==snapshot.threadId)return false;
  if(context.connectionId){if(typeof remoteUi==='undefined')return false;const connection=remoteUi.byConnection.get(context.connectionId);return connection?.state==='connected'&&remoteUi.connections.get(context.scope)?.id===context.connectionId;}
  return snapshot.scope==='local';
}
function webSearchConnectionCurrent(context){
  if(!context.connectionId)return context.scope==='local';
  if(typeof remoteUi==='undefined'||!context.scope)return false;return remoteUi.byConnection.get(context.connectionId)?.state==='connected'&&remoteUi.connections.get(context.scope)?.id===context.connectionId;
}
function webSearchTrack(item,context){
  if(!context.scope||!context.threadId||!item?.id)return null;const key=webSearchItemKey(context.scope,context.threadId,item.id),old=webSearchUi.items.get(key);
  const record={...old,...context,key,item:{...old?.item,...item}};webSearchUi.items.set(key,record);return record;
}
function webSearchStatus(record){
  const status=String(record.item.status||'').replace(/[_-]/g,'').toLowerCase(),outer=String(record.turnStatus||'').toLowerCase();
  if(['failed','error'].includes(status))return {kind:'failed',label:'Search failed'};
  if(['interrupted','cancelled','canceled','stopped'].includes(status))return {kind:'stopped',label:'Search stopped'};
  if(status==='incomplete')return {kind:'incomplete',label:'Search incomplete'};
  if(['failed','interrupted','cancelled','canceled'].includes(outer))return {kind:outer==='failed'?'failed':'stopped',label:status==='completed'?'Search results · answer '+(outer==='failed'?'failed':'stopped'):'Search '+(outer==='failed'?'failed':'stopped')};
  if(status==='completed')return {kind:'completed',label:record.item.actionType==='open_page'?'Read page':record.item.actionType==='find_in_page'?'Searched page':'Searched the web'};
  return {kind:'running',label:'Searching the web…'};
}
function webSearchBlock(record,element){
  const block=element?.closest('.turn-block');if(!block)return;record.block=block;let records=webSearchUi.blocks.get(block);if(!records){records=new Set();webSearchUi.blocks.set(block,records);}records.add(record.key);
}
function webSearchSourcesFor(record){
  const records=record.block?webSearchUi.blocks.get(record.block):null;return webSearchUniqueSources([...(records||[])].map(key=>webSearchUi.items.get(key)).filter(item=>item?.item.type==='webSearch'&&item.scope===record.scope&&item.threadId===record.threadId).flatMap(item=>[...(item.item.sources||[]),...(item.item.url?[{url:item.item.url}]:[])]));
}
function webSearchAppendSource(root,source,index){
  const url=webSearchSafeUrl(source.url),card=webSearchText('div','','web-source-card'),label=source.title||source.url;
  const title=webSearchText(url?'a':'span',(index?index+'. ':'')+label,'web-source-title');if(url){title.href=url;title.target='_blank';title.rel='noopener noreferrer';}
  card.append(title,webSearchText('small',url?new URL(url).hostname:source.url,'web-source-host'));root.append(card);
}
function webSearchSourcesCurrent(panel=webSearchUi.panel){
  return Boolean(panel&&panel.root.isConnected&&$('drawer-content')?.contains(panel.root)&&drawerOpen&&currentDrawerTab==='sources'&&!$('inspector-drawer')?.classList.contains('closed')&&!document.body.classList.contains('in-settings')&&webSearchCurrent(panel.snapshot)&&webSearchEventCurrent(panel.record));
}
function webSearchLatestSourceRecord(){
  const records=[...webSearchUi.items.values()].reverse().filter(record=>record.block?.isConnected&&webSearchEventCurrent(record));
  return records.find(record=>record.item.type==='agentMessage'&&(webSearchCitations(record.item).length||webSearchSourcesFor(record).length))||records.find(record=>record.item.type==='webSearch');
}
function webSearchUpdateSourcesTab(){const tab=$('tab-btn-sources');if(tab)tab.hidden=!webSearchLatestSourceRecord();}
async function webSearchOpenSources(record){
  if(!webSearchEventCurrent(record)||!record.block?.isConnected||document.body.classList.contains('in-settings'))return;
  const request=++webSearchUi.panelRequest,snapshot=webSearchCapture(),focus=document.activeElement;let expectedTab=currentDrawerTab,expectedOpen=drawerOpen;
  const session=typeof documentsUi==='undefined'?null:documentsUi.current,docNavigation=typeof documentsUi==='undefined'?null:documentsUi.navigation;
  const current=()=>request===webSearchUi.panelRequest&&webSearchCurrent(snapshot)&&webSearchEventCurrent(record)&&record.block?.isConnected&&!document.body.classList.contains('in-settings')&&currentDrawerTab===expectedTab&&drawerOpen===expectedOpen;
  await flushDocument();if(!current()||(typeof documentsUi!=='undefined'&&(documentsUi.current!==session||documentsUi.navigation!==docNavigation)))return;
  if(session&&typeof docsLeave==='function'){
    const leaving=docsLeave(),navigation=desktop.navigation,documentNavigation=documentsUi.navigation;
    if(!await leaving||request!==webSearchUi.panelRequest||navigation!==desktop.navigation||documentNavigation!==documentsUi.navigation)return;
    snapshot.navigation=navigation;expectedTab=currentDrawerTab;expectedOpen=drawerOpen;if(!current())return;
  }
  if(!current())return;
  ++drawerContentRequest;drawerOpen=true;currentDrawerTab='sources';const drawer=$('inspector-drawer');drawer.classList.remove('closed','document-open');drawer.classList.add('sources-open');
  document.querySelectorAll('.drawer-tab').forEach(button=>button.classList.toggle('active',button.id==='tab-btn-sources'));
  const root=webSearchText('section','','web-sources-panel');root.id='web-search-sources';root.setAttribute('aria-label','Sources');$('drawer-content').replaceChildren(root);
  webSearchUi.panel={root,record,snapshot,focus,request};webSearchRenderPanel();webSearchUpdateSourcesTab();root.querySelector('h2')?.focus();
}
function webSearchRenderPanel(){
  const panel=webSearchUi.panel;if(!webSearchSourcesCurrent(panel))return;
  const root=panel.root,record=webSearchUi.items.get(panel.record.key)||panel.record,cited=webSearchCitations(record.item),consulted=webSearchSourcesFor(record),signature=JSON.stringify([cited,consulted]);if(panel.signature===signature)return;panel.signature=signature;
  const focused=root.contains(document.activeElement),href=focused?document.activeElement.getAttribute('href'):null;root.replaceChildren();const heading=webSearchText('h2','Sources');heading.tabIndex=-1;root.append(heading);
  if(cited.length){root.append(webSearchText('h3','Cited sources'));cited.forEach((source,index)=>webSearchAppendSource(root,source,index+1));}
  if(consulted.length){root.append(webSearchText('h3','Search sources'),webSearchText('p','Sources returned by search actions. Some may not be cited in the answer.','web-search-note'));consulted.forEach(source=>webSearchAppendSource(root,source));}
  if(!cited.length&&!consulted.length)root.append(webSearchText('p','No source details were returned.','web-search-note'));
  if(focused)(Array.from(root.querySelectorAll('a')).find(anchor=>anchor.getAttribute('href')===href)||heading).focus();
}
function webSearchRenderActivity(record){
  if(!webSearchEventCurrent(record))return;setViewMode(true);const block=record.block?.isConnected?record.block:ensureTurnBlock(false),list=block.querySelector('.turn-activity-list');block.querySelector('.turn-activity-wrap')?.classList.remove('empty');
  let row=itemEls.get(record.item.id);if(!row?.classList.contains('web-search-activity')){row=webSearchText('div','','web-search-activity');list.append(row);itemEls.set(record.item.id,row);}record.element=row;webSearchBlock(record,row);row.replaceChildren();const status=webSearchStatus(record);row.dataset.status=status.kind;
  const header=webSearchButton(status.label,()=>webSearchOpenSources(record),'web-search-activity-label');row.append(header);const queries=(record.item.queries||[]).filter(query=>typeof query==='string');if(queries.length)row.append(webSearchText('div',queries.join(' · '),'web-search-query'));if(record.item.pattern)row.append(webSearchText('div',record.item.pattern,'web-search-query'));
  const sources=webSearchUniqueSources([...(record.item.sources||[]),...(record.item.url?[{url:record.item.url}]:[])]);if(sources.length)row.append(webSearchButton(sources.length+' source'+(sources.length===1?'':'s'),()=>webSearchOpenSources(record),'web-search-sources-button'));scrollBottom();
  // A late search completion may add sources after the answer appeared.
  for(const key of webSearchUi.blocks.get(block)||[]){const message=webSearchUi.items.get(key);if(message?.item.type==='agentMessage')webSearchRenderCitations(message);}
  webSearchUpdateSourcesTab();
}
function webSearchRenderCitations(record){
  if(!webSearchEventCurrent(record))return;const element=itemEls.get(record.item.id);if(!element||record.item.phase==='commentary')return;webSearchBlock(record,element);element.querySelectorAll(':scope > .web-citations').forEach(node=>node.remove());
  const citations=webSearchCitations(record.item),sources=webSearchSourcesFor(record);if(!citations.length&&!sources.length)return;const refs=webSearchText('div','','web-citations');refs.setAttribute('aria-label','Answer sources');
  // Provider offsets are intentionally not used: their Unicode index unit is unspecified.
  citations.forEach((source,index)=>{const url=webSearchSafeUrl(source.url),ref=webSearchText(url?'a':'span',String(index+1),'web-citation');ref.title=source.title||source.url;ref.setAttribute('aria-label','Source '+(index+1)+': '+(source.title||source.url));if(url){ref.href=url;ref.target='_blank';ref.rel='noopener noreferrer';}else ref.classList.add('unsafe');refs.append(ref);});
  refs.append(webSearchButton('Sources',()=>webSearchOpenSources(record),'web-search-sources-button'));element.append(refs);
  webSearchUpdateSourcesTab();
}
function webSearchRefreshRecord(record){if(record.item.type==='webSearch')webSearchRenderActivity(record);else if(record.item.type==='agentMessage')webSearchRenderCitations(record);}

const webSearchBaseUpsert=upsertTimelineItem;
upsertTimelineItem=function(item){
  if(!item)return;const context=webSearchItemContext(item);if(!webSearchEventCurrent(context))return;
  if(item.type==='webSearch'){const record=webSearchTrack(item,context);if(record)webSearchRenderActivity(record);return;}
  const result=webSearchBaseUpsert(item);if(item.type==='agentMessage'){const record=webSearchTrack(item,context);if(record)webSearchRenderCitations(record);}return result;
};
const webSearchBaseNotification=handleServerNotification;
handleServerNotification=function(method,params={}){
  const scope=webSearchScope(params.connectionId),context={scope,threadId:params.threadId||params.thread?.id||null,connectionId:params.connectionId||null,turnId:params.turnId||params.turn?.id||null};
  if(params.connectionId&&!webSearchConnectionCurrent(context))return;
  if(method==='thread/webSearch/updated'){
    webSearchRemember({id:params.threadId,...params},scope);if(webSearchEventCurrent(context))webSearchPaint();return;
  }
  if(method==='account/updated'){
    webSearchInvalidateCapabilities(scope,params.connectionId?scope+'@'+params.connectionId:'local');
  }
  // Reject replaced connections before the shared raw-id renderer can touch the current chat.
  if(context.threadId===currentThread&&!webSearchEventCurrent(context))return;
  if(method==='item/agentMessage/delta'){const final=webSearchUi.items.get(webSearchItemKey(scope,context.threadId,params.itemId));if(final?.completed)return;}
  const previous=webSearchUi.renderContext;webSearchUi.renderContext=context;
  let result;try{result=webSearchBaseNotification(method,params);}finally{webSearchUi.renderContext=previous;}
  if((method==='item/started'||method==='item/completed')&&params.item&&['webSearch','agentMessage'].includes(params.item.type)){
    const record=webSearchTrack(params.item,context);if(record){if(method==='item/completed')record.completed=true;if(webSearchEventCurrent(context))webSearchRefreshRecord(record);}
  }
  if(method==='item/agentMessage/delta'){const record=webSearchUi.items.get(webSearchItemKey(scope,context.threadId,params.itemId));if(record)webSearchRenderCitations(record);}
  if(method==='turn/completed')for(const record of webSearchUi.items.values())if(record.scope===scope&&record.threadId===context.threadId&&(!context.turnId||record.turnId===context.turnId)){record.turnStatus=params.turn?.status||'completed';webSearchRefreshRecord(record);}
  webSearchRenderPanel();return result;
};
const webSearchBaseServerMessage=window.__onServerMessage;
window.__onServerMessage=function(message){
  // SSH account notices are consumed by the remote dispatcher before the normal handler.
  if(message?.method==='remote/notification'&&message.params?.method==='account/updated'){
    const connectionId=message.params.connectionId,scope=webSearchScope(connectionId);if(webSearchConnectionCurrent({connectionId,scope}))webSearchInvalidateCapabilities(scope,scope+'@'+connectionId);
  }
  return webSearchBaseServerMessage(message);
};
const webSearchBaseRestoreDraft=restoreDraft;
restoreDraft=function(...args){const result=webSearchBaseRestoreDraft(...args);webSearchPaint();try{webSearchLoadCapabilities().catch(()=>{});}catch{}return result;};
const webSearchBaseRenderSettings=renderSettingsSection;
renderSettingsSection=function(section,...args){return section==='search'?webSearchSettings():webSearchBaseRenderSettings(section,...args);};
const webSearchBaseCloseDrawer=closeDrawer;
closeDrawer=function(...args){
  const panel=currentDrawerTab==='sources'?webSearchUi.panel:null,active=document.activeElement,restoreFocus=!active||active===document.body||$('inspector-drawer')?.contains(active);++webSearchUi.panelRequest;webSearchUi.panel=null;$('inspector-drawer')?.classList.remove('sources-open');$('tab-btn-sources')?.classList.remove('active');
  const result=webSearchBaseCloseDrawer(...args),restore=()=>{if(panel&&restoreFocus&&webSearchCurrent(panel.snapshot)&&!drawerOpen){const focus=panel.focus?.isConnected?panel.focus:$('composer-input');focus?.focus();}};
  if(result&&typeof result.then==='function')return result.then(value=>{restore();return value;});restore();return result;
};
const webSearchBaseSwitchDrawer=switchDrawerTab;
switchDrawerTab=function(tab,...args){
  if(tab==='sources'){const record=webSearchLatestSourceRecord();return record?webSearchOpenSources(record):undefined;}
  ++webSearchUi.panelRequest;webSearchUi.panel=null;$('inspector-drawer')?.classList.remove('sources-open');$('tab-btn-sources')?.classList.remove('active');return webSearchBaseSwitchDrawer(tab,...args);
};
const webSearchBaseRenderDrawer=renderDrawerContent;
renderDrawerContent=function(...args){if(currentDrawerTab==='sources'){webSearchRenderPanel();return Promise.resolve();}return webSearchBaseRenderDrawer(...args);};
const webSearchBaseClearTimeline=clearTimeline;
clearTimeline=function(...args){++webSearchUi.panelRequest;webSearchUi.panel=null;if(currentDrawerTab==='sources'&&drawerOpen)webSearchBaseCloseDrawer();const result=webSearchBaseClearTimeline(...args);webSearchUpdateSourcesTab();return result;};
document.addEventListener('keydown',event=>{if(event.key==='Escape'&&!event.isComposing&&webSearchSourcesCurrent()&&!$('modal-backdrop')?.classList.contains('open')&&!$('floating-menu')?.classList.contains('open')){event.preventDefault();event.stopPropagation();closeDrawer();}},true);
async function webSearchSaveBeforeClose(){
  try{await webSearchFlushWrites();return true;}catch(error){document.body.inert=false;rpcSend({__window:'closeCancelled'});showError(new Error('Search preferences could not be saved. '+(error?.message||String(error))));return false;}
}
const webSearchBaseRequestClose=window.__requestClose;
window.__requestClose=function(...args){
  if(webSearchUi.closing)return webSearchUi.closing;
  const work=(async()=>{if(await webSearchSaveBeforeClose())return await webSearchBaseRequestClose(...args);})().finally(()=>{if(webSearchUi.closing===work)webSearchUi.closing=null;});webSearchUi.closing=work;return work;
};
// Transcript recovery can resume closing through its captured delegate. Check
// again there, since search preferences may have changed while recovery was open.
if(typeof finishVoiceClose==='function'){const baseFinishVoiceClose=finishVoiceClose;finishVoiceClose=async function(...args){if(await webSearchSaveBeforeClose())return baseFinishVoiceClose(...args);};}
const webSearchReady=desktopReady.then(()=>{
  if(!$('web-search-control')){const button=webSearchButton('◎ Automatic',webSearchMenu,'composer-chip-btn web-search-control');button.id='web-search-control';button.onclick=webSearchMenu;button.setAttribute('aria-haspopup','menu');document.querySelector('.composer-controls-left')?.append(button);}
  if(!document.querySelector('.settings-nav-btn[data-sec="search"]')){const button=webSearchButton('Search',()=>renderSettingsSection('search'),'settings-nav-btn');button.dataset.sec='search';document.querySelector('.settings-nav')?.append(button);}
  if(!$('tab-btn-sources')){const tab=webSearchButton('Sources',()=>switchDrawerTab('sources'),'drawer-tab web-sources-tab');tab.id='tab-btn-sources';tab.hidden=true;document.querySelector('.drawer-tabs')?.append(tab);}
  webSearchPaint();try{return webSearchLoadCapabilities().catch(()=>{});}catch{}
});
