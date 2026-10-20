// Persisted local attachments are referenced by ID in drafts and messages.
const filesUi={drafts:null,mentions:null,library:false,request:0,importing:0,waiters:[]};
function fileDraftMaps(){if(!filesUi.drafts){const p=desktop.state?.settings.preferences||{};filesUi.drafts=new Map(Object.entries(p.attachmentDrafts||{}));filesUi.mentions=new Map(Object.entries(p.mentionDrafts||{}));}}
function fileDraftKey(){return draftKey();}
function attachmentInput(metadata){return {type:'attachment',attachmentId:metadata.id,id:metadata.id,name:metadata.name,kind:metadata.kind,mimeType:metadata.mimeType,size:metadata.size,parseStatus:metadata.parseStatus};}
function messageAttachments(files){
  const stored=files.filter(file=>file.id),indices=new Set(stored.filter(file=>Number.isInteger(file.imageIndex)).map(file=>file.imageIndex));let image=0;
  return [...stored.map(attachmentInput),...files.filter(file=>file.type==='input_image').filter(()=>!indices.has(image++)).map(file=>({type:'image',imageUrl:file.image_url,name:'Attached image'}))];
}
const filesSaveDraft=saveDraft;
saveDraft=function(){filesSaveDraft();fileDraftMaps();filesUi.drafts.set(fileDraftKey(),desktopAttachments.filter(file=>file.attachmentId).map(file=>({...file})));filesUi.mentions.set(fileDraftKey(),[...mentionedFiles]);};
const filesRestoreDraft=restoreDraft;
restoreDraft=function(){filesRestoreDraft();fileDraftMaps();desktopAttachments=(filesUi.drafts.get(fileDraftKey())||[]).map(file=>({...file}));mentionedFiles=[...(filesUi.mentions.get(fileDraftKey())||[])];renderComposerChips();};
const filesSavePreferences=savePreferences;
savePreferences=async function(patch){fileDraftMaps();return filesSavePreferences({...patch,attachmentDrafts:Object.fromEntries(filesUi.drafts),mentionDrafts:Object.fromEntries(filesUi.mentions)});};
const filesRenderChips=renderComposerChips;
renderComposerChips=function(){
  filesRenderChips();fileDraftMaps();
  Array.from($('composer-chips').children).forEach((chip,index)=>{const file=desktopAttachments[index-mentionedFiles.length];if(file?.attachmentId){chip.classList.add('stored-attachment-chip');if(file.parseStatus!=='ready')chip.classList.add('attachment-unreadable');chip.title=file.parseStatus==='ready'?`${file.name} · ${fileSize(file.size)}`:'This file has no readable content. Open it in Library for details.';}});
  $('composer-chips').querySelectorAll('button').forEach(button=>button.addEventListener('click',()=>{saveDraft();savePreferences({}).catch(showError);}));
};
function fileSize(bytes){return bytes<1024?`${bytes||0} B`:bytes<1024*1024?`${(bytes/1024).toFixed(1)} KB`:`${(bytes/1024/1024).toFixed(1)} MB`;}
function fileBase64(file){return new Promise((resolve,reject)=>{const reader=new FileReader();reader.onload=()=>resolve(String(reader.result).split(',')[1]||'');reader.onerror=()=>reject(new Error('Could not read '+file.name));reader.readAsDataURL(file);});}
addAttachments=async function(files,options={}){
  const key=fileDraftKey(),ticket=desktop.navigation;fileDraftMaps();filesUi.importing++;if($('files-import-status'))$('files-import-status').textContent='Importing files…';updateSubmitButton();
  try{for(const file of Array.from(files)){
    if(file.size>20*1024*1024){showError(new Error(file.name+' exceeds 20 MiB'));continue;}
    try{const result=await rpcCall('files/import',{name:file.name,mimeType:file.type||undefined,dataBase64:await fileBase64(file)});const metadata=result.attachment;
      const imported=attachmentInput(metadata);
      if(options.attach!==false){if(ticket===desktop.navigation&&fileDraftKey()===key){desktopAttachments.push(imported);saveDraft();renderComposerChips();}
      else {const stored=filesUi.drafts.get(key)||[];filesUi.drafts.set(key,[...stored,imported]);}}
      if(metadata.parseStatus!=='ready')toast(metadata.name+': '+(metadata.error||'No readable text was found. The original file is in Library.'));
    }catch(error){showError(error);}
  }}finally{await savePreferences({}).catch(showError);filesUi.importing--;if(!filesUi.importing){for(const resolve of filesUi.waiters.splice(0))resolve();if($('files-import-status'))$('files-import-status').textContent='';}updateSubmitButton();}
};
const filesBaseSubmit=updateSubmitButton;
updateSubmitButton=function(){filesBaseSubmit();if(!isTurnActive&&(filesUi.importing||desktopAttachments.some(file=>file.attachmentId&&file.parseStatus!=='ready')))$('submit-btn').disabled=true;};
const filesBaseSend=sendTurn;
sendTurn=async function(){
  if(filesUi.importing)return;
  if(desktopAttachments.some(file=>file.attachmentId&&file.parseStatus!=='ready')){showError(new Error('Remove unreadable files before sending. Their originals remain in Library.'));return;}
  const key=fileDraftKey(),ticket=desktop.navigation,hadFiles=desktopAttachments.some(file=>file.attachmentId);
  try{await filesBaseSend();if(hadFiles&&ticket===desktop.navigation&&!desktopAttachments.length){filesUi.drafts.delete(key);filesUi.drafts.delete(currentThread);await savePreferences({});}}
  catch(error){fileDraftMaps();if(ticket===desktop.navigation)saveDraft();throw error;}
};
const filesBaseRetry=retryLastTurn;
retryLastTurn=async function(){
  if(scheduledUi.goal?.threadId===currentThread&&scheduledUi.goal.status!=='complete')return filesBaseRetry();
  if(isTurnActive||!currentThread)return;const source=currentThread,ticket=desktop.navigation;
  const result=await rpcCall('thread/read',{threadId:source,includeTurns:true});if(ticket!==desktop.navigation)return;
  const turns=(result.thread.turns||[]).filter(turn=>(turn.items||[]).some(item=>item.type==='userMessage'&&!item.internal)),user=turns.at(-1)?.items.find(item=>item.type==='userMessage'&&!item.internal);if(!user)return;
  const fork=await rpcCall('thread/fork',{threadId:source,beforeUserMessage:turns.length-1});if(ticket!==desktop.navigation)return;await openThread(fork.thread.id);desktopAttachments=messageAttachments(user.attachments||[]);renderComposerChips();fillPrompt((user.content||[]).map(part=>part.text||'').join(''));await sendTurn();
};
function fileAction(label,run,className='btn-pill'){const button=document.createElement('button');button.type='button';button.textContent=label;button.className=className;button.onclick=async()=>{button.disabled=true;try{await run();}catch(error){showError(error);}finally{button.disabled=false;}};return button;}
async function downloadAttachment(id,{local=false}={}){
  const result=await (local?rpcLocalCall:rpcCall)('files/read',{id,includeOriginal:true});const raw=result.content?.dataBase64;if(typeof raw!=='string')throw new Error('The original file is unavailable');
  const bytes=Uint8Array.from(atob(raw),character=>character.charCodeAt(0));const url=URL.createObjectURL(new Blob([bytes],{type:result.attachment.mimeType||'application/octet-stream'}));
  const link=document.createElement('a');link.href=url;link.download=result.attachment.name;link.click();setTimeout(()=>URL.revokeObjectURL(url),1000);
}
async function openAttachment(id,{local=false,allowAttach=true}={}){
  const ticket=desktop.navigation,request=++filesUi.request;openCustomModal('Open file','<p id="attachment-loading">Loading file…</p>');const loading=$('attachment-loading');
  const result=await (local?rpcLocalCall:rpcCall)('files/read',{id,includeContent:true});if(ticket!==desktop.navigation||request!==filesUi.request||$('attachment-loading')!==loading||!$('modal-backdrop').classList.contains('open'))return;const file=result.attachment;
  openCustomModal(file.name,'<div id="attachment-viewer"></div><div id="attachment-viewer-actions" class="file-actions"></div>');const viewer=$('attachment-viewer');
  const status=document.createElement('p');status.className='settings-sub';status.textContent=`${file.kind.toUpperCase()} · ${fileSize(file.size)}${file.pageCount?' · '+file.pageCount+' pages':''}${file.sheetCount?' · '+file.sheetCount+' sheets':''}`;viewer.append(status);
  if(file.kind==='image'&&result.content?.dataBase64){const image=document.createElement('img');image.className='attachment-preview-image';image.alt=file.name;image.src=`data:${file.mimeType};base64,${result.content.dataBase64}`;viewer.append(image);}
  else if(typeof result.content?.text==='string'){
    const text=result.content.text,pre=document.createElement('pre');pre.className='attachment-text-preview';pre.textContent=text.slice(0,100000).replace(/[\uD800-\uDBFF]$/,'');viewer.append(pre);
    if(text.length>100000){const note=document.createElement('p');note.className='settings-sub';note.textContent='Preview shows the first 100,000 characters. Messages use the complete extracted text.';viewer.append(note);}
  } else {const note=document.createElement('p');note.className='attachment-parse-error';note.textContent=file.error||'No readable text was found. You can download the original file.';viewer.append(note);}
  for(const warning of file.warnings||[]){const note=document.createElement('p');note.className='settings-sub';note.textContent=warning;viewer.append(note);}
  $('attachment-viewer-actions').append(fileAction('Download original',()=>downloadAttachment(id,{local})));
  if(allowAttach)$('attachment-viewer-actions').append(fileAction('Add to message',async()=>{if(file.parseStatus!=='ready')throw new Error('This file has no readable content');desktopAttachments.push(attachmentInput(file));renderComposerChips();saveDraft();await savePreferences({});closeModal();if(desktop.library){if(currentThread)await openThread(currentThread);else await newThread();}}));
}
function deleteAttachmentDialog(file){
  openCustomModal('Delete file?', '<strong id="delete-file-name"></strong><p>Delete this file from the local library? Extracted text already used in chats stays in their history. The original file and preview will be removed.</p><div id="delete-file-actions" class="file-actions"></div>');$('delete-file-name').textContent=file.name;
  $('delete-file-actions').append(fileAction('Cancel',async()=>closeModal()),fileAction('Delete',async()=>{await rpcCall('files/delete',{id:file.id});for(const [key,files] of filesUi.drafts||[])filesUi.drafts.set(key,files.filter(item=>item.attachmentId!==file.id));desktopAttachments=desktopAttachments.filter(item=>item.attachmentId!==file.id);renderComposerChips();await savePreferences({});closeModal();await showFilesLibrary();},'btn-pill danger'));
}
async function showFilesLibrary(){
  const ticket=++desktop.navigation;saveDraft();await flushDocument();if(ticket!==desktop.navigation)return;
  closeSettingsView();closeScheduledView();closeFloatingMenu();closeDrawer();desktop.library=true;filesUi.library=true;
  $('home-hero').classList.add('hidden');$('conversation-wrap').classList.add('hidden');$('composer-host').style.display='none';$('work-library').classList.remove('hidden');$('topbar-title').textContent='Library';
  const result=await rpcCall('files/list');if(ticket!==desktop.navigation||!desktop.library)return;
  const root=$('work-library');root.innerHTML='<div class="library-shell"><div class="library-header"><h1>Library</h1><div id="files-library-actions" class="file-actions"></div></div><div id="files-library-grid" class="library-grid"></div></div>';
  $('files-library-actions').append(fileAction('Documents',()=>showLibrary()),fileAction('Files',async()=>showFilesLibrary(),'btn-pill primary'),fileAction('+ Add files',async()=>$('library-file-input').click()));
  for(const file of result.data||[]){const card=document.createElement('article');card.className='document-card file-library-card';
    const open=fileAction(file.name,()=>openAttachment(file.id),'file-card-open');const kind=document.createElement('small');kind.textContent=`${file.kind.toUpperCase()} · ${fileSize(file.size)}`;card.append(open,kind);
    if(file.parseStatus!=='ready'){const state=document.createElement('small');state.className='attachment-parse-error';state.textContent=file.parseStatus==='no_text'?'No readable text':'Could not read file';card.append(state);}
    card.append(fileAction('Delete',async()=>deleteAttachmentDialog(file),'file-card-delete'));$('files-library-grid').append(card);
  }
  if(!result.data?.length){const note=document.createElement('p');note.className='empty-note';note.textContent='Imported files will appear here.';$('files-library-grid').append(note);}syncShellNavigation();
}
const filesBaseLibrary=showLibrary;
showLibrary=async function(...args){filesUi.library=false;await filesBaseLibrary(...args);if(!desktop.library)return;document.querySelector('.library-header')?.append(fileAction('Files',()=>showFilesLibrary()));};
const filesBaseTimeline=upsertTimelineItem;
upsertTimelineItem=function(item){filesBaseTimeline(item);if(item.type!=='userMessage'||item.internal)return;const bubble=itemEls.get(item.id)?.querySelector('.msg-user-bubble');if(!bubble)return;
  for(const file of item.attachments||[]){if(!file.id||bubble.querySelector(`[data-attachment-id="${CSS.escape(file.id)}"]`))continue;const card=fileAction(file.name,()=>openAttachment(file.id),'message-file-card');card.dataset.attachmentId=file.id;bubble.append(card);}
};
const filesReady=desktopReady.then(()=>{
  const status=document.createElement('div');status.id='files-import-status';status.setAttribute('role','status');$('composer-host').prepend(status);
  const input=document.createElement('input');input.id='library-file-input';input.type='file';input.multiple=true;input.hidden=true;input.onchange=async event=>{await addAttachments(event.target.files,{attach:false});event.target.value='';if(filesUi.library)await showFilesLibrary();};document.body.append(input);
  $('attachment-input').accept='.txt,.md,.csv,.tsv,.json,.xml,.yaml,.yml,.log,.rs,.js,.ts,.py,.html,.pdf,.docx,.pptx,.xlsx,image/png,image/jpeg,image/webp';
  fileDraftMaps();restoreDraft();
}).catch(showError);
const filesRequestClose=window.__requestClose;
window.__requestClose=async function(){if(filesUi.importing)await new Promise(resolve=>filesUi.waiters.push(resolve));await filesRequestClose();};
