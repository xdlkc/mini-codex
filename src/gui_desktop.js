// Desktop product state and transport. Native and preview use the same Rust RPC.
const desktop = { mode:'codex', state:null, threads:[], tabs:[], drafts:new Map(), projectId:null, library:false, document:null, docTimer:null, docSaving:null, loading:false, autoScroll:true, navigation:0, documentNavigation:0, approvals:new Map() };
const $ = id => document.getElementById(id);
const titleCase = text => text.charAt(0).toUpperCase()+text.slice(1);
const uiIcon = name => ({chat:'◌',work:'▤',codex:'⌘',project:'▱',document:'▤',code:'⌘'}[name] || '◌');
const rpcClientPrefix=window.crypto?.randomUUID?.() || ('client-'+Date.now()+'-'+Math.random().toString(36).slice(2));
let eventStream;
let transportReady = Promise.resolve();
if (!window.ipc) {
  transportReady = new Promise((resolve,reject) => {
    eventStream = new EventSource('/events');
    eventStream.onopen = () => { $('connection-status') && ($('connection-status').textContent=''); resolve(); };
    eventStream.onmessage = e => { try { window.__onServerMessage(JSON.parse(e.data)); } catch(error) { console.error(error); } };
    eventStream.onerror = () => { $('connection-status') && ($('connection-status').textContent='Reconnecting…'); };
    setTimeout(() => reject(new Error('Could not connect to the Rust backend')),10000);
  });
  window.ipc = { postMessage: body => fetch('/rpc',{method:'POST',headers:{'Content-Type':'application/json'},body}).then(r=>{if(!r.ok)throw new Error('Backend connection failed');}).catch(error=>{let request;try{request=JSON.parse(body);}catch{}const pending=pendingRpc.get(request?.id);if(pending){pendingRpc.delete(request.id);pending.reject(error);}else showError(error);}) };
}
rpcCall = function(method,params={},timeoutMs=30000) {
  const id=rpcClientPrefix+'-'+nextRpcId++;
  return new Promise((resolve,reject) => {
    const timeout=setTimeout(()=>{pendingRpc.delete(id);reject(new Error(`Request timed out: ${method}`));},timeoutMs);
    pendingRpc.set(id,{resolve:v=>{clearTimeout(timeout);resolve(v);},reject:e=>{clearTimeout(timeout);reject(new Error(e.message||'Request failed'));}});
    rpcSend({id,method,params});
  });
};
const rpcLocalCall=rpcCall;
function captureRpcContext(){return {key:'local',call:rpcLocalCall};}
function toast(message,action) {
  const note=document.createElement('div'); note.className='toast'; note.setAttribute('role','status'); note.textContent=message;
  if(action){const b=document.createElement('button');b.textContent=action.label;b.onclick=()=>{action.run().catch(showError);note.remove();};note.append(b);}
  $('toast-host').append(note); setTimeout(()=>note.remove(),action?9000:4500);
}
function showError(error) { toast(error?.message || String(error)); }
window.addEventListener('unhandledrejection',e=>{e.preventDefault();showError(e.reason);});
window.addEventListener('error',e=>{if(e.message)showError(e.error||e.message);});
const baseServerMessage=window.__onServerMessage;
window.__onServerMessage=function(message){
  if(message.id!==undefined&&message.method){
    const p=message.params||{};desktop.approvals.set(message.id,{id:message.id,method:message.method,params:p});
    if(p.threadId===currentThread)renderPendingApprovals();
    else toast('A task is waiting for approval',{label:'Open task',run:()=>openThread(p.threadId)});
    return;
  }
  baseServerMessage(message);
};
function renderPendingApprovals(){
  $('messages').querySelectorAll('[data-approval]').forEach(card=>card.remove());
  for(const request of desktop.approvals.values()){
    if(request.params.threadId!==currentThread)continue;
    renderApprovalCard(request.id,request.method,request.params);
    const card=$('messages').lastElementChild;card.dataset.approval=request.id;
    card.querySelectorAll('button[data-dec]').forEach(button=>{button.onclick=()=>{rpcSend({id:request.id,result:{decision:button.dataset.dec}});desktop.approvals.delete(request.id);card.remove();};});
  }
}
const previousNotification=handleServerNotification;
handleServerNotification=function(method,p){
  if(method==='error' && p.threadId!==currentThread){toast('A background task needs attention.');return;}
  if(method==='approval/resolved'){desktop.approvals.delete(p.requestId);renderPendingApprovals();return;}
  if(method==='desktop/download/completed'){toast(p.success?'Exported to '+(p.path||'Downloads'):'Export failed');return;}
  if(method==='automation/error'){toast(p.message||'A background task needs attention');return;}
  previousNotification(method,p);
  if(method==='turn/completed') {
    for(const [id,request] of desktop.approvals){if(request.params.threadId===p.threadId)desktop.approvals.delete(id);}
    const tab=desktop.tabs.find(t=>t.id===p.threadId);if(tab)tab.running=false;renderTabs();
    if(p.threadId!==currentThread) rpcCall('desktop/threadMetadata/set',{threadId:p.threadId,unread:true}).then(refreshThreads).catch(showError);
  }
  if(method==='turn/started'){const tab=desktop.tabs.find(t=>t.id===p.threadId);if(tab)tab.running=true;renderTabs();}
};
const originalViewMode=setViewMode;
setViewMode=function(hasMessages){originalViewMode(hasMessages);document.body.classList.toggle('has-messages',hasMessages);};
const originalScrollBottom=scrollBottom;
scrollBottom=function(){if(desktop.autoScroll)originalScrollBottom();updateJumpButton();};
$('conversation-wrap').addEventListener('scroll',()=>{
  const c=$('conversation-wrap');desktop.autoScroll=c.scrollHeight-c.scrollTop-c.clientHeight<120;updateJumpButton();
});
function updateJumpButton(){if($('jump-bottom'))$('jump-bottom').hidden=desktop.autoScroll;}
function draftKey(){return currentThread || desktop.mode;}
function saveDraft(){desktop.drafts.set(draftKey(),$('composer-input').value);}
function restoreDraft(){ $('composer-input').value=desktop.drafts.get(draftKey())||'';autoResizeInput(); }
async function loadDesktop(){ desktop.state=await rpcCall('desktop/state/read');return desktop.state; }
async function savePreferences(patch){ const r=await rpcCall('desktop/settings/update',{preferences:patch});desktop.state.settings=r.settings; }
function applyTheme(theme){
  const dark=theme==='dark'||(theme==='system'&&matchMedia('(prefers-color-scheme: dark)').matches);
  document.documentElement.className=dark?'dark':'light';
}
matchMedia('(prefers-color-scheme: dark)').addEventListener('change',()=>{if(desktop.state?.settings.preferences.theme==='system')applyTheme('system');});
function updateModeUI(){
  document.body.dataset.mode=desktop.mode;
  for(const mode of ['chat','work','codex'])$('mode-'+mode).setAttribute('aria-selected',String(mode===desktop.mode));
  $('new-label').textContent=desktop.mode==='codex'?'New task':'New chat';
  $('sidebar-section-title').textContent=desktop.mode==='codex'?'Tasks':'Chats';
  const headline={chat:'What can I help with?',work:'What would you like to get done?',codex:'What can I help you build?'};
  document.querySelector('.hero-heading').textContent=headline[desktop.mode];
  $('composer-input').setAttribute('aria-label',desktop.mode==='chat'?'Ask ChatGPT':'Describe your task');
  $('composer-input').placeholder=desktop.mode==='chat'?'Ask anything':desktop.mode==='work'?'Describe a task':'Ask anything, @ to add context';
  const suggestions=desktop.mode==='work' ? [
    ['Write a document','Start with an editable page','document'],['Plan a project','Turn an idea into clear next steps','Help me plan a project. Ask about the goal, timeline and constraints.'],
    ['Explore a topic','Research, compare and understand','Help me explore a topic in detail. Ask me what I want to learn.'],['Analyze a file','Add a text file or image','attach']
  ] : [
    ['Review changes','Inspect the current Git diff','Review uncommitted changes in this repository.'],['Explain this codebase','Understand modules and data flow','Explain the architecture and key modules of this repository.'],
    ['Fix a bug','Find the cause and verify the fix','Help me diagnose a bug. Inspect this project and ask about the failure.'],['Build a feature','Turn an idea into working code','Help me build a feature in this repository. Ask me what I want to add.']
  ];
  const grid=document.querySelector('.starter-grid');grid.replaceChildren();
  for(const [title,desc,action] of suggestions){
    const b=document.createElement('button');b.className='starter-card';b.innerHTML=`<span class="nav-symbol">${uiIcon(desktop.mode)}</span><div><div class="starter-card-title">${escapeHtml(title)}</div><div class="starter-card-desc">${escapeHtml(desc)}</div></div>`;
    b.onclick=()=>action==='document'?createDocument():action==='attach'?$('attachment-input').click():fillPrompt(action);grid.append(b);
  }
}
async function switchMode(mode){
  if(!['chat','work','codex'].includes(mode))return;
  const ticket=++desktop.navigation;saveDraft();await flushDocument();if(ticket!==desktop.navigation)return;desktop.mode=mode;desktop.projectId=null;closeFloatingMenu();closeModal();closeDrawer();closeSettingsView();hideLibrary();
  await rpcCall('desktop/settings/update',{mode});if(ticket!==desktop.navigation)return;updateModeUI();await newThread(false,true);await refreshThreads();
}
newThread=async function(keepProject=true,skipSave=false){
  if(!skipSave){desktop.navigation++;saveDraft();}closeSettingsView();hideLibrary();
  if(!keepProject)desktop.projectId=null;
  currentThread=null;currentTurn=null;desktop.loading=false;mentionedFiles=[];desktopAttachments=[];renderComposerChips();
  clearTimeline();setTurnStatus(false);desktop.autoScroll=true;
  $('topbar-title').textContent=titleCase(desktop.mode);$('nav-home').classList.add('active');$('nav-library')?.classList.remove('active');restoreDraft();renderTabs();$('composer-input').focus();
};
function fillPrompt(text){$('composer-input').value=text;autoResizeInput();updateSubmitButton();$('composer-input').focus();}
runPromptPreset=async function(text){fillPrompt(text);};
function addTab(thread){
  let tab=desktop.tabs.find(t=>t.id===thread.id);
  if(!tab){tab={id:thread.id,title:thread.name||thread.preview||'New chat',mode:thread.mode||desktop.mode};desktop.tabs.push(tab);}
  else tab.title=thread.name||thread.preview||tab.title;
  tab.running=thread.status?.type==='active';renderTabs();
}
function renderTabs(){
  $('workspace-tabs').replaceChildren();
  const tabs=desktop.tabs.filter(t=>t.mode===desktop.mode);
  if(tabs.length<2)return;
  for(const t of tabs){
    const b=document.createElement('div');b.className='workspace-tab'+(t.id===currentThread?' active':'');b.setAttribute('role','tab');b.setAttribute('aria-selected',String(t.id===currentThread));b.tabIndex=0;
    const title=document.createElement('span');title.textContent=(t.running?'• ':'')+t.title;b.append(title);
    b.onclick=()=>openThread(t.id).catch(showError);b.onkeydown=e=>{if(e.key==='Enter'||e.key===' ')b.click();};
    const close=document.createElement('button');close.className='tab-close';close.setAttribute('aria-label','Close '+t.title);close.textContent='×';
    close.onclick=e=>{e.stopPropagation();desktop.tabs=desktop.tabs.filter(x=>x.id!==t.id);if(t.id===currentThread)newThread();renderTabs();};b.append(close);$('workspace-tabs').append(b);
  }
}
let threadRefreshVersion=0;
refreshThreads=async function(){
  const version=++threadRefreshVersion;
  const [r,state]=await Promise.all([rpcCall('thread/list',{limit:200,mode:desktop.mode}),rpcCall('desktop/state/read')]);
  if(version!==threadRefreshVersion)return;
  desktop.state=state;desktop.threads=r.data||[];
  $('thread-count').textContent=desktop.threads.length||'';
  const list=$('thread-list');list.replaceChildren();
  const pinned=desktop.threads.filter(t=>t.pinned), recent=desktop.threads.filter(t=>!t.pinned&&!t.projectId);
  const heading=text=>{const h=document.createElement('div');h.className='thread-group-label';h.textContent=text;list.append(h);};
  if(pinned.length){heading('Pinned');pinned.forEach(t=>list.append(threadRow(t)));}
  if(recent.length){heading(pinned.length?'Recents':'');recent.forEach(t=>list.append(threadRow(t)));}
  const projects=state.projects.filter(p=>p.mode===desktop.mode);
  if(projects.length){heading('Projects');for(const p of projects){
    const group=document.createElement('div');group.className='project-group';
    const header=document.createElement('button');header.className='nav-row project-hdr';header.innerHTML=`<span>${uiIcon('project')}</span><span style="flex:1">${escapeHtml(p.name)}</span><span>…</span>`;
    header.onclick=()=>openProject(p);group.append(header);desktop.threads.filter(t=>t.projectId===p.id&&!t.pinned).forEach(t=>group.append(threadRow(t)));list.append(group);
  }}
  if(!desktop.threads.length&&!projects.length){const empty=document.createElement('div');empty.className='empty-note';empty.textContent=desktop.mode==='codex'?'Your tasks will appear here.':'Your chats will appear here.';list.append(empty);}
  desktop.tabs.forEach(tab=>{const t=desktop.threads.find(t=>t.id===tab.id);if(t){tab.title=t.name||t.preview||tab.title;tab.running=t.status?.type==='active';}});renderTabs();
};
function threadRow(t){
  const row=document.createElement('div');row.className='thread-row'+(t.id===currentThread?' active':'');row.tabIndex=0;row.setAttribute('role','button');
  const title=t.name||t.preview||'New chat';
  row.innerHTML=`${t.status?.type==='active'?'<span class="thread-status-dot"></span>':''}<span class="thread-title">${escapeHtml(title)}</span>${t.unread?'<span aria-label="Unread">•</span>':''}<span class="thread-actions"><button class="thread-act-btn" aria-label="Options for ${escapeHtml(title)}">…</button></span>`;
  row.onclick=e=>{if(e.target.closest('button')){threadMenu(e,t);return;}openThread(t.id).catch(showError);};
  row.onkeydown=e=>{if(e.key==='Enter')openThread(t.id).catch(showError);};
  row.oncontextmenu=e=>{e.preventDefault();threadMenu(e,t);};return row;
}
openThread=async function(id){
  const ticket=++desktop.navigation;saveDraft();await flushDocument();if(ticket!==desktop.navigation)return;closeSettingsView();hideLibrary();closeFloatingMenu();
  const r=await rpcCall('thread/resume',{threadId:id});if(ticket!==desktop.navigation)return;
  if(r.thread.mode&&r.thread.mode!==desktop.mode){desktop.mode=r.thread.mode;updateModeUI();}
  currentThread=r.thread.id;desktop.projectId=r.thread.projectId||null;updateCwdUI(r.cwd||currentCwd);clearTimeline();desktop.autoScroll=true;
  for(const turn of r.thread.turns||[]){activeTurnBlock=null;for(const item of turn.items||[])upsertTimelineItem(item);finalizeTurnWorkedDivider(activeTurnBlock,turn.durationMs);}
  $('topbar-title').textContent=r.thread.name||r.thread.preview||'Chat';$('nav-home').classList.remove('active');$('nav-library')?.classList.remove('active');
  setTurnStatus(r.thread.status?.type==='active');addTab(r.thread);restoreDraft();renderPendingApprovals();
  if(typeof webSearchThreadOpened==='function')webSearchThreadOpened(r.thread);
  await rpcCall('desktop/threadMetadata/set',{threadId:currentThread,unread:false});refreshThreads();
};
function menuButton(label,run){const b=document.createElement('button');b.className='menu-item';b.textContent=label;b.onclick=()=>{closeFloatingMenu();Promise.resolve().then(run).catch(showError);};return b;}
function threadMenu(e,t){
  e.stopPropagation();const anchor=e.currentTarget||e.target;openFloatingMenuAt('thread-'+t.id,anchor,'',false);const menu=$('floating-menu');
  menu.append(menuButton(t.pinned?'Unpin':'Pin',async()=>{await rpcCall('desktop/threadMetadata/set',{threadId:t.id,pinned:!t.pinned});await refreshThreads();}));
  menu.append(menuButton('Rename',()=>promptRenameThread(t.id,t.name||t.preview||'New chat')));
  menu.append(menuButton('Move to project',()=>moveThreadDialog(t)));
  menu.append(menuButton('Open a copy',async()=>{const r=await rpcCall('thread/fork',{threadId:t.id});await openThread(r.thread.id);}));
  menu.append(menuButton('Archive',()=>archiveThread(t.id)));
  if(!t.readOnly)menu.append(menuButton('Delete',()=>confirmDeleteThread(t)));
}
async function archiveThread(id){await rpcCall('thread/archive',{threadId:id});if(currentThread===id)await newThread();desktop.tabs=desktop.tabs.filter(t=>t.id!==id);await refreshThreads();toast('Chat archived',{label:'Undo',run:async()=>{await rpcCall('thread/unarchive',{threadId:id});await refreshThreads();}});}
function confirmDeleteThread(t){
  openCustomModal('Delete chat?',`<p>This permanently deletes “${escapeHtml(t.name||t.preview||'New chat')}” from Mini ChatGPT.</p><div style="display:flex;gap:8px;justify-content:flex-end"><button class="btn-pill" onclick="closeModal()">Cancel</button><button class="btn-pill danger" id="confirm-delete">Delete</button></div>`);
  $('confirm-delete').onclick=async()=>{await rpcCall('thread/delete',{threadId:t.id});closeModal();desktop.tabs=desktop.tabs.filter(x=>x.id!==t.id);if(currentThread===t.id)await newThread();await refreshThreads();};
}
deleteThread=async id=>{const t=desktop.threads.find(t=>t.id===id);if(t)confirmDeleteThread(t);};
async function moveThreadDialog(t){
  const projects=desktop.state.projects.filter(p=>p.mode===desktop.mode);
  openCustomModal('Move to project','<div id="move-project-options"></div>');const box=$('move-project-options');
  for(const p of [{id:null,name:'No project'},...projects])box.append(menuButton(p.name,async()=>{await rpcCall('desktop/threadMetadata/set',{threadId:t.id,projectId:p.id});if(currentThread===t.id)desktop.projectId=p.id;closeModal();await refreshThreads();}));
}
async function createDesktopProject(){
  openCustomModal('New project',`<label for="desktop-project-name">Project name</label><input class="modal-input" id="desktop-project-name" placeholder="My project"/><label for="desktop-project-instructions">Instructions</label><textarea class="modal-input" id="desktop-project-instructions" placeholder="What should ChatGPT know about this project?" rows="4"></textarea><button class="btn-pill primary" id="save-desktop-project">Create project</button>`);
  $('desktop-project-name').focus();$('save-desktop-project').onclick=async()=>{const name=$('desktop-project-name').value.trim();if(!name)return;const r=await rpcCall('desktop/project/create',{name,mode:desktop.mode,instructions:$('desktop-project-instructions').value});closeModal();await loadDesktop();await openProject(r.project);};
}
async function openProject(project){
  desktop.projectId=project.id;await newThread();$('topbar-title').textContent=project.name;
  const hero=$('home-hero');document.querySelector('.hero-heading').textContent=project.name;
  openCustomModal(project.name,`<label for="project-instructions">Project instructions</label><textarea class="modal-input" id="project-instructions" rows="5">${escapeHtml(project.instructions)}</textarea><div style="display:flex;gap:8px"><button class="btn-pill primary" id="project-save">Save</button><button class="btn-pill" id="project-chat">New chat</button><button class="btn-pill danger" id="project-remove">Remove project</button></div>`);
  $('project-save').onclick=async()=>{await rpcCall('desktop/project/update',{id:project.id,instructions:$('project-instructions').value});closeModal();await refreshThreads();};
  $('project-chat').onclick=()=>{closeModal();$('composer-input').focus();};
  $('project-remove').onclick=async()=>{await rpcCall('desktop/project/delete',{id:project.id});desktop.projectId=null;closeModal();await refreshThreads();updateModeUI();toast('Project removed. Chats and documents kept.');};
}
let desktopAttachments=[];
renderComposerChips=function(){
  const wrap=$('composer-chips');wrap.replaceChildren();
  mentionedFiles.forEach((file,index)=>{const chip=document.createElement('span');chip.className='context-chip';chip.textContent='@'+file;const close=document.createElement('button');close.textContent='×';close.setAttribute('aria-label','Remove '+file);close.onclick=()=>{mentionedFiles.splice(index,1);renderComposerChips();};chip.append(close);wrap.append(chip);});
  desktopAttachments.forEach((file,index)=>{const chip=document.createElement('span');chip.className='context-chip';chip.textContent=file.name;const close=document.createElement('button');close.textContent='×';close.setAttribute('aria-label','Remove '+file.name);close.onclick=()=>{desktopAttachments.splice(index,1);renderComposerChips();};chip.append(close);wrap.append(chip);});
};
function showAttachmentMenu(e){
  openFloatingMenuAt('attachments',e.currentTarget,'',true);const menu=$('floating-menu');
  menu.append(menuButton('Add photos & files',()=>$('attachment-input').click()));
  if(desktop.mode!=='chat')menu.append(menuButton('Mention a workspace file',()=>triggerFileMention()));
}
async function addAttachments(files){
  for(const file of files){
    if(/^image\/(png|jpeg|webp)$/.test(file.type)){
      if(file.size>4_000_000){toast(file.name+' exceeds 4 MB');continue;}
      const url=await new Promise((resolve,reject)=>{const r=new FileReader();r.onload=()=>resolve(r.result);r.onerror=reject;r.readAsDataURL(file);});desktopAttachments.push({type:'image',name:file.name,imageUrl:url});
    }else{
      if(file.size>200_000){toast(file.name+' exceeds 200 KB');continue;}
      const text=await file.text();if(text.includes('\u0000')){toast('Add a text file, PNG, JPEG or WebP image.');continue;}desktopAttachments.push({type:'attachment',name:file.name,text});
    }
  }
  renderComposerChips();
}
$('attachment-input').onchange=async e=>{await addAttachments(e.target.files);e.target.value='';};
$('composer-input').addEventListener('paste',e=>{if(e.clipboardData.files.length){e.preventDefault();addAttachments(e.clipboardData.files).catch(showError);}});
$('composer-host').addEventListener('dragover',e=>e.preventDefault());
$('composer-host').addEventListener('drop',e=>{e.preventDefault();addAttachments(e.dataTransfer.files).catch(showError);});
sendTurn=async function(){
  const serviceTier=typeof selectedServiceTier==='function'?selectedServiceTier():'default';
  if(desktop.loading||isTurnActive)return;
  const raw=$('composer-input').value.trim();if(!raw&&!desktopAttachments.length&&!mentionedFiles.length)return;
  if(['/new','/clear'].includes(raw)){await newThread();return;}
  if(['/diff','/review'].includes(raw)&&desktop.mode==='codex'){toggleDrawer('diff');$('composer-input').value='';autoResizeInput();return;}
  const ticket=desktop.navigation,mode=desktop.mode,projectId=desktop.projectId,model=currentModel,cwd=currentCwd,effort=currentEffort,sandbox=currentSandbox,draft=draftKey(),search=typeof webSearchRequest==='function'?webSearchRequest():{};
  let target=currentThread;desktop.loading=true;const attachments=[...desktopAttachments],mentions=[...mentionedFiles];
  try{
    if(!target){
      const r=await rpcCall('thread/start',{cwd:cwd||undefined,mode,projectId,model,sandbox,...search});target=r.thread.id;
      if(typeof webSearchThreadStarted==='function')webSearchThreadStarted(r.thread,draft,search);
      if(ticket===desktop.navigation){currentThread=target;updateCwdUI(r.cwd);addTab({...r.thread,preview:raw.slice(0,50)});}
    }
    const t=desktop.threads.find(t=>t.id===target);
    if(t?.readOnly){const r=await rpcCall('thread/fork',{threadId:target});target=r.thread.id;if(typeof webSearchThreadStarted==='function')webSearchThreadStarted(r.thread,draft,search);if(ticket===desktop.navigation){currentThread=target;addTab(r.thread);}}
    if(ticket===desktop.navigation){$('composer-input').value='';autoResizeInput();closeAutocomplete();desktopAttachments=[];mentionedFiles=[];renderComposerChips();desktop.drafts.delete(target);desktop.drafts.delete(draft);desktop.autoScroll=true;}
    const input=[...(raw?[{type:'text',text:raw}]:[]),...attachments,...mentions.map(path=>({type:'mention',name:path,path}))];
    await rpcCall('turn/start',{threadId:target,model,reasoningEffort:effort,serviceTier,...search,sandboxPolicy:{type:sandbox==='read-only'?'readOnly':sandbox==='danger-full-access'?'dangerFullAccess':'workspaceWrite'},input});
    if(ticket===desktop.navigation)$('topbar-title').textContent=raw.slice(0,60)||'Chat';
  }catch(error){if(ticket===desktop.navigation){$('composer-input').value=raw;desktopAttachments=attachments;mentionedFiles=mentions;renderComposerChips();autoResizeInput();}else desktop.drafts.set(target||draft,raw);throw error;}
  finally{desktop.loading=false;}
};
handleSubmitOrStop=function(){if(isTurnActive){rpcCall('turn/interrupt',{threadId:currentThread,turnId:currentTurn||''}).catch(showError);}else sendTurn().catch(showError);};
const originalUpsert=upsertTimelineItem;
upsertTimelineItem=function(item){
  if(item.type==='userMessage'&&item.internal){if(!itemEls.has(item.id))itemEls.set(item.id,ensureTurnBlock(true));return;}
  originalUpsert(item);
  if(item.type==='userMessage'&&item.attachments?.length){const group=itemEls.get(item.id);for(const attachment of item.attachments){if(!/^data:image\/(png|jpeg|webp);base64,/.test(attachment.image_url||''))continue;const img=document.createElement('img');img.className='attachment-thumb';img.src=attachment.image_url;img.alt='Attached image';group?.querySelector('.msg-user-bubble')?.append(img);}}
  if(item.type==='userMessage'){const group=itemEls.get(item.id);const edit=group?.querySelector('[data-act=edit]');if(edit)edit.onclick=()=>editUserPrompt((item.content||[]).map(c=>c.text||'').join(''),item.id,item.attachments||[]);}
  if(item.type==='agentMessage'&&item.phase!=='commentary'){
    const el=itemEls.get(item.id);const actions=el?.parentElement.querySelector('.msg-agent-actions');
    if(actions&&!actions.querySelector('[data-action=canvas]')){
      const canvas=document.createElement('button');canvas.className='msg-action-btn';canvas.dataset.action='canvas';canvas.textContent='Open in canvas';canvas.onclick=()=>createDocument(itemTexts.get(item.id)||el.innerText,'Untitled document');actions.append(canvas);
      const retry=document.createElement('button');retry.className='msg-action-btn';retry.textContent='Try again';retry.onclick=()=>retryLastTurn();actions.append(retry);
    }
  }
};
async function retryLastTurn(){
  if(isTurnActive||!currentThread)return;
  const r=await rpcCall('thread/read',{threadId:currentThread,includeTurns:true});const turns=(r.thread.turns||[]).filter(turn=>(turn.items||[]).some(item=>item.type==='userMessage'&&!item.internal));const last=turns.at(-1);if(!last)return;
  const user=last.items.find(i=>i.type==='userMessage'&&!i.internal);if(!user)return;
  const text=(user.content||[]).map(c=>c.text||'').join('');const fork=await rpcCall('thread/fork',{threadId:currentThread,beforeUserMessage:turns.length-1});await openThread(fork.thread.id);fillPrompt(text);await sendTurn();
}
editUserPrompt=async function(text,itemId,attachments=[]){
  if(isTurnActive||!currentThread)return;
  const groups=Array.from($('messages').querySelectorAll('.msg-user-bubble'));const target=itemEls.get(itemId)?.querySelector('.msg-user-bubble');const index=groups.indexOf(target);
  if(index<0){fillPrompt(text);return;}
  openCustomModal('Edit message',`<textarea id="edit-message" class="modal-input" rows="6">${escapeHtml(text)}</textarea><div style="display:flex;justify-content:flex-end;gap:8px"><button class="btn-pill" onclick="closeModal()">Cancel</button><button id="edit-send" class="btn-pill primary">Send as new branch</button></div>`);
  const source=currentThread,ticket=desktop.navigation;
  $('edit-send').onclick=async()=>{const value=$('edit-message').value.trim();if(!value&&!attachments.length)return;const r=await rpcCall('thread/fork',{threadId:source,beforeUserMessage:index});if(ticket!==desktop.navigation)return;closeModal();await openThread(r.thread.id);desktopAttachments=messageAttachments(attachments);renderComposerChips();fillPrompt(value);await sendTurn();};
};
async function openSearch(){
  openCustomModal('Search chats, projects and documents',`<input id="global-search" class="modal-input" placeholder="Search…" aria-label="Search chats, projects and documents"/><div id="search-results" class="search-modal"></div>`);
  let generation=0;
  const search=async()=>{const ticket=++generation;const term=$('global-search')?.value||'';const [r,state,documents]=await Promise.all([rpcCall('thread/list',{searchTerm:term,limit:100}),rpcCall('desktop/state/read'),rpcLocalCall('desktop/document/list',{search:term,limit:100})]);if(ticket!==generation||!$('search-results'))return;
    const box=$('search-results');box.replaceChildren();const q=term.toLowerCase();
    for(const t of r.data||[]){const b=document.createElement('button');b.className='search-result';b.innerHTML=`${escapeHtml(t.name||t.preview||'New chat')}<small>${titleCase(t.mode||'codex')} · Chat</small>`;b.onclick=()=>{closeModal();openThread(t.id).catch(showError);};box.append(b);}
    for(const project of state.projects.filter(p=>(p.name+' '+p.instructions).toLowerCase().includes(q))){const b=document.createElement('button');b.className='search-result';b.innerHTML=`${escapeHtml(project.name)}<small>${titleCase(project.mode)} · Project</small>`;b.onclick=async()=>{closeModal();await switchMode(project.mode);await openProject(project);};box.append(b);}
    for(const doc of documents.items||[]){const b=document.createElement('button');b.className='search-result';b.innerHTML=`${escapeHtml(doc.title)}<small>Work · Local document</small>`;b.onclick=()=>{closeModal();openDocument(doc.id,{layout:'page'}).catch(showError);};box.append(b);}
    if(!box.children.length)box.innerHTML='<div class="empty-note">No results found</div>';
  };$('global-search').oninput=()=>search().catch(showError);$('global-search').focus();await search();
}
function hideLibrary(){$('work-library').classList.add('hidden');desktop.library=false;$('composer-host').style.display='';}
async function showLibrary(){
  closeSettingsView();$('nav-home').classList.remove('active');$('nav-library')?.classList.add('active');desktop.library=true;$('home-hero').classList.add('hidden');$('conversation-wrap').classList.add('hidden');$('composer-host').style.display='none';$('work-library').classList.remove('hidden');$('topbar-title').textContent='Library';
  const r=await rpcLocalCall('desktop/document/list');const root=$('work-library');root.innerHTML='<div class="library-shell"><div class="library-header"><h1>Library</h1><button class="btn-pill primary" id="library-new">+ New document</button></div><div class="library-grid" id="library-grid"></div></div>';
  $('library-new').onclick=()=>createDocument();
  for(const doc of r.items||[]){const b=document.createElement('button');b.className='document-card';b.innerHTML=`<span style="font-size:22px">${uiIcon(doc.kind)}</span><strong>${escapeHtml(doc.title)}</strong><small>${new Date(doc.updatedAt).toLocaleDateString()} · ${doc.kind==='code'?'Code':'Document'}</small>`;b.onclick=()=>openDocument(doc.id,{layout:'page'}).catch(showError);$('library-grid').append(b);}
  if(!r.items?.length)$('library-grid').innerHTML='<div class="empty-note">Documents you create will appear here.</div>';
}
async function createDocument(content='',title='Untitled document'){
  const r=await rpcCall('desktop/document/create',{title,content,kind:'document',projectId:desktop.projectId});await openDocument(r.document.id);
}
async function flushDocument(){
  clearTimeout(desktop.docTimer);
  if(desktop.docSaving)await desktop.docSaving;
  if(!desktop.document||!$('document-title'))return;
  const title=$('document-title').value||'Untitled document',content=$('document-content')?.value??desktop.document.content;
  if(title===desktop.document.title&&content===desktop.document.content)return;
  const id=desktop.document.id;
  desktop.docSaving=rpcCall('desktop/document/update',{id,title,content});
  try{const r=await desktop.docSaving;if(desktop.document?.id===id){desktop.document=r.document;if($('document-save-status'))$('document-save-status').textContent='Saved';}}finally{desktop.docSaving=null;}
}
async function openDocument(id){
  const ticket=++desktop.documentNavigation;await flushDocument();if(ticket!==desktop.documentNavigation)return;const r=await rpcCall('desktop/document/read',{id});if(ticket!==desktop.documentNavigation)return;desktop.document=r.document;
  drawerOpen=true;currentDrawerTab='document';$('inspector-drawer').classList.add('document-open');$('inspector-drawer').classList.remove('closed');
  if(!$('tab-btn-document')){const b=document.createElement('button');b.id='tab-btn-document';b.className='drawer-tab canvas-tab active';b.textContent='Canvas';b.onclick=()=>openDocument(desktop.document.id);document.querySelector('.drawer-tabs').append(b);}document.querySelectorAll('.drawer-tab').forEach(b=>b.classList.remove('active'));
  $('drawer-content').innerHTML=`<div class="document-editor"><input class="document-editor-title" id="document-title" aria-label="Document title" value="${escapeHtml(r.document.title)}"/><textarea class="document-editor-text" id="document-content" aria-label="Document content" placeholder="Start writing…">${escapeHtml(r.document.content)}</textarea><div class="document-editor-footer"><span class="document-editor-status" id="document-save-status">Saved</span><button class="btn-pill" id="document-preview">Preview</button><button class="btn-pill" id="document-export">Export</button><button class="btn-pill danger" id="document-delete">Delete</button></div></div>`;
  const changed=()=>{$('document-save-status').textContent='Saving…';clearTimeout(desktop.docTimer);desktop.docTimer=setTimeout(()=>flushDocument().catch(showError),500);};$('document-title').oninput=changed;$('document-content').oninput=changed;
  $('document-export').onclick=async()=>{await flushDocument();downloadText(desktop.document.title+'.md',desktop.document.content);};
  $('document-preview').onclick=async()=>{await flushDocument();openCustomModal(desktop.document.title,`<div class="msg-agent">${renderMarkdown(desktop.document.content)||'This document is empty.'}</div>`);};
  $('document-delete').onclick=()=>{openCustomModal('Delete document?',`<p>Permanently delete “${escapeHtml(desktop.document.title)}”?</p><button class="btn-pill danger" id="document-confirm-delete">Delete</button>`);$('document-confirm-delete').onclick=async()=>{clearTimeout(desktop.docTimer);await rpcCall('desktop/document/delete',{id});desktop.document=null;closeModal();closeDrawer();if(desktop.library)await showLibrary();toast('Document deleted');};};
}
function downloadText(name,text){const a=document.createElement('a');a.href=URL.createObjectURL(new Blob([text],{type:'text/plain;charset=utf-8'}));a.download=name.replace(/[\\/]/g,'-');a.click();setTimeout(()=>URL.revokeObjectURL(a.href),1000);}
const baseCloseDrawer=closeDrawer;
closeDrawer=function(){desktop.documentNavigation++;flushDocument().catch(showError);baseCloseDrawer();};
const baseRenderDrawer=renderDrawerContent;
renderDrawerContent=async function(){if(currentDrawerTab==='document'){if(desktop.document)await openDocument(desktop.document.id);}else{await flushDocument();$('inspector-drawer').classList.remove('document-open');await baseRenderDrawer();}};
const baseOpenSettings=openSettingsView;
openSettingsView=async function(section='general'){await flushDocument();hideLibrary();$('nav-home').classList.remove('active');$('nav-library')?.classList.remove('active');document.body.classList.add('in-settings');desktop.documentNavigation++;baseCloseDrawer();await baseOpenSettings(section);};
const baseCloseSettings=closeSettingsView;
closeSettingsView=function(){document.body.classList.remove('in-settings');baseCloseSettings();};
const baseHandleComposerInput=handleComposerInput;
handleComposerInput=async function(){if(desktop.mode==='chat'){autoResizeInput();closeAutocomplete();return;}await baseHandleComposerInput();};
const baseRenderSettings=renderSettingsSection;
renderSettingsSection=async function(section){
  currentSettingsSection=section;document.querySelectorAll('.settings-nav-btn').forEach(b=>b.classList.toggle('active',b.dataset.sec===section));
  const box=$('settings-body');
  if(section==='general'){
    const r=await rpcCall('openIn/list');openInApps=r.targets||[];
    box.innerHTML=`<h1 class="settings-h1">General</h1><div class="settings-card"><div class="settings-row"><div><div class="settings-row-label">Default editor</div><div class="settings-row-desc">Preferred app for opening a workspace.</div></div><select id="default-editor" class="settings-select" aria-label="Default editor">${openInApps.map(a=>`<option value="${escapeHtml(a.id)}">${escapeHtml(a.label)}</option>`).join('')}</select></div><div class="settings-row"><div><div class="settings-row-label">Workspace folder</div><div class="settings-row-desc">${escapeHtml(currentCwd)}</div></div><button class="btn-pill" id="change-workspace">Change folder…</button></div></div>`;
    $('default-editor').value=desktop.state.settings.preferences.openIn||openInApps[0]?.id||'';
    $('default-editor').onchange=async e=>{await savePreferences({openIn:e.target.value});toast('Default editor saved');};$('change-workspace').onclick=()=>openCreateProjectDialog('existing');return;
  }
  if(section==='appearance'){
    const theme=desktop.state?.settings.preferences.theme||'system';
    box.innerHTML=`<h1 class="settings-h1">Appearance</h1><div class="settings-card"><div class="settings-row"><div><div class="settings-row-label">Theme</div><div class="settings-row-desc">Choose how Mini ChatGPT looks.</div></div><select id="theme-select" class="settings-select" aria-label="Theme"><option value="system">System</option><option value="light">Light</option><option value="dark">Dark</option></select></div></div>`;
    $('theme-select').value=theme;$('theme-select').onchange=async e=>{applyTheme(e.target.value);await savePreferences({theme:e.target.value});};return;
  }
  if(section==='archived'){
    const r=await rpcCall('thread/list',{archived:true,limit:200});box.innerHTML='<h1 class="settings-h1">Archived chats</h1><div id="archived-list"></div>';
    for(const t of r.data||[]){const row=document.createElement('div');row.className='capability-row';const title=document.createElement('span');title.textContent=t.name||t.preview||'New chat';const restore=document.createElement('button');restore.className='btn-pill';restore.textContent='Restore';restore.onclick=async()=>{await rpcCall('thread/unarchive',{threadId:t.id});await renderSettingsSection('archived');await refreshThreads();};row.append(title,restore);$('archived-list').append(row);}if(!r.data?.length)$('archived-list').innerHTML='<div class="empty-note">No archived chats</div>';return;
  }
  if(section==='shortcuts'){
    box.innerHTML='<h1 class="settings-h1">Keyboard shortcuts</h1>'+[['New chat','⌘N'],['Search','⌘K'],['Toggle sidebar','⌘S'],['Settings','⌘,'],['Switch workspace','⌘1 / ⌘2 / ⌘3'],['Send message','Enter'],['New line','Shift + Enter'],['Close menu','Esc']].map(([name,key])=>`<div class="capability-row"><span>${name}</span><kbd>${key}</kbd></div>`).join('');return;
  }
  if(section==='capabilities'){
    const state=await loadDesktop();box.innerHTML='<h1 class="settings-h1">Features & connections</h1><p class="settings-sub">Features available on this computer. Services that need a separate connection are listed below.</p>';
    for(const [key,feature] of Object.entries(state.capabilities)){const row=document.createElement('div');row.className='capability-row';row.innerHTML=`<div>${escapeHtml(key.replace(/([A-Z])/g,' $1'))}<small>${escapeHtml(feature.reason||'Available on this computer')}</small></div><span class="${feature.available?'status-available':'status-unavailable'}">${feature.available?'Available':'Not connected'}</span>`;box.append(row);}return;
  }
  if(section==='personalization'&&desktop.mode!=='codex'){
    const custom=desktop.state?.settings.preferences.customInstructions||'';box.innerHTML=`<h1 class="settings-h1">Personalization</h1><div class="settings-card"><label for="custom-instructions">Custom instructions</label><p class="settings-sub">What should ChatGPT know about you? How would you like it to respond?</p><textarea id="custom-instructions" class="modal-input" rows="8">${escapeHtml(custom)}</textarea><button id="custom-instructions-save" class="btn-pill primary">Save</button></div>`;
    $('custom-instructions-save').onclick=async()=>{await savePreferences({customInstructions:$('custom-instructions').value});toast('Instructions saved');};return;
  }
  if(section==='account'){
    const r=await rpcCall('account/read');box.innerHTML=`<h1 class="settings-h1">Account</h1><div class="settings-card"><div class="settings-row"><div><div class="settings-row-label">${r.account?.type==='chatgpt'?'ChatGPT':r.account?.type==='apiKey'?'OpenAI API':'Not signed in'}</div><div class="settings-row-desc">${r.account?'Credentials are available on this computer.':'Run mcodex login or set OPENAI_API_KEY to send messages.'}</div></div></div><p class="settings-sub">Subscription details are not available through this connection.</p>`;return;
  }
  await baseRenderSettings(section);
  if(section==='configuration'){const b=document.createElement('button');b.className='btn-pill';b.textContent='Set model ID';b.onclick=()=>{openCustomModal('Set model ID','<input id=custom-model-id class=modal-input aria-label="Model ID" placeholder="Model ID from your provider"/><button id=custom-model-save class="btn-pill primary">Save</button>');$('custom-model-save').onclick=async()=>{const id=$('custom-model-id').value.trim();if(!id)return;await selectModel(id);const r=await rpcCall('model/list');modelCatalog=r.data;closeModal();await renderSettingsSection('configuration');};};box.append(b);}
  // This app owns its configuration. The original app is a read-only import source.
  box.querySelectorAll('code').forEach(el=>{el.textContent=el.textContent.replace(/~\/\.codex\//g,'~/.mcodex/');});
};
const baseToggleSidebar=toggleSidebar;
toggleSidebar=function(){baseToggleSidebar();savePreferences({sidebarCollapsed:$('sidebar').classList.contains('collapsed')}).catch(showError);};
function setupAccessibility(){
  document.querySelectorAll('button[title]').forEach(b=>{if(!b.hasAttribute('aria-label'))b.setAttribute('aria-label',b.title);});
  $('modal-backdrop').querySelector('.modal-card').setAttribute('role','dialog');$('modal-backdrop').querySelector('.modal-card').setAttribute('aria-modal','true');$('modal-backdrop').querySelector('.modal-card').setAttribute('aria-labelledby','modal-title');
}
let modalPreviousFocus;
const baseOpenModal=openCustomModal;
openCustomModal=function(title,html){modalPreviousFocus=document.activeElement;baseOpenModal(title,html);requestAnimationFrame(()=>{$('modal-body').querySelector('input,textarea,button,select')?.focus();});};
const baseCloseModal=closeModal;
closeModal=function(){baseCloseModal();modalPreviousFocus?.focus?.();};
document.addEventListener('keydown',e=>{
  if(e.key==='Escape'){closeModal();closeFloatingMenu();closeAutocomplete();}
  if(e.key==='Tab'&&$('modal-backdrop').classList.contains('open')){
    const focusable=Array.from($('modal-backdrop').querySelectorAll('button,input,textarea,select,a')).filter(el=>!el.disabled&&el.getClientRects().length);
    const first=focusable[0],last=focusable.at(-1);if(e.shiftKey&&document.activeElement===first){e.preventDefault();last?.focus();}else if(!e.shiftKey&&document.activeElement===last){e.preventDefault();first?.focus();}
  }
  if((e.metaKey||e.ctrlKey)&&e.key.toLowerCase()==='k'){e.preventDefault();e.stopImmediatePropagation();openSearch().catch(showError);}
  if((e.metaKey||e.ctrlKey)&&['1','2','3'].includes(e.key)){e.preventDefault();switchMode(['chat','work','codex'][Number(e.key)-1]).catch(showError);}
},true);
// Replace the original search shortcut with the global search overlay.
$('thread-search').addEventListener('focus',()=>openSearch().catch(showError));
async function desktopBoot(){
  const toastHost=document.createElement('div');toastHost.id='toast-host';document.body.append(toastHost);
  const status=document.createElement('span');status.id='connection-status';status.textContent='';$('topbar').append(status);
  const menu=document.createElement('button');menu.className='icon-btn desktop-thread-menu';menu.title='Chat options';menu.setAttribute('aria-label','Chat options');menu.textContent='…';menu.onclick=e=>{const t=desktop.threads.find(t=>t.id===currentThread);if(t)threadMenu(e,t);else openSettingsView('general');};document.querySelector('.topbar-right')?.append(menu);
  const jump=document.createElement('button');jump.id='jump-bottom';jump.className='jump-bottom';jump.hidden=true;jump.textContent='↓';jump.title='Scroll to bottom';jump.onclick=()=>{desktop.autoScroll=true;originalScrollBottom();jump.hidden=true;};$('main-col').append(jump);
  await transportReady;
  const init=await rpcCall('initialize',{clientInfo:{name:'mini-chatgpt',version:'0.3.0'}});currentCwd=init.cwd||'';rpcSend({method:'initialized'});
  const state=await loadDesktop();desktop.drafts=new Map(Object.entries(state.settings.preferences.drafts||{}));desktop.mode=state.settings.mode;applyTheme(state.settings.preferences.theme||'system');
  if(state.settings.preferences.sidebarCollapsed){baseToggleSidebar();}
  const m=await rpcCall('model/list');if(typeof applyModelCatalog==='function')applyModelCatalog(m,true);else{modelCatalog=m.data||[];currentModel=m.defaultModel||modelCatalog[0]?.model||currentModel;currentEffort=m.defaultReasoningEffort||'medium';fastMode=m.serviceTier==='fast';updateModelAndEffortUI();}
  const settings=await rpcCall('settings/read');currentSandbox=settings.sandboxMode||'workspace-write';updateSandboxUI();
  const account=await rpcCall('account/read');$('account-label').textContent=account.account?.type==='chatgpt'?'ChatGPT':account.account?.type==='apiKey'?'OpenAI API':'Mini ChatGPT';$('account-sub').textContent=account.account?'Settings':'Sign in to send messages';$('account-avatar').textContent='M';
  const settingsNav=document.querySelector('.settings-nav');for(const [sec,label] of [['archived','Archived chats'],['shortcuts','Keyboard shortcuts'],['capabilities','Features & connections']]){const b=document.createElement('button');b.className='settings-nav-btn';b.dataset.sec=sec;b.textContent=label;b.onclick=()=>renderSettingsSection(sec);settingsNav.append(b);}
  updateModeUI();updateCwdUI(currentCwd);await refreshThreads();await newThread(true,true);setupAccessibility();
  const approvals=await rpcCall('approval/list').catch(()=>({data:[]}));for(const request of approvals.data||[])desktop.approvals.set(request.id,request);if(desktop.approvals.size)toast('A task is waiting for approval',{label:'Open task',run:()=>openThread(desktop.approvals.values().next().value.params.threadId)});
}
const desktopReady=desktopBoot().catch(error=>{showError(error);$('connection-status')&&($('connection-status').textContent='Connection failed');});

window.__requestClose=async function(){try{saveDraft();await savePreferences({drafts:Object.fromEntries(desktop.drafts)});await flushDocument();rpcSend({__window:'close'});}catch(error){showError(error);rpcSend({__window:'closeCancelled'});}};
window.addEventListener('pagehide',()=>{
  if(!eventStream)return;
  saveDraft();navigator.sendBeacon('/rpc',new Blob([JSON.stringify({id:'draft-exit-'+Date.now(),method:'desktop/settings/update',params:{preferences:{drafts:Object.fromEntries(desktop.drafts)}}})],{type:'application/json'}));
  if(!desktop.document||!$('document-title'))return;
  navigator.sendBeacon('/rpc',new Blob([JSON.stringify({id:'pagehide-'+Date.now(),method:'desktop/document/update',params:{id:desktop.document.id,title:$('document-title').value||'Untitled document',content:$('document-content')?.value??desktop.document.content}})],{type:'application/json'}));
});

let draftSaveTimer;
$('composer-input').addEventListener('input',()=>{
  saveDraft();clearTimeout(draftSaveTimer);draftSaveTimer=setTimeout(()=>savePreferences({drafts:Object.fromEntries(desktop.drafts)}).catch(showError),600);
});
handleSubmitOrStop=function(){clearTimeout(draftSaveTimer);if(isTurnActive){rpcCall('turn/interrupt',{threadId:currentThread,turnId:currentTurn||''}).catch(showError);}else sendTurn().then(()=>savePreferences({drafts:Object.fromEntries(desktop.drafts)})).catch(showError);};

launchOpenIn=async function(appId){closeFloatingMenu();await rpcCall('openIn/open',{target:appId,path:currentCwd});};
const desktopBaseTurnStatus=setTurnStatus;
setTurnStatus=function(active){desktopBaseTurnStatus(active);updateSubmitButton();};
function updateSubmitButton(){const b=$('submit-btn');if(!b)return;b.disabled=!isTurnActive&&(desktop.loading||(!$('composer-input').value.trim()&&!desktopAttachments.length&&!mentionedFiles.length));b.setAttribute('aria-label',isTurnActive?'Stop response':'Send message');}
$('composer-input').addEventListener('input',updateSubmitButton);
const desktopBaseChips=renderComposerChips;
renderComposerChips=function(){desktopBaseChips();updateSubmitButton();};

refreshProjects=async function(){const execution=captureExecutionUi(),r=await execution.call('projects/list');if(executionUiCurrent(execution))projectsList=(r.projects||[]).map(p=>({...p,label:p.label||p.name||baseName(p.path)}));};
const desktopBaseModelPicker=toggleModelPicker;
toggleModelPicker=function(event){desktopBaseModelPicker(event);if(!$('floating-menu').classList.contains('open'))return;$('floating-menu').append(menuButton('Use a custom model…',()=>openSettingsView('configuration')));};

async function writeClipboardText(text){try{if(navigator.clipboard){await navigator.clipboard.writeText(text);return;}}catch{}await rpcCall('clipboard/write',{text});}
copyRawText=async function(button,text){try{await writeClipboardText(text||'');const original=button.innerHTML;button.textContent='Copied';setTimeout(()=>{button.innerHTML=original;},1400);}catch(error){showError(error);}};
copyCodeBlock=async function(button){await copyRawText(button,button.closest('.code-block-wrap')?.querySelector('code')?.innerText||'');};
