// Codex desktop chrome. Existing controls keep their RPC, draft and host bindings.
const codexChrome={mounted:false,placements:[],history:[],position:-1,replaying:false};
const codexMark="CODEX_MARK_SVG";
const codexSourceIcons="CODEX_SOURCE_ICONS";
function codexIcon(name){return (codexSourceIcons[name]||'').replaceAll('#0D0D0D','currentColor');}
function codexText(en,zh){return /^zh\b/i.test(desktop.state?.settings.preferences.codexLanguage||navigator.language||'en')?zh:en;}
function codexEffortLabel(effort){return codexText(EFFORT_LABELS[effort]||effort,({auto:'自动',none:'无',minimal:'最低',low:'低',medium:'中',high:'高',xhigh:'极高',max:'最高',ultra:'超高'})[effort]||effort);}
function codexRememberPlacement(node){
  if(!node)return;
  const marker=document.createComment('Codex control home');node.before(marker);
  codexChrome.placements.push({node,marker});
}
function codexButton(label,icon,run){
  const button=document.createElement('button');button.type='button';button.className='icon-btn';button.title=label;button.setAttribute('aria-label',label);
  button.innerHTML=icon;button.onclick=()=>Promise.resolve().then(run).catch(showError);return button;
}
function codexSyncHistory(){
  if($('codex-back'))$('codex-back').disabled=codexChrome.position<=0;
  if($('codex-forward'))$('codex-forward').disabled=codexChrome.position>=codexChrome.history.length-1;
}
function codexNavigationIntent(){return typeof codexProjectsUi!=='undefined'?codexProjectsUi.intent:desktop.navigation;}
function codexRecordView(){
  if(!codexChrome.mounted||codexChrome.replaying||desktop.mode!=='codex')return;
  const view={threadId:currentThread,profile:remoteContext()||null,cwd:currentCwd,projectId:desktop.projectId,
    section:document.body.classList.contains('in-settings')?currentSettingsSection:null};
  const prior=codexChrome.history[codexChrome.position];
  if(prior&&JSON.stringify(prior)===JSON.stringify(view))return;
  codexChrome.history.splice(codexChrome.position+1);codexChrome.history.push(view);
  if(codexChrome.history.length>80)codexChrome.history.shift();
  codexChrome.position=codexChrome.history.length-1;codexSyncHistory();
}
async function codexNavigateHistory(delta){
  const position=codexChrome.position+delta,view=codexChrome.history[position];if(!view||codexChrome.replaying)return;
  codexChrome.replaying=true;
  try{
    if(desktop.mode!=='codex')await switchMode('codex');
    if(view.threadId)await openThread(view.threadId);
    else{
      if((remoteContext()||null)!==view.profile)await remoteSelect(view.profile);
      if(desktop.mode!=='codex'||(remoteContext()||null)!==view.profile)return;
      const work=newThread(false),intent=codexNavigationIntent();await work;
      if(intent!==codexNavigationIntent()||desktop.mode!=='codex')return;
      desktop.projectId=view.projectId;updateCwdUI(view.cwd);
    }
    if(currentThread!==view.threadId||desktop.mode!=='codex')return;
    if(view.section)await openSettingsView(view.section);
    if(currentThread!==view.threadId||desktop.mode!=='codex')return;
    codexChrome.position=position;
  }finally{codexChrome.replaying=false;codexRecordView();codexSyncHistory();}
}
function codexUpdateDocumentTitle(){
  const active=desktop.mode==='codex',named=currentThread||document.body.classList.contains('in-settings')||document.body.classList.contains('in-scheduled');
  const title=active?(named?$('topbar-title')?.textContent||'Codex':'Codex'):'Mini ChatGPT';if(document.title!==title)document.title=title;
}
function codexSyncChrome(){
  if(!codexChrome.mounted)return;
  const active=desktop.mode==='codex';
  $('codex-sidebar-toggle').title=active?'Toggle sidebar (⌘B)':'Toggle sidebar (⌘S)';$('codex-sidebar-toggle').setAttribute('aria-label',$('codex-sidebar-toggle').title);
  if(active){
    if($('codex-forward').nextElementSibling!==$('codex-sidebar-toggle'))$('codex-forward').after($('codex-sidebar-toggle'));
    const execution=$('codex-execution-bar'),projectButton=$('project-pill-btn'),locationButton=$('remote-location');
    if(execution.firstElementChild!==projectButton||projectButton.nextElementSibling!==locationButton)execution.prepend(projectButton,locationButton);
    const options=$('codex-composer-options'),modelButton=$('model-pill-btn'),voice=$('voice-composer-controls'),submit=$('submit-btn');
    if(options.firstElementChild!==modelButton||modelButton.nextElementSibling!==voice||voice.nextElementSibling!==submit)options.append(modelButton,voice,submit);
    $('new-label').textContent=codexText('New chat','新聊天');
    $('composer-input').placeholder=codexText('Ask anything','随心输入');
    const heading=document.querySelector('.hero-heading');
    heading.replaceChildren(document.createTextNode(codexText('What do you want to build in ','你想让我们在 ')));
    const displayName=typeof latestGitStatusContext!=='undefined'&&typeof captureRpcContext==='function'&&latestGitStatusContext===captureRpcContext().key+'\0'+currentCwd?latestGitStatus?.workspaceName:null;
    const project=document.createElement('span');project.className='codex-hero-project';project.textContent=displayName||baseName(currentCwd)||'Codex';if(displayName)$('cwd-pill-text').textContent=displayName;heading.append(project,document.createTextNode(codexText('?',' 中构建什么？')));
    const effort=$('codex-effort-summary');effort.textContent=codexEffortLabel(currentEffort);
    const model=modelCatalog.find(item=>item.model===currentModel);
    effort.hidden=!model?.reasoningMetadataKnown||modelEffortOptions(model).length<2;
    const modelNameText=$('model-pill-text').textContent;
    $('model-pill-btn').setAttribute('aria-label','Model: '+modelNameText+(effort.hidden?'':' · '+effort.textContent));
    const location=$('remote-location');
    if(location){const label=remoteContext()?location.textContent:codexText('This computer','此计算机');location.innerHTML=codexIcon('computer');location.append(document.createTextNode(label));}
    $('sandbox-pill-text').textContent=typeof codexPermissionLabel==='function'?codexPermissionLabel():codexText(SANDBOX_LABELS[currentSandbox]||currentSandbox,({'read-only':'只读','workspace-write':'工作区写入','danger-full-access':'完全访问'})[currentSandbox]||currentSandbox);
    $('codex-notifications')?.classList.toggle('has-unread',desktop.threads.some(thread=>thread.unread));
  }else{
    for(const {node,marker} of codexChrome.placements)if(marker.nextSibling!==node)marker.after(node);
    $('model-pill-btn').setAttribute('aria-label','Select model');
  }
  codexUpdateDocumentTitle();
  document.body.classList.toggle('codex-chrome',active);codexSyncHistory();
}
function codexMenuKeyboard(menu,anchor){
  menu.setAttribute('role','menu');
  const options=()=>Array.from(menu.querySelectorAll('button:not(:disabled),input')).filter(node=>node.getClientRects().length);
  menu.onkeydown=event=>{
    if(event.key==='Escape'){event.preventDefault();closeFloatingMenu();anchor.focus();return;}
    if(['ArrowLeft','ArrowRight'].includes(event.key)&&event.target.closest('.codex-thinking-options')){
      const items=Array.from(event.target.closest('.codex-thinking-options').querySelectorAll('button:not(:disabled)')),index=items.indexOf(event.target);event.preventDefault();items[(index+(event.key==='ArrowRight'?1:items.length-1))%items.length]?.focus();return;
    }
    if(!['ArrowDown','ArrowUp','Home','End'].includes(event.key)||event.target.matches('input')&&!['ArrowDown','ArrowUp'].includes(event.key))return;
    const items=options();if(!items.length)return;event.preventDefault();
    const current=items.indexOf(document.activeElement),index=event.key==='Home'?0:event.key==='End'?items.length-1:(current+(event.key==='ArrowUp'?items.length-1:1))%items.length;
    items[index]?.focus();
  };
}
function codexOpenNotifications(){
  const unread=desktop.threads.filter(thread=>thread.unread);
  openCustomModal(codexText('Notifications','通知'),'<div id="codex-notification-list"></div>');const list=$('codex-notification-list');
  for(const thread of unread){const button=menuButton(thread.name||thread.preview||codexText('New chat','新聊天'),async()=>{closeModal();await openThread(thread.id);});list.append(button);}
  if(!unread.length){const note=document.createElement('p');note.className='empty-note';note.textContent=codexText('You’re all caught up','没有新通知');list.append(note);}
  else list.append(menuButton(codexText('Mark all as read','全部标为已读'),async()=>{await Promise.all(unread.map(thread=>rpcCall('desktop/threadMetadata/set',{threadId:thread.id,unread:false})));await refreshThreads();closeModal();codexSyncChrome();}));
}
function codexThreadContext(id){
  if(remoteUi.ambiguous.has(id))throw new Error('This chat ID exists on more than one host. Reconnect with separate agent stores.');
  const profile=remoteUi.threads.get(id)||null;
  if(!profile)return {host:'local',call:rpcLocalCall};
  const connection=remoteConnection(profile);return {host:profile,call:(method,params={},timeout)=>remoteCall(connection,method,params,timeout)};
}
const codexBaseThreadMenu=threadMenu;
threadMenu=function(event,thread){
  codexBaseThreadMenu(event,thread);if(desktop.mode!=='codex'||!$('floating-menu').classList.contains('open'))return;
  const context=codexThreadContext(thread.id),anchor=event.currentTarget||event.target;
  const read=menuButton(thread.unread?codexText('Mark as read','标为已读'):codexText('Mark as unread','标为未读'),async()=>{
    await context.call('desktop/threadMetadata/set',{threadId:thread.id,unread:!thread.unread});await refreshThreads();codexSyncChrome();
  });$('floating-menu').prepend(read);codexMenuKeyboard($('floating-menu'),anchor);
};
const codexBaseArchive=archiveThread;
archiveThread=async function(id){
  if(desktop.mode!=='codex')return codexBaseArchive(id);
  const context=codexThreadContext(id),navigation=desktop.navigation;
  const [threadResult,schedules]=await Promise.all([context.call('thread/read',{threadId:id}),context.call('scheduler/list',{kind:'heartbeat',limit:1000})]);
  if(navigation!==desktop.navigation)return;
  const tasks=(schedules.data||[]).filter(task=>task.targetThreadId===id);
  if((schedules.data||[]).length>=1000)throw new Error(codexText('There are too many scheduled tasks to check this chat. Manage its follow-ups before archiving.','计划任务数量超出检查范围。请先管理此聊天的跟进任务。'));
  const running=threadResult.thread?.status?.type==='active';
  if(!running&&!tasks.length)return codexBaseArchive(id);
  const message=[running?codexText('The response will stop.','正在运行的回复将停止。'):'',tasks.length?codexText(`${tasks.length} scheduled follow-ups will be deleted.`,`${tasks.length} 个计划跟进任务将删除。`):''].filter(Boolean).join(' ');
  if(!await scheduledConfirm(codexText('Archive this chat?','归档此聊天？'),message,codexText('Archive','归档')))return;
  if(running){
    await context.call('turn/interrupt',{threadId:id});
    const deadline=Date.now()+5000;
    while((await context.call('thread/read',{threadId:id})).thread?.status?.type==='active'){
      if(Date.now()>deadline)throw new Error(codexText('The response is still stopping. Try archiving again shortly.','回复仍在停止中，请稍后再归档。'));
      await new Promise(resolve=>setTimeout(resolve,80));
    }
  }
  await context.call('thread/archive',{threadId:id});
  if(currentThread===id)await newThread();desktop.tabs=desktop.tabs.filter(tab=>tab.id!==id);await refreshThreads();
  toast(codexText('Chat archived','聊天已归档'),{label:codexText('Undo','撤销'),run:async()=>{
    await context.call('thread/unarchive',{threadId:id});await refreshThreads();
  }});
};
const codexBaseModelPicker=toggleModelPicker;
toggleModelPicker=function(event){
  if(desktop.mode!=='codex')return codexBaseModelPicker(event);
  event.stopPropagation();const anchor=event.currentTarget;
  openFloatingMenuAt('codex-model',anchor,'',true);const menu=$('floating-menu');if(!menu.classList.contains('open'))return;
  menu.classList.add('codex-model-menu');
  const heading=document.createElement('div');heading.className='menu-section-title';heading.textContent=codexText('Model','模型');menu.append(heading);
  for(const model of modelCatalog){
    if((model.hidden||model.upgrade)&&model.model!==currentModel)continue;
    const button=menuButton('',()=>selectModel(model.model));button.setAttribute('role','menuitemradio');button.setAttribute('aria-checked',String(model.model===currentModel));
    const text=document.createElement('span');text.className='menu-item-text';const name=document.createElement('strong');name.textContent=modelName(model);text.append(name);
    if(model.description){const detail=document.createElement('small');detail.textContent=model.description;text.append(detail);}
    button.append(text);if(model.model===currentModel)button.insertAdjacentHTML('beforeend',CHECK_SVG);menu.append(button);
  }
  const selected=modelCatalog.find(model=>model.model===currentModel),efforts=modelEffortOptions(selected);
  if(selected?.reasoningMetadataKnown&&efforts.length>1){
    const title=document.createElement('div');title.className='menu-section-title codex-model-divider';title.textContent=codexText('Thinking','思考');menu.append(title);
    const group=document.createElement('div');group.className='codex-thinking-options';group.setAttribute('role','group');group.setAttribute('aria-label','Thinking');
    for(const option of efforts){const button=menuButton(codexEffortLabel(option.reasoningEffort),()=>selectEffort(option.reasoningEffort));button.title=option.description||button.textContent;button.setAttribute('role','menuitemradio');button.setAttribute('aria-checked',String(currentEffort===option.reasoningEffort));group.append(button);}menu.append(group);
  }
  const tiers=modelTiers(selected);
  if(tiers.length){const title=document.createElement('div');title.className='menu-section-title codex-model-divider';title.textContent=codexText('Speed','速度');menu.append(title);
    for(const tier of [{id:'default',name:codexText('Standard','标准')},...tiers]){const button=menuButton(tier.id==='fast'?codexText('Fast','快速'):tier.name,()=>selectServiceTier(tier.id));button.setAttribute('role','menuitemradio');button.setAttribute('aria-checked',String(selectedServiceTier()===tier.id));if(selectedServiceTier()===tier.id)button.insertAdjacentHTML('beforeend',CHECK_SVG);menu.append(button);}}
  const refresh=menuButton(codexText('Refresh models','刷新模型'),async()=>{await refreshModelCatalog(true);closeFloatingMenu();});refresh.classList.add('codex-model-divider');menu.append(refresh);
  codexMenuKeyboard(menu,anchor);menu.querySelector('[aria-checked=true]')?.focus();
};
const codexBaseModelUI=updateModelAndEffortUI;
updateModelAndEffortUI=function(...args){const result=codexBaseModelUI(...args);codexSyncChrome();return result;};
const codexBaseCwdUI=updateCwdUI;
updateCwdUI=function(...args){const result=codexBaseCwdUI(...args);codexSyncChrome();return result;};
const codexBaseModeUI=updateModeUI;
updateModeUI=function(...args){const result=codexBaseModeUI(...args);codexSyncChrome();return result;};
const codexBaseSandboxUI=updateSandboxUI;
updateSandboxUI=function(...args){const result=codexBaseSandboxUI(...args);codexSyncChrome();return result;};
const codexBaseRemoteLocation=remoteRenderLocation;
remoteRenderLocation=function(...args){const result=codexBaseRemoteLocation(...args);codexSyncChrome();return result;};
const codexBaseLocationMenu=remoteLocationMenu;
remoteLocationMenu=async function(event){
  const anchor=event.currentTarget;await codexBaseLocationMenu(event);
  if(activeFloatingMenuKey!=='remote-location'||!$('floating-menu').classList.contains('open'))return;
  const menu=$('floating-menu'),title=document.createElement('div');title.className='menu-section-title codex-model-divider';title.textContent=codexText('Worktree','工作树');menu.append(title);
  menu.append(menuButton(codexText('New worktree…','新建工作树…'),()=>createWorktreeDialog()),menuButton(codexText('Manage worktrees','管理工作树'),()=>openSettingsView('git')));
  codexMenuKeyboard(menu,anchor);
};
const codexBaseAttachments=showAttachmentMenu;
showAttachmentMenu=function(event){
  codexBaseAttachments(event);
  if(desktop.mode!=='codex'||!$('floating-menu').classList.contains('open'))return;
  const anchor=event.currentTarget;
  $('floating-menu').append(menuButton(codexText('Web search','网页搜索'),()=>webSearchMenu({stopPropagation(){},currentTarget:anchor})));
  codexMenuKeyboard($('floating-menu'),anchor);
};
const codexBaseNewThread=newThread;
newThread=async function(...args){const result=await codexBaseNewThread(...args);codexSyncChrome();codexRecordView();return result;};
const codexBaseOpenThread=openThread;
openThread=async function(...args){const result=await codexBaseOpenThread(...args);codexSyncChrome();if(currentThread===args[0]){if(desktop.mode==='codex'){const thread=desktop.threads.find(item=>item.id===currentThread);if(thread?.name||thread?.preview)$('topbar-title').textContent=thread.name||thread.preview;}codexRecordView();}return result;};
const codexBaseSendTurn=sendTurn;
sendTurn=async function(...args){const navigation=desktop.navigation,result=await codexBaseSendTurn(...args);if(navigation===desktop.navigation){codexSyncChrome();codexRecordView();}return result;};
const codexBaseSettings=openSettingsView;
openSettingsView=async function(...args){const result=await codexBaseSettings(...args);codexRecordView();return result;};
const codexBaseCloseMenu=closeFloatingMenu;
closeFloatingMenu=function(...args){const menu=$('floating-menu'),owned=activeFloatingMenuKey==='codex-model',restore=owned&&menu.contains(document.activeElement);const result=codexBaseCloseMenu(...args);menu.classList.remove('codex-model-menu');if(owned)menu.onkeydown=null;if(restore)$('model-pill-btn').focus();return result;};
const codexChromeReady=Promise.all([shellReady,remoteUiReady,voiceReady]).then(()=>{
  const toolbar=document.createElement('header');toolbar.id='codex-window-toolbar';toolbar.onmousedown=maybeDragWindow;
  const back=codexButton('Back (⌘[)',codexIcon('back'),()=>codexNavigateHistory(-1));back.id='codex-back';
  const forward=codexButton('Forward (⌘])',codexIcon('forward'),()=>codexNavigateHistory(1));forward.id='codex-forward';
  const sidebar=document.querySelector('.sidebar-top button');
  codexRememberPlacement(sidebar);sidebar.id='codex-sidebar-toggle';
  sidebar.innerHTML=codexIcon('sidebar');
  sidebar.title='Toggle sidebar (⌘B)';sidebar.setAttribute('aria-label','Toggle sidebar (⌘B)');
  toolbar.append(back,forward,sidebar);
  const spacer=document.createElement('div');spacer.className='codex-toolbar-spacer';toolbar.append(spacer);
  const fresh=codexButton('New chat (⌘N)',codexIcon('newChat'),()=>newThread());toolbar.append(fresh);$('app').before(toolbar);
  const execution=document.createElement('div');execution.id='codex-execution-bar';execution.setAttribute('aria-label','Execution context');
  const settings=codexButton('Workspace settings',document.querySelector('.sidebar-footer>.icon-btn').innerHTML,()=>openSettingsView('configuration'));settings.id='codex-workspace-settings';execution.append(settings);document.querySelector('.composer-pill').before(execution);
  const options=document.createElement('div');options.id='codex-composer-options';document.querySelector('.composer-bar').append(options);
  for(const id of ['project-pill-btn','remote-location','model-pill-btn','voice-composer-controls','submit-btn'])codexRememberPlacement($(id));
  const summary=document.createElement('span');summary.id='codex-effort-summary';$('model-pill-text').after(summary);
  $('project-pill-btn').querySelector('svg').outerHTML=codexIcon('folder');
  const notifications=codexButton(codexText('Notifications','通知'),codexIcon('notifications'),codexOpenNotifications);notifications.id='codex-notifications';document.querySelector('.desktop-mode-header>.icon-btn').before(notifications);
  document.querySelector('#home-hero .codex-emblem').innerHTML=codexMark.replaceAll('fill="#0D0D0D"','fill="currentColor"');
  codexChrome.mounted=true;codexSyncChrome();codexRecordView();
  new MutationObserver(codexUpdateDocumentTitle).observe($('topbar-title'),{childList:true,subtree:true,characterData:true});
  document.addEventListener('keydown',event=>{
    if(event.target?.closest?.('#codex-terminal-panel')&&event.ctrlKey&&!event.metaKey)return;
    if(desktop.mode!=='codex'||!(event.metaKey||event.ctrlKey)||event.altKey)return;
    if(['[',']'].includes(event.key)){event.preventDefault();codexNavigateHistory(event.key==='['?-1:1).catch(showError);}
    if(event.key.toLowerCase()==='b'){event.preventDefault();event.stopImmediatePropagation();toggleSidebar();}
    if(event.key.toLowerCase()==='s'){event.preventDefault();event.stopImmediatePropagation();if(event.shiftKey)toggleSidebar();}
  },true);
}).catch(showError);
