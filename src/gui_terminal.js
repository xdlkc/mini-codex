// Persistent, user-operated PTYs for the Codex terminal panel.
const codexTerminalUi={sessions:new Map(),selected:new Map(),closed:new Set(),restored:new Set(),view:0,root:null,context:null,open:false,dock:null,navigationIntent:0,pendingAuxiliary:false};
const codexTerminalText=(en,zh)=>typeof codexText==='function'?codexText(en,zh):en;
const codexTerminalHost=()=>typeof remoteContext==='function'?remoteContext()||'local':'local';
const codexTerminalId=(host,id)=>JSON.stringify([host,id]);
function codexTerminalCapture(){const context=workspaceCapture();return {...context,host:codexTerminalHost(),cwd:currentCwd,threadId:currentThread};}
function codexTerminalKey(context){return JSON.stringify([context.host,context.key,codexEnvPath(context.cwd),context.threadId]);}
function codexTerminalCurrent(context){return desktop.mode==='codex'&&workspaceCurrent(context)&&context.host===codexTerminalHost()&&codexEnvPath(context.cwd)===codexEnvPath(currentCwd)&&context.threadId===currentThread;}
function codexTerminalChatVisible(){return !codexTerminalUi.pendingAuxiliary&&desktop.mode==='codex'&&!desktop.library&&!document.body.classList.contains('in-settings')&&!document.body.classList.contains('in-scheduled')&&!document.body.classList.contains('in-document');}
function codexTerminalPanelCurrent(context,view=codexTerminalUi.view){return codexTerminalUi.open&&codexTerminalChatVisible()&&view===codexTerminalUi.view&&codexTerminalUi.root?.isConnected&&codexTerminalCurrent(context);}
function codexTerminalDock(){
  if(codexTerminalUi.dock?.isConnected)return codexTerminalUi.dock;
  const dock=document.createElement('section');dock.id='codex-terminal-dock';dock.hidden=true;dock.setAttribute('aria-label',codexTerminalText('Session terminal','会话终端'));
  const header=document.createElement('div');header.className='codex-terminal-dock-header';const label=document.createElement('strong');label.textContent=codexTerminalText('Terminal','终端');
  const close=document.createElement('button');close.type='button';close.className='icon-btn';close.textContent='×';close.setAttribute('aria-label',codexTerminalText('Close terminal panel','关闭终端面板'));close.onclick=codexTerminalClose;
  header.append(label,close);const content=document.createElement('div');content.className='codex-terminal-dock-content';dock.append(header,content);$('main-col').append(dock);codexTerminalUi.dock=dock;return dock;
}
function codexTerminalClose(){codexTerminalUi.open=false;codexTerminalUi.view++;if(codexTerminalUi.dock)codexTerminalUi.dock.hidden=true;$('composer-input')?.focus();}
function codexTerminalSyncDock(){
  const visible=codexTerminalUi.open&&codexTerminalChatVisible();if(codexTerminalUi.dock)codexTerminalUi.dock.hidden=!visible;
  if(visible&&(!codexTerminalUi.context||!codexTerminalCurrent(codexTerminalUi.context)))codexTerminalRender().catch(showError);
}
function codexTerminalToggle(){if(codexTerminalUi.open&&codexTerminalChatVisible())return codexTerminalClose();return codexTerminalRender();}
async function codexTerminalRestoreHost(){
  const context=codexTerminalCapture(),key=JSON.stringify([context.host,context.key]);if(codexTerminalUi.restored.has(key))return;codexTerminalUi.restored.add(key);
  try{const result=await context.call('terminal/list');for(const state of result.data||[])codexTerminalRemember(state,{...context,cwd:state.cwd,threadId:state.threadId});}
  catch{codexTerminalUi.restored.delete(key);}
}
function codexTerminalTheme(){const dark=document.documentElement.classList.contains('dark');return {background:dark?'#171717':'#ffffff',foreground:dark?'#eeeeee':'#171717',cursor:dark?'#ffffff':'#171717',selectionBackground:dark?'#ffffff40':'#00000030',selectionInactiveBackground:dark?'#ffffff26':'#00000020'};}
function codexTerminalAcknowledge(session,sequence){
  session.acknowledged=Math.max(session.acknowledged||0,sequence||0);if(session.ackTimer)return;
  session.ackTimer=setTimeout(()=>{session.ackTimer=null;session.context.call('terminal/ack',{id:session.state.id,sequence:session.acknowledged}).catch(()=>{});},20);
}
function codexTerminalReplay(session,output,sequence){
  session.replaying=true;session.term.reset();session.term.write(output||'',()=>{session.replaying=false;codexTerminalAcknowledge(session,sequence);});
}
function codexTerminalRemember(state,context){
  if(!state?.id)return null;const key=codexTerminalId(context.host,state.id);if(codexTerminalUi.closed.has(key))return null;
  let session=codexTerminalUi.sessions.get(key);
  if(session&&codexTerminalCurrent(context))session.context=context;
  if(!session){session={state,context,key,sequence:state.sequence||0};codexTerminalUi.sessions.set(key,session);}
  else if((state.sequence||0)>=session.sequence){
    if(session.state.status!=='running'&&state.status==='running')return session;
    if((state.sequence||0)>session.sequence&&session.term)codexTerminalReplay(session,state.output,state.sequence);
    session.state=state;session.sequence=state.sequence||0;
  }
  if(!session.term)codexTerminalAcknowledge(session,state.sequence);
  if(codexTerminalCurrent(session.context))codexTerminalPaintTabs();return session;
}
function codexTerminalDispose(session){if(session.ackTimer)clearTimeout(session.ackTimer);session.observer?.disconnect();session.term?.dispose();session.container?.remove();session.term=null;session.container=null;}
function codexTerminalForget(session){
  codexTerminalUi.closed.add(session.key);codexTerminalDispose(session);codexTerminalUi.sessions.delete(session.key);
  if(!codexTerminalCurrent(session.context))return;codexTerminalPaintTabs();const next=codexTerminalUi.sessions.get(codexTerminalUi.selected.get(codexTerminalKey(session.context)));if(next)codexTerminalShow(next);else codexTerminalUi.root?.querySelector('.codex-terminal-viewport')?.replaceChildren();
}
function codexTerminalShellTitle(title,cwd){
  const clean=value=>String(value||'').replace(/[\u0000-\u001f\u007f]/g,'').trim(),value=clean(title),directory=clean(cwd),name=directory.split(/[\\/]/).filter(Boolean).at(-1)||'';
  if(!value||value===directory||value===name)return null;
  for(const separator of [': ',' — ',' - ',' | '])for(const prefix of [directory,name])if(prefix&&value.startsWith(prefix+separator))return value.slice(prefix.length+separator.length).trim()||null;
  return value;
}
function codexTerminalTabTitle(session){
  const state=session.state,fixed=Object.hasOwn(state,'fixedTitle')?state.fixedTitle:state.title!=='Terminal'?state.title:null;
  return fixed||codexTerminalShellTitle(session.rawShellTitle,state.cwd)||state.title||codexTerminalText('Terminal','终端');
}
function codexTerminalPaintTabs(){
  const root=codexTerminalUi.root,context=codexTerminalUi.context;if(!root?.isConnected||!context||!codexTerminalCurrent(context))return;
  const tabs=root.querySelector('[role=tablist]');tabs.replaceChildren();
  const key=codexTerminalKey(context),sessions=[...codexTerminalUi.sessions.values()].filter(s=>codexTerminalKey(s.context)===key);
  let active=codexTerminalUi.selected.get(key);if(!sessions.some(s=>s.key===active)){active=sessions[0]?.key;codexTerminalUi.selected.set(key,active);}
  for(const session of sessions){
    const row=document.createElement('div');row.className='codex-terminal-tab';const tab=document.createElement('button');tab.type='button';tab.setAttribute('role','tab');tab.setAttribute('aria-selected',String(session.key===active));tab.tabIndex=session.key===active?0:-1;tab.textContent=codexTerminalTabTitle(session);tab.title=tab.textContent;tab.onclick=()=>{codexTerminalUi.selected.set(key,session.key);codexTerminalPaintTabs();codexTerminalShow(session);};
    tab.onkeydown=event=>{if(!['ArrowLeft','ArrowRight','Home','End'].includes(event.key))return;event.preventDefault();const index=sessions.indexOf(session),next=event.key==='Home'?0:event.key==='End'?sessions.length-1:(index+(event.key==='ArrowRight'?1:sessions.length-1))%sessions.length;codexTerminalUi.selected.set(key,sessions[next].key);codexTerminalPaintTabs();codexTerminalShow(sessions[next]);tabs.querySelectorAll('[role=tab]')[next]?.focus();};
    const close=document.createElement('button');close.type='button';close.className='icon-btn';close.textContent='×';close.setAttribute('aria-label',codexTerminalText('Close terminal','关闭终端')+' '+tab.textContent);close.onclick=async()=>{if(!codexTerminalCurrent(context))return;close.disabled=true;try{await context.call('terminal/close',{id:session.state.id});codexTerminalForget(session);}catch(error){showError(error);}finally{close.disabled=false;}};
    row.append(tab,close);tabs.append(row);
  }
}
function codexTerminalFit(session){
  if(!session.container?.isConnected||!session.container.getClientRects().length||!codexTerminalCurrent(session.context))return;
  session.fit.fit();const cols=session.term.cols,rows=session.term.rows;
  if(session.state.status!=='running')return;
  if(cols===session.state.cols&&rows===session.state.rows)return;session.state.cols=cols;session.state.rows=rows;
  session.context.call('terminal/resize',{id:session.state.id,cols,rows}).catch(error=>{if(codexTerminalCurrent(session.context))showError(error);});
}
function codexTerminalWrite(session,data){
  const context=session.context;if(!codexTerminalCurrent(context)||session.state.status!=='running')return;
  session.inputChain=(session.inputChain||Promise.resolve()).then(()=>{if(session.state.status==='running'&&!codexTerminalUi.closed.has(session.key))return context.call('terminal/write',{id:session.state.id,data});}).catch(error=>{if(codexTerminalCurrent(context)&&!codexTerminalUi.closed.has(session.key))showError(error);});
}
function codexTerminalHandleKey(session,event){
  if(event.type!=='keydown')return true;
  const plainMeta=event.metaKey&&!event.ctrlKey&&!event.altKey&&!event.shiftKey;
  const apple=/Mac|iPhone|iPad/i.test(navigator.platform||navigator.userAgent),plainCommand=plainMeta||!apple&&event.ctrlKey&&!event.metaKey&&!event.altKey&&!event.shiftKey;
  const consume=()=>{event.preventDefault();event.stopPropagation();return false;};
  if(plainCommand&&event.key==='t'){codexTerminalCreate(session.context,null,codexTerminalUi.view).catch(showError);return consume();}
  if(plainCommand&&event.key.toLowerCase()==='k'){session.term.clear();return consume();}
  const controls={ArrowLeft:'\u0001',ArrowUp:'\u0001',ArrowRight:'\u0005',ArrowDown:'\u0005',Backspace:'\u0015',Delete:'\u000b'};
  if(plainMeta&&controls[event.key]){codexTerminalWrite(session,controls[event.key]);return consume();}
  const key=event.key.toLowerCase(),ctrl=event.ctrlKey&&!event.altKey&&!event.metaKey;
  const copy=plainMeta&&key==='c'&&session.term.hasSelection()||!apple&&ctrl&&(key==='c'&&(event.shiftKey||session.term.hasSelection())||key==='insert'&&!event.shiftKey);
  if(copy){const text=session.term.getSelection();if(text){let copied=false;try{copied=document.execCommand?.('copy')===true;}catch{}if(!copied&&navigator.clipboard?.writeText)navigator.clipboard.writeText(text).catch(()=>{});}return consume();}
  const paste=!apple&&(ctrl&&key==='v'||!event.ctrlKey&&event.shiftKey&&!event.altKey&&!event.metaKey&&key==='insert');
  if(paste&&navigator.clipboard?.readText){navigator.clipboard.readText().then(text=>{if(text&&codexTerminalCurrent(session.context)){session.userInput=true;session.term.paste(text);}}).catch(()=>{});return consume();}
  return true;
}
function codexTerminalShow(session){
  const root=codexTerminalUi.root;if(!codexTerminalPanelCurrent(session.context))return;
  const viewport=root.querySelector('.codex-terminal-viewport');
  if(!session.term){
    const container=document.createElement('div');container.className='codex-terminal-surface';session.container=container;
    const font=getComputedStyle(document.documentElement).getPropertyValue('--font-mono').trim()||'monospace';
    session.term=new Terminal({fontFamily:font,fontSize:13,theme:codexTerminalTheme(),cursorBlink:true,scrollback:5000,allowProposedApi:false,convertEol:false});
    session.term.onTitleChange(title=>{session.rawShellTitle=title;codexTerminalPaintTabs();});
    session.fit=new FitAddon.FitAddon();session.term.loadAddon(session.fit);viewport.replaceChildren(container);session.term.open(container);codexTerminalReplay(session,session.state.output,session.sequence);
    session.term.onData(data=>{const user=session.userInput;session.userInput=false;if(!session.replaying||user)codexTerminalWrite(session,data);});
    session.term.onKey(()=>{session.userInput=true;});container.addEventListener('input',()=>{session.userInput=true;},true);container.addEventListener('paste',()=>{session.userInput=true;},true);
    session.term.attachCustomKeyEventHandler(event=>codexTerminalHandleKey(session,event));
    session.observer=new ResizeObserver(()=>codexTerminalFit(session));session.observer.observe(container);
  }else viewport.replaceChildren(session.container);
  session.term.options.theme=codexTerminalTheme();requestAnimationFrame(()=>{codexTerminalFit(session);session.term.focus();});
  const state=root.querySelector('.codex-terminal-status');state.textContent=session.state.status==='running'?'':codexTerminalText('Terminal exited','终端已退出')+(session.state.exitCode!=null?' ('+session.state.exitCode+')':'');
}
async function codexTerminalCreate(context=codexTerminalCapture(),action=null,view=null){
  if(!codexTerminalCurrent(context))return;
  const result=await context.call('terminal/create',{cwd:context.cwd,threadId:context.threadId,...(action?{command:action.command,title:action.name}:{})});
  const session=codexTerminalRemember(result.terminal,context);if(!session||!codexTerminalCurrent(context))return session;
  context.call('terminal/status',{id:session.state.id}).then(result=>codexTerminalRemember(result.terminal,context)).catch(()=>{});
  codexTerminalUi.selected.set(codexTerminalKey(context),session.key);
  if(view!==null&&!codexTerminalPanelCurrent(context,view))return session;
  if(!codexTerminalUi.open||!codexTerminalChatVisible())await codexTerminalRender();
  else {codexTerminalPaintTabs();codexTerminalShow(session);}return session;
}
async function codexTerminalRender(){
  if(!codexTerminalChatVisible())return;
  const keepBottom=desktop.autoScroll;
  codexTerminalUi.open=true;const dock=codexTerminalDock();dock.hidden=false;
  const context=codexTerminalCapture(),ticket=++codexTerminalUi.view,box=dock.querySelector('.codex-terminal-dock-content');
  const root=document.createElement('section');root.id='codex-terminal-panel';
  const toolbar=document.createElement('div');toolbar.className='codex-terminal-toolbar';const tabs=document.createElement('div');tabs.setAttribute('role','tablist');tabs.setAttribute('aria-label',codexTerminalText('Terminals','终端'));const add=document.createElement('button');add.type='button';add.className='icon-btn';add.textContent='+';add.setAttribute('aria-label',codexTerminalText('New terminal','新终端'));add.onclick=async()=>{add.disabled=true;try{await codexTerminalCreate(context,null,ticket);}catch(error){showError(error);}finally{add.disabled=false;}};toolbar.append(tabs,add);
  const viewport=document.createElement('div');viewport.className='codex-terminal-viewport';const status=document.createElement('div');status.className='codex-terminal-status';status.setAttribute('role','status');root.append(toolbar,viewport,status);box.replaceChildren(root);codexTerminalUi.root=root;codexTerminalUi.context=context;
  if(keepBottom&&typeof scrollBottom==='function')requestAnimationFrame(()=>{if(codexTerminalPanelCurrent(context,ticket)){desktop.autoScroll=true;scrollBottom();}});
  try{
    const result=await context.call('terminal/list',{cwd:context.cwd,threadId:context.threadId});
    if(!codexTerminalPanelCurrent(context,ticket))return;
    for(const state of result.data||[])codexTerminalRemember(state,context);
    codexTerminalPaintTabs();let session=codexTerminalUi.sessions.get(codexTerminalUi.selected.get(codexTerminalKey(context)));
    if(!session)session=await codexTerminalCreate(context,null,ticket);if(session&&codexTerminalPanelCurrent(context,ticket))codexTerminalShow(session);
  }catch(error){if(ticket===codexTerminalUi.view&&codexTerminalCurrent(context)){status.textContent=error.message;const retry=document.createElement('button');retry.className='btn-pill';retry.textContent=codexTerminalText('Retry','重试');retry.onclick=()=>codexTerminalRender();status.append(retry);}}
}
const codexTerminalBaseDrawer=renderDrawerContent;
renderDrawerContent=function(...args){return desktop.mode==='codex'&&currentDrawerTab==='terminal'?codexTerminalRender():codexTerminalBaseDrawer(...args);};
const codexTerminalBaseToggleDrawer=toggleDrawer;
toggleDrawer=function(tab){return desktop.mode==='codex'&&tab==='terminal'?codexTerminalToggle():codexTerminalBaseToggleDrawer(tab);};
const codexTerminalBaseSwitchDrawer=typeof switchDrawerTab==='function'?switchDrawerTab:null;
if(codexTerminalBaseSwitchDrawer)switchDrawerTab=function(tab){return desktop.mode==='codex'&&tab==='terminal'?codexTerminalRender():codexTerminalBaseSwitchDrawer(tab);};
const codexTerminalBaseEnvironment=codexEnvironmentStart;
codexEnvironmentStart=async function(request,context=codexEnvironmentContext()){
  if(desktop.mode!=='codex'||request.scriptType!=='action')return codexTerminalBaseEnvironment(request,context);
  workspaceRequire(context);const captured={...context,host:codexTerminalHost(),cwd:request.cwd||currentCwd,threadId:request.threadId??currentThread};
  const result=await context.call('workspace/environment/read',{cwd:captured.cwd,configPath:request.configPath,configSourceCwd:request.configSourceCwd});
  if(!codexTerminalCurrent(captured))return;
  const action=result.environment?.type==='success'?result.environment.environment.actions?.[request.actionIndex]:null;if(!action)throw new Error(codexTerminalText('Action is no longer available','该操作已不可用'));
  return codexTerminalCreate(captured,action);
};
const codexTerminalBaseNotification=handleServerNotification;
handleServerNotification=function(method,params){
  if(method==='terminal/updated'||method==='terminal/output'){
    const host=params.connectionId?remoteUi.byConnection.get(params.connectionId)?.profileId:'local';if(!host)return;
    if(method==='terminal/updated'){
      const state=params.terminal,key=codexTerminalId(host,state.id);let existing=codexTerminalUi.sessions.get(key);
      if(!existing){try{const context=host===codexTerminalHost()?codexTerminalCapture():typeof codexProjectCapture==='function'?{...codexProjectCapture(host),host}:null;if(context)existing=codexTerminalRemember(state,{...context,cwd:state.cwd,threadId:state.threadId});}catch{}}
      if(existing&&state.status==='closed'&&!state.error){codexTerminalForget(existing);return;}
      if(existing){const session=codexTerminalRemember(state,existing.context);if(session&&codexTerminalUi.selected.get(codexTerminalKey(session.context))===session.key)codexTerminalShow(session);}return;
    }
    const session=codexTerminalUi.sessions.get(codexTerminalId(host,params.id));if(!session||(params.sequence||0)<=session.sequence)return;
    if(params.sequence!==session.sequence+1){if(!session.syncing){session.syncing=true;session.context.call('terminal/status',{id:params.id}).then(result=>codexTerminalRemember(result.terminal,session.context)).catch(()=>{}).finally(()=>session.syncing=false);}return;}
    session.sequence=params.sequence;session.state.sequence=params.sequence;session.state.output=((session.state.output||'')+params.delta).slice(-1048576);if(session.term)session.term.write(params.delta,()=>codexTerminalAcknowledge(session,params.sequence));else codexTerminalAcknowledge(session,params.sequence);return;
  }
  return codexTerminalBaseNotification(method,params);
};
const codexTerminalBaseCwd=updateCwdUI;
updateCwdUI=function(...args){const result=codexTerminalBaseCwd(...args);codexTerminalRestoreHost().catch(()=>{});codexTerminalSyncDock();return result;};
for(const name of ['newThread','openThread','switchMode','openSettingsView','openScheduledView','showLibrary','showFilesLibrary','showImagesLibrary','openDocument']){
  const base=window[name];if(typeof base!=='function')continue;window[name]=async function(...args){const intent=++codexTerminalUi.navigationIntent;codexTerminalUi.pendingAuxiliary=!['newThread','openThread','switchMode'].includes(name);if(codexTerminalUi.pendingAuxiliary&&codexTerminalUi.dock)codexTerminalUi.dock.hidden=true;try{return await base.apply(this,args);}finally{if(intent===codexTerminalUi.navigationIntent)codexTerminalUi.pendingAuxiliary=false;codexTerminalSyncDock();}};
}
if(typeof closeSettingsView==='function'){const base=closeSettingsView;closeSettingsView=function(...args){const result=base(...args);codexTerminalSyncDock();return result;};}
new MutationObserver(()=>{if(codexTerminalUi.dock)codexTerminalUi.dock.hidden=!codexTerminalUi.open||!codexTerminalChatVisible();}).observe(document.body,{attributes:true,attributeFilter:['class','data-mode']});
if(typeof codexChromeReady!=='undefined')codexChromeReady.then(codexTerminalRestoreHost).catch(()=>{});
new MutationObserver(()=>{for(const session of codexTerminalUi.sessions.values())if(session.term)session.term.options.theme=codexTerminalTheme();}).observe(document.documentElement,{attributes:true,attributeFilter:['class']});
