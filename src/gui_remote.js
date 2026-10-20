// SSH execution contexts. Account credentials and UI preferences stay on this device.
const remoteUi={profiles:[],connections:new Map(),byConnection:new Map(),threads:new Map(),files:new Map(),projects:new Map(),tasks:new Map(),ambiguous:new Set(),active:null,contexts:new Map(),navigation:0,request:0,modalRevision:0,locationRequest:0,persistTimer:null,local:null};
const remoteLocalRpc=typeof rpcLocalCall==='function'?rpcLocalCall:rpcCall;
const remoteBaseRpc=rpcCall,remoteBaseSend=rpcSend;
const remoteBaseModal=openCustomModal,remoteBaseCloseModal=closeModal;
openCustomModal=function(...args){remoteUi.modalRevision++;return remoteBaseModal(...args);};
closeModal=function(...args){remoteUi.modalRevision++;return remoteBaseCloseModal(...args);};
function remoteProfile(id){return remoteUi.profiles.find(profile=>profile.id===id);}
function remoteContext(){if(currentThread&&remoteUi.threads.has(currentThread))return remoteUi.threads.get(currentThread);return desktop.mode==='codex'?remoteUi.active:null;}
function remoteConnection(profileId){const connection=remoteUi.connections.get(profileId);if(!connection||connection.state!=='connected')throw new Error('SSH connection is closed. Connect to '+(remoteProfile(profileId)?.name||'this host')+' before continuing.');return connection;}
function remoteMark(map,id,profileId){if(typeof id!=='string'||!id||map.has(id)&&map.get(id)===profileId)return;if(map.has(id)&&map.get(id)!==profileId){remoteUi.ambiguous.add(id);throw new Error('Resource ID collision between this device and SSH hosts: '+id+'. Use separate agent stores; this resource is blocked to prevent cross-host operations.');}map.set(id,profileId);remotePersist();}
function remotePersist(){clearTimeout(remoteUi.persistTimer);remoteUi.persistTimer=setTimeout(()=>{const preferences={remoteThreadProfiles:Object.fromEntries(remoteUi.threads),remoteFileProfiles:Object.fromEntries(remoteUi.files),remoteProjectProfiles:Object.fromEntries(remoteUi.projects),remoteTaskProfiles:Object.fromEntries(remoteUi.tasks)};remoteLocalRpc('desktop/settings/update',{preferences}).catch(showError);},500);}
function remoteBind(value,profileId){
  if(!value||typeof value!=='object')return;
  if(Array.isArray(value)){value.forEach(item=>remoteBind(item,profileId));return;}
  const persisted=(value.attachmentId||typeof value.id==='string'&&value.id.startsWith('att_'))&&typeof value.profileId==='string'?value.profileId:profileId;
  const source=value.connectionId?remoteUi.byConnection.get(value.connectionId)?.profileId??persisted:persisted;
  if(value.threadId)remoteMark(remoteUi.threads,value.threadId,source);
  if(value.targetThreadId)remoteMark(remoteUi.threads,value.targetThreadId,source);
  if(typeof value.id==='string'){
    if(value.id.startsWith('att_'))remoteMark(remoteUi.files,value.id,source);
    else if(value.id.startsWith('project_'))remoteMark(remoteUi.projects,value.id,source);
    else if(value.id.startsWith('schedule_')||value.id.startsWith('run_'))remoteMark(remoteUi.tasks,value.id,source);
    else if(value.turns||value.cwd!==undefined&&value.model!==undefined||value.readOnly!==undefined&&value.status)remoteMark(remoteUi.threads,value.id,source);
  }
  for(const [key,child] of Object.entries(value)){if(key==='settings'||key==='preferences')continue;remoteBind(child,source);}
}
function remoteActor(method,params={}){
  if(method.startsWith('remote/')||method.startsWith('account/')||method.startsWith('images/')||method.startsWith('voice/')||method.startsWith('desktop/settings/')||method.startsWith('desktop/document/')||method.startsWith('clipboard/'))return null;
  const threadId=params.threadId||params.targetThreadId;
  if([threadId,params.id,params.taskId].some(id=>id&&remoteUi.ambiguous.has(id)))throw new Error('Ambiguous resource ID is blocked. This ID exists on more than one host.');
  if(threadId)return remoteUi.threads.get(threadId)??null;
  if(method.startsWith('files/')){
    if(params.id)return remoteUi.files.get(params.id)??null;
    return remoteContext();
  }
  if(method.startsWith('desktop/project/')&&params.id)return remoteUi.projects.get(params.id)??null;
  if(method.startsWith('scheduler/')&&(params.id||params.taskId)){const id=params.taskId||params.id;return remoteUi.tasks.get(id)??null;}
  if(params.connectionId)return remoteUi.byConnection.get(params.connectionId)?.profileId??null;
  if(method==='thread/start'&&params.mode&&params.mode!=='codex')return null;
  return /^(thread\/|turn\/|workspace\/|fs\/|command\/|projects\/|model\/|settings\/|mcp\/|skills\/|scheduler\/|approval\/|desktop\/threadMetadata\/|desktop\/project\/)/.test(method)?remoteContext():null;
}
async function remoteCall(profileId,method,params={},timeout){
  const connection=typeof profileId==='object'?profileId:remoteConnection(profileId);
  const latest=remoteUi.byConnection.get(connection.id)||connection;
  if(latest.state!=='connected'||remoteUi.connections.get(connection.profileId)?.id!==connection.id)throw new Error('SSH connection is closed or replaced. Reconnect before continuing.');
  const result=await remoteLocalRpc('remote/rpc',{connectionId:connection.id,method,params},timeout);
  remoteBind(result,connection.profileId);return result;
}
captureRpcContext=function(){const profile=remoteContext();if(!profile)return {key:'local',call:remoteLocalRpc};const connection=remoteConnection(profile);return {key:profile+'@'+connection.id,call:(method,params={},timeout)=>remoteCall(connection,method,params,timeout)};};
rpcCall=async function(method,params={},timeout){
  // The base mode switch commits desktop.mode before this write. Apply the
  // destination workspace here, after it saved the source draft and document.
  if(method==='desktop/settings/update'&&params.mode===desktop.mode)remoteRestore(params.mode==='codex'?remoteUi.active:null);
  if(method==='voice/capabilities'&&remoteContext())return {available:false,reason:'Voice and dictation are not connected to SSH chats. Use a local Chat or Work conversation.',voices:[]};
  if(method==='voice/start'&&remoteUi.threads.get(params.threadId))throw new Error('Voice is not connected to SSH chats. Use a local Chat or Work conversation.');
  if(method==='desktop/state/read'){
    const local=await remoteLocalRpc(method,params,timeout);remoteBind(local,null);
    const profile=remoteContext();if(!profile)return local;
    const state=await remoteCall(profile,method,params,timeout);
    return {...local,projects:[...(local.projects||[]),...(state.projects||[])],threadMetadata:[...(local.threadMetadata||[]),...(state.threadMetadata||[])]};
  }
  const profileId=remoteActor(method,params);
  if(profileId&&method==='projects/pickFolder')throw new Error('Enter an absolute folder path on the SSH host. The folder picker runs on this device.');
  if(remoteContext()&&method==='openIn/open')throw new Error('Remote editor launch is not connected. Copy the remote path into your SSH editor.');
  if(remoteContext()&&method==='openIn/list')return {targets:[],apps:[],reason:'Remote editor launch is not connected'};
  const result=profileId?await remoteCall(profileId,method,params,timeout):await remoteLocalRpc(method,params,timeout);
  if(!profileId)remoteBind(result,null);return result;
};
rpcSend=function(message){
  if(typeof message.id==='string'&&message.id.startsWith('remote:')&&message.result){
    const connectionId=message.id.split(':')[1],connection=remoteUi.byConnection.get(connectionId);
    if(!connection||connection.state!=='connected'){showError(new Error('SSH connection is closed; this approval cannot be sent.'));return;}
    remoteLocalRpc('remote/approval/respond',{connectionId,requestId:message.id,decision:message.result.decision}).catch(showError);return;
  }
  return remoteBaseSend(message);
};
const remoteBaseMessage=window.__onServerMessage;
window.__onServerMessage=function(message){
  const params=message.params||{};
  if(message.method==='remote/status'){
    const connection=params.connection;if(!connection)return;
    remoteUi.byConnection.set(connection.id,connection);
    const previous=remoteUi.connections.get(connection.profileId);
    if(!previous||previous.id===connection.id||connection.createdAt>previous.createdAt||connection.state==='connecting')remoteUi.connections.set(connection.profileId,connection);
    if(['failed','disconnected'].includes(connection.state)){
      for(const [id,approval] of desktop.approvals){if(approval.params?.connectionId===connection.id)desktop.approvals.delete(id);}
      if(currentThread&&remoteUi.threads.get(currentThread)===connection.profileId&&remoteContext()===connection.profileId){setTurnStatus(false);if(connection.state==='failed')toast('SSH connection lost. Reconnect to continue.');}
      if(typeof renderPendingApprovals==='function')renderPendingApprovals();
    }
    remoteRenderLocation();return;
  }
  if(message.method==='remote/request'||message.method==='remote/notification'){
    const connection=remoteUi.byConnection.get(params.connectionId);if(!connection)return;
    try{remoteBind(params.params,connection.profileId);}catch(error){showError(error);return;}
    if(message.method==='remote/request'){remoteBaseMessage({id:params.id,method:params.method,params:{...params.params,connectionId:params.connectionId}});return;}
    if(params.method==='account/updated'){
      if(remoteContext()===connection.profileId)refreshModelCatalog(false,true).catch(showError);
      return;
    }
    handleServerNotification(params.method,{...params.params,connectionId:params.connectionId});return;
  }
  try{remoteBind(params,null);}catch(error){showError(error);return;}return remoteBaseMessage(message);
};
function remoteSnapshot(){return {cwd:currentCwd,model:currentModel,effort:currentEffort,sandbox:currentSandbox,fast:fastMode};}
function remoteRemember(){const snapshot=remoteSnapshot(),profile=remoteContext();if(profile)remoteUi.contexts.set(profile,snapshot);else remoteUi.local=snapshot;}
function remoteRestore(profileId){const connection=profileId?remoteConnection(profileId):null;const saved=profileId?remoteUi.contexts.get(profileId):remoteUi.local;
  if(saved){currentCwd=saved.cwd;currentModel=saved.model;currentEffort=saved.effort;currentSandbox=saved.sandbox;fastMode=saved.fast;}
  else if(connection){currentCwd=connection.initialize?.cwd||remoteProfile(profileId)?.remoteCwd||'';currentEffort='auto';}
}
draftKey=function(){return currentThread||(desktop.mode==='codex'&&remoteUi.active?'codex@'+remoteUi.active:desktop.mode);};
modelContextKey=function(){const profile=remoteContext();return desktop.mode+'\0'+(profile?profile+'\0'+(remoteUi.connections.get(profile)?.id||'closed'):'local')+'\0'+currentCwd;};
async function remoteLoadProfiles(){const [profiles,status]=await Promise.all([remoteLocalRpc('remote/profile/list'),remoteLocalRpc('remote/status')]);remoteUi.profiles=profiles.data||[];for(const connection of status.data||[]){remoteUi.byConnection.set(connection.id,connection);const old=remoteUi.connections.get(connection.profileId);if(!old||connection.createdAt>=old.createdAt)remoteUi.connections.set(connection.profileId,connection);}return remoteUi.profiles;}
async function remoteRestoreApprovals(connection){
  const result=await remoteCall(connection,'approval/list',{});
  for(const [id,request] of desktop.approvals){if(remoteUi.byConnection.get(request.params?.connectionId)?.profileId===connection.profileId)desktop.approvals.delete(id);}
  for(const request of result.data||[])remoteBaseMessage({id:request.id,method:request.method,params:{...request.params,connectionId:connection.id}});
}
async function remoteConnect(profileId){let connection;
  const result=await remoteLocalRpc('remote/connect',{id:profileId},20000);connection=result.connection;remoteUi.byConnection.set(connection.id,connection);remoteUi.connections.set(profileId,connection);remoteRenderLocation();return connection;
}
async function remoteSelect(profileId){
  const request=++desktop.navigation;saveDraft();await flushDocument();if(request!==desktop.navigation)return;remoteRemember();
  const connection=profileId?await remoteConnect(profileId):null;if(request!==desktop.navigation)return;
  closeModal();closeFloatingMenu();if(typeof closeDrawer==='function')closeDrawer();
  remoteUi.active=profileId;
  let work;if(desktop.mode!=='codex')work=remoteBaseSwitchMode('codex');else{remoteRestore(profileId);if(typeof closeScheduledView==='function')closeScheduledView();work=remoteBaseNewThread(false,true);}
  const ownedNavigation=desktop.navigation;await work;if(ownedNavigation!==desktop.navigation)return;
  desktop.threads=[];$('thread-list')?.replaceChildren();
  updateCwdUI(currentCwd);remoteRenderLocation();await Promise.all([refreshThreads(),refreshModelCatalog(false,true),typeof refreshProjects==='function'?refreshProjects():Promise.resolve(),connection?remoteRestoreApprovals(connection):Promise.resolve()]);
}
const remoteBaseSwitchMode=switchMode;
switchMode=async function(mode){
  remoteRemember();
  const request=++desktop.navigation;
  if(mode==='codex'&&remoteUi.active){await remoteConnect(remoteUi.active);if(request!==desktop.navigation)return;}
  const work=remoteBaseSwitchMode(mode),ownedNavigation=desktop.navigation;await work;if(ownedNavigation!==desktop.navigation)return;
  updateCwdUI(currentCwd);remoteRenderLocation();await refreshModelCatalog(false,true);
};
const remoteBaseNewThread=newThread;
newThread=async function(keepProject=true,skipSave=false){
  if(!skipSave)remoteRemember();const work=remoteBaseNewThread(keepProject,skipSave),navigation=desktop.navigation;await work;if(navigation!==desktop.navigation)return;
  const destination=desktop.mode==='codex'?remoteUi.active:null;remoteRestore(destination);updateCwdUI(currentCwd);remoteRenderLocation();
};
const remoteBaseOpenThread=openThread;
openThread=async function(id){
  const profile=remoteUi.threads.get(id)??null;remoteMark(remoteUi.threads,id,profile);
  if(profile){if(desktop.mode!=='codex'||remoteUi.active!==profile||remoteUi.connections.get(profile)?.state!=='connected')await remoteSelect(profile);}
  const previous=remoteContext();if(!profile&&previous)remoteRemember();
  const result=await remoteBaseOpenThread(id);if(currentThread!==id)return result;
  if(!profile){if(desktop.mode==='codex')remoteUi.active=null;const cwd=currentCwd;remoteRestore(null);currentCwd=cwd;updateCwdUI(cwd);}
  remoteRenderLocation();await refreshModelCatalog(false,true);return result;
};
const remoteBaseProject=typeof openProject==='function'?openProject:null;
if(remoteBaseProject)openProject=async function(project){const profile=remoteUi.projects.get(project.id)??null;if(profile!==remoteContext())await remoteSelect(profile);return remoteBaseProject(project);};
const remoteBaseAttachmentInput=typeof attachmentInput==='function'?attachmentInput:null;
if(remoteBaseAttachmentInput)attachmentInput=function(metadata){const value=remoteBaseAttachmentInput(metadata);const profile=metadata.profileId??remoteUi.files.get(metadata.id)??(metadata.connectionId?remoteUi.byConnection.get(metadata.connectionId)?.profileId:null);if(profile){value.profileId=profile;value.connectionId=remoteUi.connections.get(profile)?.id;remoteMark(remoteUi.files,metadata.id,profile);}return value;};
// Capture destination before FileReader yields; another tab/host may open meanwhile.
if(typeof addAttachments==='function')addAttachments=async function(files,options={}){
  const profile=remoteContext(),connection=profile?remoteConnection(profile):null,key=fileDraftKey(),ticket=desktop.navigation;
  fileDraftMaps();filesUi.importing++;updateSubmitButton();if($('files-import-status'))$('files-import-status').textContent='Importing files…';
  try{for(const file of Array.from(files)){
    if(file.size>20*1024*1024){showError(new Error(file.name+' exceeds 20 MiB'));continue;}
    try{const dataBase64=await fileBase64(file),params={name:file.name,mimeType:file.type||undefined,dataBase64};
      const result=connection?await remoteCall(connection,'files/import',params):await remoteLocalRpc('files/import',params);remoteBind(result,profile);const metadata={...result.attachment,...(profile?{profileId:profile,connectionId:connection.id}:{})},imported=attachmentInput(metadata);
      if(options.attach!==false){if(ticket===desktop.navigation&&fileDraftKey()===key){desktopAttachments.push(imported);saveDraft();renderComposerChips();}else{filesUi.drafts.set(key,[...(filesUi.drafts.get(key)||[]),imported]);}}
      if(metadata.parseStatus!=='ready')toast(metadata.name+': '+(metadata.error||'No readable text was found. The original is in Files.'));
    }catch(error){showError(error);}
  }}finally{await savePreferences({}).catch(showError);filesUi.importing--;if(!filesUi.importing){for(const resolve of filesUi.waiters.splice(0))resolve();if($('files-import-status'))$('files-import-status').textContent='';}updateSubmitButton();}
};
const remoteBasePicker=window.pickNativeFolderForInput;
window.pickNativeFolderForInput=async function(inputId){if(!remoteContext())return remoteBasePicker(inputId);const input=$(inputId);if(input){input.placeholder='Absolute path on '+(remoteProfile(remoteContext())?.name||'SSH host');input.focus();}toast('Enter the remote folder path manually.');};
const remoteBaseOpenMenu=typeof toggleOpenInMenu==='function'?toggleOpenInMenu:null;
if(remoteBaseOpenMenu)toggleOpenInMenu=function(event){if(!remoteContext())return remoteBaseOpenMenu(event);event?.stopPropagation();openFloatingMenuAt('remote-open',event.currentTarget,'',false);const menu=$('floating-menu');const note=document.createElement('p');note.className='remote-note';note.textContent='Remote editor launch is not connected.';menu.append(note,menuButton('Copy remote folder path',()=>writeClipboardText(currentCwd)));};
const remoteBaseSubmit=updateSubmitButton;
updateSubmitButton=function(){remoteBaseSubmit();const profile=remoteContext();if(profile&&remoteUi.connections.get(profile)?.state!=='connected'&&!isTurnActive)$('submit-btn').disabled=true;};
function remoteScheduleLabels(){const profile=remoteProfile(remoteContext());if(!profile)return;const select=document.querySelector('#scheduled-editor [aria-label="Execution location"]');if(select?.options?.[0])select.options[0].textContent=profile.name+' · SSH';const help=document.querySelector('#scheduled-editor .scheduled-help[data-schedule-only]');if(help)help.textContent='Runs on '+profile.name+' while this SSH connection and the remote agent remain running. Disconnecting stops the agent; this is not a background daemon.';}
if(typeof openScheduledEditor==='function'){const base=openScheduledEditor;openScheduledEditor=async function(task,...args){const profile=task?remoteUi.tasks.get(task.id):remoteContext();if(profile&&profile!==remoteContext())await remoteSelect(profile);await base(task,...args);remoteScheduleLabels();};}
const remoteBaseSettings=renderSettingsSection;
renderSettingsSection=async function(section){if(section==='remote'){currentSettingsSection=section;document.querySelectorAll('.settings-nav-btn').forEach(button=>button.classList.toggle('active',button.dataset.sec===section));return remoteSettings();}return remoteBaseSettings(section);};
function remoteButton(label,run,className='btn-pill'){const button=document.createElement('button');button.type='button';button.className=className;button.textContent=label;button.onclick=async()=>{button.disabled=true;try{await run();}catch(error){showError(error);}finally{button.disabled=false;}};return button;}
function remoteRenderLocation(){const button=$('remote-location');if(!button)return;const actor=remoteContext();button.hidden=desktop.mode!=='codex'&&!actor;const profile=remoteProfile(actor),state=remoteUi.connections.get(actor)?.state;button.textContent=actor?(profile?.name||'SSH profile unavailable')+(state==='connected'?' · SSH':' · '+(state||'Disconnected')):'Local';button.dataset.state=state||(actor?'disconnected':'local');button.title=profile?`${profile.user?profile.user+'@':''}${profile.host}:${profile.port}\n${profile.remoteCwd}`:actor?'This chat is bound to an unavailable SSH profile.':'Run Codex on this computer';if(typeof updateSubmitButton==='function')updateSubmitButton();}
async function remoteLocationMenu(event){
  event.stopPropagation();const anchor=event.currentTarget,ticket=desktop.navigation,actor=remoteContext(),request=++remoteUi.locationRequest;
  await remoteLoadProfiles();if(request!==remoteUi.locationRequest||ticket!==desktop.navigation||actor!==remoteContext()||!anchor?.isConnected)return;
  openFloatingMenuAt('remote-location',anchor,'',false);const menu=$('floating-menu');menu.append(menuButton((!remoteUi.active?'✓ ':'')+'Local · This computer',()=>remoteSelect(null)));for(const profile of remoteUi.profiles)menu.append(menuButton((remoteUi.active===profile.id?'✓ ':'')+profile.name+' · SSH',()=>remoteSelect(profile.id)));menu.append(menuButton('Manage SSH connections…',()=>openSettingsView('remote')));
}
async function remoteSettings(){
  const request=++remoteUi.request,box=$('settings-body');box.replaceChildren();const heading=document.createElement('h1');heading.className='settings-h1';heading.textContent='SSH connections';const note=document.createElement('p');note.className='settings-sub';note.textContent='Run the Rust agent on a host you configure. Host keys are verified. Password prompts are disabled. The remote host owns its account credentials.';box.append(heading,note,remoteButton('Add SSH host',()=>remoteEdit()));
  await remoteLoadProfiles();if(request!==remoteUi.request||currentSettingsSection!=='remote')return;
  if(!remoteUi.profiles.length){const empty=document.createElement('p');empty.className='empty-note';empty.textContent='Install mcodex on your host, then add its SSH connection. No host is contacted until you choose Connect or Inspect host key.';box.append(empty);}
  for(const profile of remoteUi.profiles){const connection=remoteUi.connections.get(profile.id),card=document.createElement('article');card.className='remote-card';const title=document.createElement('strong');title.textContent=profile.name;const address=document.createElement('code');address.textContent=`${profile.user?profile.user+'@':''}${profile.host}:${profile.port} · ${profile.remoteCwd}`;const state=document.createElement('p');state.className='remote-note';state.textContent=connection?.state||'Not connected';card.append(title,address,state);
    if(connection?.error||connection?.stderr){const error=document.createElement('pre');error.className='remote-error';error.textContent=[connection.error,connection.stderr].filter(Boolean).join('\n');card.append(error);}
    const actions=document.createElement('div');actions.className='remote-actions';actions.append(remoteButton(connection?.state==='connected'?'Use in Codex':'Connect',()=>remoteSelect(profile.id)),remoteButton('Inspect host key…',()=>remoteInspect(profile.id)),remoteButton('Edit',()=>remoteEdit(profile)),remoteButton('Remote account',()=>remoteAccount(profile.id)));
    if(['connected','connecting'].includes(connection?.state))actions.append(remoteButton('Disconnect',async()=>{await remoteLocalRpc('remote/disconnect',{connectionId:connection.id});await remoteSettings();}));
    else actions.append(remoteButton('Delete profile',()=>remoteDelete(profile),'btn-pill danger'));
    card.append(actions);box.append(card);
  }
}
function remoteEdit(profile=null){
  openCustomModal(profile?'Edit SSH host':'Add SSH host','<form id="remote-profile-form" class="remote-form"></form>');const form=$('remote-profile-form'),navigation=desktop.navigation,modal=remoteUi.modalRevision;const inputs={};
  for(const [key,label,placeholder] of [['name','Name','Build host'],['host','Host or SSH alias','host.example.com'],['user','User (optional)','Uses SSH config when empty'],['port','Port','22'],['identityFile','Identity file (optional)','Absolute local path; key is never uploaded'],['remoteCwd','Remote folder','/home/user/project'],['agentPath','Rust agent executable','mcodex']]){const item=document.createElement('label');item.textContent=label;const input=document.createElement('input');input.className='modal-input';input.name=key;input.autocomplete='off';input.placeholder=placeholder;input.value=profile?.[key]??(key==='port'?'22':key==='agentPath'?'mcodex':'');input.required=['name','host','remoteCwd'].includes(key);if(key==='port'){input.type='number';input.min='1';input.max='65535';}item.append(input);form.append(item);inputs[key]=input;}
  const note=document.createElement('p');note.className='remote-note';note.textContent='mcodex must be installed on the host. Configure its credentials there. This app does not send your local API key.';const error=document.createElement('p');error.className='remote-error';const submit=document.createElement('button');submit.type='submit';submit.className='btn-pill primary';submit.textContent='Save profile';form.append(note,error,submit);
  form.onsubmit=async event=>{event.preventDefault();submit.disabled=true;try{const params={...(profile?{id:profile.id}:{}),name:inputs.name.value.trim(),host:inputs.host.value.trim(),user:inputs.user.value.trim()||null,port:Number(inputs.port.value),identityFile:inputs.identityFile.value.trim()||null,remoteCwd:inputs.remoteCwd.value.trim(),agentPath:inputs.agentPath.value.trim()||'mcodex'};await remoteLocalRpc(profile?'remote/profile/update':'remote/profile/create',params);if(navigation!==desktop.navigation||modal!==remoteUi.modalRevision||$('remote-profile-form')!==form)return;closeModal();if(currentSettingsSection==='remote')await remoteSettings();}catch(e){error.textContent=e.message;}finally{submit.disabled=false;}};
}
async function remoteInspect(profileId){
  const profile=remoteProfile(profileId),ticket=desktop.navigation,request=++remoteUi.request,section=currentSettingsSection,modal=remoteUi.modalRevision,context=modelContextKey();const result=await remoteLocalRpc('remote/hostKey/probe',{profileId},15000);if(ticket!==desktop.navigation||request!==remoteUi.request||section!==currentSettingsSection||modal!==remoteUi.modalRevision||context!==modelContextKey())return;openCustomModal('Verify SSH host key','<div id="remote-trust"></div>');const box=$('remote-trust'),ownedModal=remoteUi.modalRevision;const note=document.createElement('p');note.textContent=`Compare these fingerprints for ${result.host}:${result.port} with a trusted source. A network scan does not verify identity.`;box.append(note);
  result.keys.forEach((key,index)=>{const row=document.createElement('pre');row.className='remote-fingerprint';row.textContent=result.fingerprints[index]+'\n'+key;box.append(row);});
  const label=document.createElement('label');label.className='remote-confirm';const check=document.createElement('input');check.type='checkbox';label.append(check,document.createTextNode('I compared these fingerprints with the host owner.'));box.append(label);
  const accept=remoteButton('Trust and connect',async()=>{await remoteLocalRpc('remote/hostKey/trust',{profileId,keys:result.keys,fingerprints:result.fingerprints});if(ticket!==desktop.navigation||ownedModal!==remoteUi.modalRevision||$('remote-trust')!==box)return;closeModal();await remoteSelect(profileId);},'btn-pill primary');accept.disabled=true;check.onchange=()=>{accept.disabled=!check.checked;};box.append(accept);if(!profile)throw new Error('SSH profile is no longer available');
}
async function remoteAccount(profileId){const ticket=desktop.navigation,request=++remoteUi.request,section=currentSettingsSection,modal=remoteUi.modalRevision,context=modelContextKey();const result=await remoteCall(profileId,'account/read');if(ticket!==desktop.navigation||request!==remoteUi.request||section!==currentSettingsSection||modal!==remoteUi.modalRevision||context!==modelContextKey())return;openCustomModal('Remote account','<div id="remote-account-details"></div>');const text=document.createElement('p');text.textContent=result.account?`Connected on the remote host: ${result.account.type==='apiKey'?'OpenAI API key':'ChatGPT'}.`:'No account is connected on this host. Run mcodex login on the host, then reconnect.';const note=document.createElement('p');note.className='remote-note';note.textContent='Local credentials are not copied to this host.';$('remote-account-details').append(text,note);}
function remoteDelete(profile){openCustomModal('Delete SSH profile?','<div id="remote-delete-profile"></div>');const box=$('remote-delete-profile');const text=document.createElement('p');text.textContent='Delete '+profile.name+' from this computer? Files and chats on the host are kept.';box.append(text,remoteButton('Delete profile',async()=>{await remoteLocalRpc('remote/profile/delete',{id:profile.id});if(remoteUi.active===profile.id)await remoteSelect(null);closeModal();await remoteSettings();},'btn-pill danger'));}
async function installRemoteUi(){
  await desktopReady;remoteUi.local=remoteSnapshot();const preferences=desktop.state?.settings?.preferences||{};
  for(const [key,map] of [['remoteThreadProfiles',remoteUi.threads],['remoteFileProfiles',remoteUi.files],['remoteProjectProfiles',remoteUi.projects],['remoteTaskProfiles',remoteUi.tasks]])for(const [id,profile] of Object.entries(preferences[key]||{})){try{remoteMark(map,id,profile);}catch(error){showError(error);}}
  for(const thread of desktop.threads||[]){try{remoteMark(remoteUi.threads,thread.id,null);}catch(error){showError(error);}}
  if(typeof filesUi!=='undefined'){fileDraftMaps();for(const files of filesUi.drafts.values())for(const file of files)if(file.profileId){try{remoteMark(remoteUi.files,file.attachmentId||file.id,file.profileId);}catch(error){showError(error);}}}
  const button=document.createElement('button');button.id='remote-location';button.type='button';button.className='pill-badge remote-location';button.onclick=event=>remoteLocationMenu(event).catch(showError);document.querySelector('.topbar-left')?.append(button);
  const nav=document.createElement('button');nav.className='settings-nav-btn';nav.dataset.sec='remote';nav.textContent='SSH connections';nav.onclick=()=>renderSettingsSection('remote');document.querySelector('.settings-nav')?.append(nav);
  await remoteLoadProfiles();remoteRenderLocation();
}
const remoteUiReady=installRemoteUi().catch(showError);
