let nativeCaptureContext=null;
const nativeHandoffs=new Map();
function nativeWindowRequest(action,fields={}){if(action==='capture')nativeCaptureContext={threadId:currentThread,mode:desktop.mode,navigation:desktop.navigation};rpcSend({__window:action,...fields});}
async function nativeOpenMain(target='main'){
  if(filesUi.importing)throw new Error('Wait for the file import to finish.');
  saveDraft();
  const navigation=desktop.navigation,key=draftKey(),threadId=currentThread;
  const draft={text:$('composer-input').value,attachments:desktopAttachments.map(file=>({...file})),mentions:[...mentionedFiles],mode:desktop.mode,cwd:currentCwd,model:currentModel,projectId:desktop.projectId};
  await savePreferences({drafts:Object.fromEntries(desktop.drafts)});
  if(navigation!==desktop.navigation||key!==draftKey())throw new Error('The chat changed while saving. Try opening the window again.');
  if(!draft.text&&!draft.attachments.length&&!draft.mentions.length){nativeWindowRequest(target,{threadId,mode:draft.mode});return;}
  if(nativeHandoffs.size)return;
  const ticket=crypto.randomUUID(),fingerprint=JSON.stringify(draft);
  return new Promise((resolve,reject)=>{const timer=setTimeout(()=>{nativeHandoffs.delete(ticket);reject(new Error('The other window did not confirm saving this draft. Your original draft is kept.'));},30000);nativeHandoffs.set(ticket,{resolve,reject,timer,key,fingerprint,draft});nativeWindowRequest(target,{threadId,draft,ticket});});
}
function nativePopout(){
  if(typeof remoteContext==='function'&&remoteContext())throw new Error('Open a local chat to pop it out.');
  return nativeOpenMain('popout');
}
window.__nativeInvoke=async function(action,params={}){
  await shellReady;await Promise.all([filesReady,voiceReady]);
  if(action==='thread'){await flushDocument();if(params.threadId)await openThread(params.threadId);else if(nativeWindowInfo.kind!=='main'){await switchMode('chat');}return;}
  if(action==='mode'){await switchMode(params.mode);return;}
  if(action==='handoff'){
    const draft=params.draft;if(!draft)return;
    if(typeof remoteContext==='function'&&remoteContext())await remoteSelect(null);
    let id=params.threadId;if(!id){const result=await rpcLocalCall('thread/start',{mode:['chat','work','codex'].includes(draft.mode)?draft.mode:'chat',cwd:draft.cwd,model:draft.model,projectId:draft.projectId||null});id=result.thread.id;}
    await openThread(id);const old=$('composer-input').value||'';fillPrompt(old&&draft.text&&old!==draft.text?old+'\n\n'+draft.text:draft.text||old);
    for(const file of draft.attachments||[])if(!desktopAttachments.some(item=>(item.attachmentId||item.id||item.imageUrl)===(file.attachmentId||file.id||file.imageUrl)))desktopAttachments.push(file);
    mentionedFiles=[...new Set([...mentionedFiles,...(draft.mentions||[])])];renderComposerChips();saveDraft();await savePreferences({drafts:Object.fromEntries(desktop.drafts)});nativeWindowRequest('handoffComplete',{ticket:params.ticket,threadId:id});return;
  }
  if(action==='handoffComplete'){
    const pending=nativeHandoffs.get(params.ticket);if(!pending)return;clearTimeout(pending.timer);nativeHandoffs.delete(params.ticket);
    const current={text:$('composer-input').value,attachments:desktopAttachments.map(file=>({...file})),mentions:[...mentionedFiles],mode:desktop.mode,cwd:currentCwd,model:currentModel,projectId:desktop.projectId};
    try{if(draftKey()===pending.key&&JSON.stringify(current)===pending.fingerprint){$('composer-input').value='';desktopAttachments=[];mentionedFiles=[];renderComposerChips();saveDraft();await savePreferences({drafts:Object.fromEntries(desktop.drafts)});}pending.resolve();}catch(error){pending.reject(error);throw error;}return;
  }
  if(action==='dictation'){if(voiceUi.dictation)stopDictation();else await startDictation();return;}
  if(action==='captureStarted'){nativeCaptureContext={threadId:currentThread,mode:desktop.mode,navigation:desktop.navigation};return;}
  if(action==='capture'){
    const file=params.attachment;if(!file)return;
    const origin=nativeCaptureContext;nativeCaptureContext=null;
    if(origin&&(origin.threadId!==currentThread||origin.mode!==desktop.mode||origin.navigation!==desktop.navigation)){toast('Screenshot saved in the local file library');return;}
    if(typeof remoteContext==='function'&&remoteContext())throw new Error('Choose a local chat before attaching this screenshot.');
    if(desktop.library||desktop.document){await flushDocument();await newThread();}
    desktopAttachments.push(attachmentInput(file));renderComposerChips();saveDraft();await savePreferences({});$('composer-input').focus();return;
  }
};
window.__nativeHotkeysUpdated=function(status){window.__nativeHotkeys=status;if(currentSettingsSection==='shortcuts'&&document.body.classList.contains('in-settings'))renderSettingsSection('shortcuts').catch(showError);};
const nativeRenderSettings=renderSettingsSection;
renderSettingsSection=async function(section,...args){
  const result=await nativeRenderSettings(section,...args);
  if(section!=='shortcuts'||!window.__mcodexNative)return result;
  const container=$('settings-body'),card=document.createElement('section');card.className='settings-card';card.append(docsText('h2','Global shortcuts'));
  const preferences=desktop.state?.settings?.preferences||{},status=window.__nativeHotkeys||{};
  for(const [key,label,fallback] of [['globalMiniShortcut','Show / hide Mini','option-space'],['globalDictationShortcut','Dictate in Mini','control-option-space'],['globalCaptureShortcut','Capture screenshot','option-shift-space']]){
    const row=document.createElement('label');row.className='settings-row';row.append(docsText('span',label));const select=document.createElement('select');select.setAttribute('aria-label',label);select.className='settings-select';
    for(const [value,title] of [['option-space','⌥ Space'],['control-option-space','⌃ ⌥ Space'],['option-shift-space','⌥ ⇧ Space'],['command-shift-space','⌘ ⇧ Space'],['disabled','Off']]){const option=document.createElement('option');option.value=value;option.textContent=title;select.append(option);}select.value=preferences[key]||fallback;
    select.onchange=async()=>{select.disabled=true;try{await savePreferences({[key]:select.value});nativeWindowRequest('shortcuts');}catch(error){showError(error);}finally{select.disabled=false;}};
    row.append(select);card.append(row);const info=status[key];card.append(docsText('p',info?.registered?'Registered globally':info?.disabled?'Disabled':info?.error||status.error||'Checking registration…','settings-sub'));
  }
  card.append(docsText('p','Mini stays available while the app is running. Closing a window saves it and hides it; Quit saves all windows and stops the app.','settings-sub'));
  container.append(card);
};
const nativeControlsReady=shellReady.then(async()=>{
  if(!window.__mcodexNative)return;
  document.body.classList.add('native-'+nativeWindowInfo.kind);
  const controls=document.createElement('div');controls.className='native-window-controls';
  const button=(label,action)=>{const node=document.createElement('button');node.className='icon-btn';node.textContent=({'Open in main window':'↗','Pop out chat':'↗','New chat':'+','Always on top':'⌖','Hide window':'×','Capture screenshot':'▣'})[label]||label;node.setAttribute('aria-label',label);node.title=label;node.onclick=async()=>{node.disabled=true;try{await action();}catch(error){showError(error);}finally{node.disabled=false;}};controls.append(node);return node;};
  if(nativeWindowInfo.kind==='main'){button('Mini',()=>nativeWindowRequest('mini'));button('Pop out chat',nativePopout);}
  else {button('Open in main window',nativeOpenMain);button('New chat',()=>newThread(false));button('Always on top',()=>nativeWindowRequest('alwaysOnTop')).setAttribute('aria-pressed',String(nativeWindowInfo.kind==='mini'));button('Hide window',()=>nativeWindowRequest('requestClose'));}
  button('Capture screenshot',()=>nativeWindowRequest('capture'));document.querySelector('.topbar-right')?.prepend(controls);
  document.addEventListener('click',event=>{const link=event.target.closest?.('a[href]');if(!event.defaultPrevented&&link&&/^(https?:|mailto:)/i.test(link.href)){event.preventDefault();nativeWindowRequest('external',{url:link.href});}});
  if(nativeWindowInfo.threadId)await window.__nativeInvoke('thread',{threadId:nativeWindowInfo.threadId});
  if(nativeWindowInfo.kind==='mini')$('composer-input').focus();
  nativeWindowRequest('ready');
});
