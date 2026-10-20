// Local Pages use a structured editor and revision-checked Rust writes.
const documentsUi={sessions:new Map(),current:null,navigation:0,layout:'page',returnTo:'library',side:null,selectionToolbar:null,slashMenu:null,proposals:new Map(),find:{query:'',matches:[],index:0},request:0,modalRevision:0};
function docsButton(label,run,className='btn-pill'){
  const button=document.createElement('button');button.type='button';button.textContent=label;button.className=className;
  button.onclick=async()=>{button.disabled=true;try{await run();}catch(error){showError(error);}finally{button.disabled=false;}};return button;
}
function docsText(tag,text,className=''){const element=document.createElement(tag);element.textContent=text;element.className=className;return element;}
function docsCurrent(session){return documentsUi.current===session&&!session.deleted;}
function docsSnapshot(session){
  const title=session.title.value||'Untitled document';
  const value=session.source?{title,format:'markdown',content:session.source.value}:{title,format:'richtext',schemaVersion:EditorBridge.schemaVersion||2,body:session.bridge.getJSON()};
  return {...value,fingerprint:JSON.stringify(value)};
}
function docsDirty(session){return Boolean(session&&(session.bridge||session.source)&&(session.needsMigration||docsSnapshot(session).fingerprint!==session.savedFingerprint));}
function docsStatus(session){
  if(!session?.status)return;
  const message=session.conflict?'Newer version found':session.error?'Could not save':session.saving?'Saving…':docsDirty(session)?'Unsaved changes':'Saved on this device';
  session.status.textContent=message;session.status.dataset.state=session.error||session.conflict?'error':session.saving?'saving':'saved';
  if(session.conflictButton)session.conflictButton.hidden=!session.conflict;
  if(session.errorBox){session.errorBox.textContent=session.error||'';session.errorBox.hidden=!session.error;}
}
function docsQueue(session,work){
  const pending=(session.queue||Promise.resolve()).catch(()=>{}).then(()=>{if(session.deleted)throw new Error('This document was deleted');return work();});
  session.queue=pending;return pending;
}
function docsUpdateMetadata(session,document){
  session.doc=document;if(docsCurrent(session)){desktop.document=document;$('topbar-title').textContent=document.title||'Document';}docsStatus(session);
}
function docsConflict(session,result){
  if(!result.conflict)return false;session.conflict=result.document;session.error='Another window saved a newer revision. Keep your edits as a new document, or review the latest version.';docsStatus(session);
  if(docsCurrent(session))docsShowConflict(session);return true;
}
async function docsFlush(session=documentsUi.current){
  if(!session||session.deleted)return;clearTimeout(session.timer);
  return docsQueue(session,async()=>{
    if(session.conflict)throw new Error(session.error||'Resolve the document conflict before closing');
    session.saving=true;docsStatus(session);
    try{
      while(docsDirty(session)){
        const snapshot=docsSnapshot(session),{fingerprint,...fields}=snapshot;
        const result=await rpcLocalCall('desktop/document/update',{id:session.doc.id,expectedRevision:session.doc.revision,...fields});
        if(docsConflict(session,result))throw new Error(session.error);
        session.savedFingerprint=fingerprint;session.needsMigration=false;session.error='';docsUpdateMetadata(session,result.document);
      }
    }catch(error){session.error=error.message||'Could not save this document';docsStatus(session);throw error;}
    finally{session.saving=false;docsStatus(session);}
  });
}
function docsChanged(session){
  if(session.loading||session.deleted)return;session.error='';clearTimeout(session.timer);docsStatus(session);
  session.timer=setTimeout(()=>docsFlush(session).catch(showError),600);if(docsCurrent(session))docsUpdateTools(session);
}
async function docsImage(id){
  const result=await rpcLocalCall('files/read',{id,includeOriginal:true}),mime=result.attachment?.mimeType,encoded=result.content?.dataBase64;
  if(!['image/png','image/jpeg','image/webp'].includes(mime)||typeof encoded!=='string')throw new Error('The original image is unavailable');
  return 'data:'+mime+';base64,'+encoded;
}
function docsSelection(session){
  if(!session.bridge)return null;const editor=session.bridge.editor,{from,to,empty}=editor.state.selection;
  return {from,to,empty,quote:editor.state.doc.textBetween(from,to,'\n','\uFFFC'),revision:session.doc.revision};
}
function docsCommand(session,name,argument){
  if(!session.bridge||session.conflict||session.locked)return false;const editor=session.bridge.editor,chain=editor.chain().focus();
  const commands={bold:'toggleBold',italic:'toggleItalic',underline:'toggleUnderline',strike:'toggleStrike',code:'toggleCode',bullet:'toggleBulletList',ordered:'toggleOrderedList',task:'toggleTaskList',quote:'toggleBlockquote',codeBlock:'toggleCodeBlock',rule:'setHorizontalRule',undo:'undo',redo:'redo',row:'addRowAfter',column:'addColumnAfter',deleteRow:'deleteRow',deleteColumn:'deleteColumn',deleteTable:'deleteTable',header:'toggleHeaderRow',fitTable:'fitTable'};
  let result=false;
  if(name==='paragraph')result=chain.setParagraph().run();
  else if(name==='heading')result=chain.toggleHeading({level:argument}).run();
  else if(name==='table')result=chain.insertTable({rows:3,cols:3,withHeaderRow:true}).run();
  else if(name==='tableLayout')result=chain.setTableLayout(argument).run();
  else if(commands[name]&&typeof chain[commands[name]]==='function')result=chain[commands[name]]().run();
  docsUpdateTools(session);return result;
}
function docsTool(session,label,command,argument){
  const button=docsButton(label,async()=>docsCommand(session,command,argument),'page-tool');button.title=label;button.setAttribute('aria-label',label);button.dataset.command=command;
  button.textContent=({bold:'B',italic:'I',underline:'U',strike:'S',code:'<>',bullet:'•',ordered:'1.',task:'☑',undo:'↶',redo:'↷'}[command])||label;
  button.onmousedown=event=>event.preventDefault();return button;
}
function docsUpdateTools(session){
  if(!session.bridge||!session.shell?.isConnected)return;const editor=session.bridge.editor;
  const active={bold:'bold',italic:'italic',underline:'underline',strike:'strike',code:'code',bullet:'bulletList',ordered:'orderedList',task:'taskList',quote:'blockquote',codeBlock:'codeBlock'};
  for(const button of session.shell.querySelectorAll('[data-command]')){
    const command=button.dataset.command;if(active[command])button.setAttribute('aria-pressed',String(editor.isActive(active[command])));
    button.disabled=Boolean(session.conflict||session.locked)||(command==='undo'&&!session.bridge.canUndo())||(command==='redo'&&!session.bridge.canRedo());
  }
  session.tableTools.hidden=!editor.isActive('table');
  if(session.tableLayoutSelect&&editor.isActive('table'))session.tableLayoutSelect.value=editor.getAttributes('table').layout||'normal';
  if(session.blockSelect){session.blockSelect.value=editor.isActive('heading')?'h'+editor.getAttributes('heading').level:editor.isActive('codeBlock')?'codeBlock':editor.isActive('blockquote')?'quote':'paragraph';}
  if(docsCurrent(session))docsSelectionTools(session);
}
function docsHideFloating(){if(documentsUi.selectionToolbar)documentsUi.selectionToolbar.hidden=true;if(documentsUi.slashMenu)documentsUi.slashMenu.hidden=true;}
function docsPlaceFloating(element,rect,above=true){
  element.hidden=false;const bounds=element.getBoundingClientRect();let top=above?rect.top-bounds.height-8:rect.bottom+7;
  if(top<10)top=rect.bottom+7;top=Math.min(Math.max(10,top),innerHeight-bounds.height-10);
  element.style.top=top+'px';element.style.left=Math.max(10,Math.min(rect.left,innerWidth-bounds.width-10))+'px';
}
function docsRenderToolbar(session){
  const toolbar=document.createElement('div');toolbar.className='page-format-toolbar';toolbar.setAttribute('role','toolbar');toolbar.setAttribute('aria-label','Document formatting');
  const select=document.createElement('select');select.setAttribute('aria-label','Block type');for(const [value,label] of [['paragraph','Text'],['h1','Heading 1'],['h2','Heading 2'],['h3','Heading 3'],['quote','Quote'],['codeBlock','Code block']]){const option=document.createElement('option');option.value=value;option.textContent=label;select.append(option);}
  select.onchange=()=>select.value.startsWith('h')?docsCommand(session,'heading',Number(select.value.slice(1))):docsCommand(session,select.value);session.blockSelect=select;toolbar.append(select);
  for(const [label,command] of [['Bold','bold'],['Italic','italic'],['Underline','underline'],['Strike','strike'],['Code','code'],['Bullets','bullet'],['Numbered','ordered'],['Checklist','task']])toolbar.append(docsTool(session,label,command));
  toolbar.append(docsButton('Link',()=>docsLinkDialog(session),'page-tool'),docsButton('Insert',event=>docsInsertMenu(session),'page-tool'),docsTool(session,'Undo','undo'),docsTool(session,'Redo','redo'));
  if(session.source)toolbar.hidden=true;session.toolbar=toolbar;return toolbar;
}
function docsRenderTableTools(session){
  const toolbar=document.createElement('div');toolbar.className='page-table-toolbar';toolbar.hidden=true;toolbar.setAttribute('role','toolbar');toolbar.setAttribute('aria-label','Table editing');
  for(const [label,command] of [['Add row','row'],['Add column','column'],['Delete row','deleteRow'],['Delete column','deleteColumn'],['Header row','header'],['Fit columns','fitTable'],['Delete table','deleteTable']])toolbar.append(docsTool(session,label,command));
  const layout=document.createElement('select');layout.setAttribute('aria-label','Table width');for(const value of ['normal','flexible','full']){const option=document.createElement('option');option.value=value;option.textContent=titleCase(value);layout.append(option);}layout.onchange=()=>docsCommand(session,'tableLayout',layout.value);toolbar.append(layout);session.tableLayoutSelect=layout;session.tableTools=toolbar;return toolbar;
}
function docsPageRoot(){
  if(!$('document-page')){const root=document.createElement('section');root.id='document-page';root.hidden=true;root.setAttribute('aria-label','Document editor');$('main-col').append(root);}return $('document-page');
}
function docsBuildSession(documentValue){
  const session={doc:documentValue,shell:document.createElement('div'),bridge:null,source:null,queue:Promise.resolve(),timer:null,saving:false,error:'',conflict:null,loading:true,deleted:false,sideRequest:0};
  const shell=session.shell;shell.className='page-editor-shell';shell.dataset.documentId=documentValue.id;
  const header=document.createElement('header');header.className='page-editor-header';
  const back=docsButton('← Library',()=>docsClosePage(),'page-editor-back');back.setAttribute('aria-label','Back to Library');header.append(back);
  session.status=docsText('span','Opening…','page-editor-status');session.status.setAttribute('role','status');header.append(session.status);
  session.conflictButton=docsButton('Review conflict',()=>docsShowConflict(session,true));session.conflictButton.hidden=true;header.append(session.conflictButton);
  const layout=docsButton('Alongside chat',()=>docsChangeLayout(documentsUi.layout==='page'?'canvas':'page'),'btn-pill');layout.title='Change document layout';session.layoutButton=layout;header.append(layout,docsButton('Comments',()=>docsOpenSide(session,'comments')),docsButton('Suggestions',()=>docsOpenSide(session,'suggestions')),docsButton('History',()=>docsOpenSide(session,'history')),docsButton('Find',()=>docsToggleFind(session)),docsButton('Export',()=>docsExportDialog(session)));
  shell.append(header);
  const toolbar=docsRenderToolbar(session),tableTools=docsRenderTableTools(session);shell.append(toolbar,tableTools);
  session.findBar=docsBuildFind(session);shell.append(session.findBar);
  session.errorBox=docsText('div','','page-editor-error');session.errorBox.setAttribute('role','alert');session.errorBox.hidden=true;shell.append(session.errorBox);
  const main=document.createElement('div');main.className='page-editor-main';shell.append(main);
  const scroll=document.createElement('div');scroll.className='page-document-scroll';scroll.onscroll=docsHideFloating;main.append(scroll);session.scroll=scroll;
  session.title=document.createElement('input');session.title.className='page-document-title';session.title.value=documentValue.title||'Untitled document';session.title.placeholder='Untitled document';session.title.setAttribute('aria-label','Document title');session.title.oninput=()=>docsChanged(session);scroll.append(session.title);
  session.content=document.createElement('div');session.content.className='page-rich-content';scroll.append(session.content);
  session.side=document.createElement('aside');session.side.className='page-side-panel';session.side.hidden=true;session.side.setAttribute('aria-label','Document details');main.append(session.side);
  let imported=null;
  if(documentValue.format!=='richtext'&&documentValue.kind!=='code'){
    try{imported=EditorBridge.importMarkdown(documentValue.content||'');}
    catch{imported={lossless:false,warnings:['The rich editor cannot import this source. Source editing keeps its original text.']};}
  }
  if(documentValue.kind==='code'||imported&&!imported.lossless){
    if(imported&&!imported.lossless){const note=docsText('div','The original Markdown is preserved. This document uses source editing because it contains formatting the rich editor cannot yet preserve. '+(imported.warnings||[]).join(' '),'page-source-warning');scroll.insertBefore(note,session.content);}
    const source=document.createElement('textarea');source.className='page-source-content';source.setAttribute('aria-label',documentValue.kind==='code'?'Code document content':'Markdown source');source.value=documentValue.content||'';source.spellcheck=false;source.oninput=()=>docsChanged(session);session.content.append(source);session.source=source;toolbar.hidden=true;
  }else{
    session.bridge=EditorBridge.create({element:session.content,...(documentValue.format==='richtext'?{json:documentValue.body}:{json:imported.json}),ariaLabel:'Document content',dragHandle:true,resolveImage:docsImage,
      onUpdate:()=>docsChanged(session),onSelectionUpdate:()=>{if(!session.loading)docsUpdateTools(session);},
      onError:error=>{session.error=error.message||'The editor could not apply this change';docsStatus(session);},
      onAttachmentClick:value=>{if(value.open)openAttachment(value.attachmentId,{local:true,allowAttach:false}).catch(showError);},
      onFilesDrop:files=>docsImportFiles(session,files).catch(showError),onWorkBlockAction:action=>docsWorkBlockAction(session,action).catch(showError),
    });
    session.bridge.editor.on('transaction',()=>{if(!session.loading&&docsCurrent(session))docsUpdateTools(session);});
    session.content.addEventListener('keydown',event=>docsEditorKey(session,event),true);
    session.content.addEventListener('keyup',()=>docsSlash(session));
    session.content.addEventListener('focusout',event=>{if(!session.shell.contains(event.relatedTarget)&&!documentsUi.selectionToolbar?.contains(event.relatedTarget))docsHideFloating();});
  }
  session.loading=false;session.needsMigration=Boolean(session.bridge&&documentValue.format!=='richtext');session.savedFingerprint=docsSnapshot(session).fingerprint;docsStatus(session);documentsUi.sessions.set(documentValue.id,session);return session;
}
function docsMountSession(session,layout){
  docsClearPrint();
  const page=docsPageRoot();documentsUi.current=session;documentsUi.layout=layout;desktop.document=session.doc;docsHideFloating();
  if(layout==='page'){
    page.replaceChildren(session.shell);page.hidden=false;document.body.classList.add('in-document');
    baseCloseDrawer();$('inspector-drawer').classList.remove('document-open');hideLibrary();closeSettingsView();closeScheduledView();
  }else{
    page.hidden=true;document.body.classList.remove('in-document');hideLibrary();setViewMode(Boolean(currentThread&&itemEls.size));drawerOpen=true;currentDrawerTab='document';
    $('inspector-drawer').classList.add('document-open');$('inspector-drawer').classList.remove('closed');$('drawer-content').replaceChildren(session.shell);
    if(!$('tab-btn-document')){const tab=document.createElement('button');tab.id='tab-btn-document';tab.className='drawer-tab canvas-tab';tab.textContent='Canvas';tab.onclick=()=>docsMountSession(session,'canvas');document.querySelector('.drawer-tabs').append(tab);}
    document.querySelectorAll('.drawer-tab').forEach(button=>button.classList.toggle('active',button.id==='tab-btn-document'));
  }
  if(session.bridge&&!session.bridge.mounted)session.bridge.mount(session.content);
  session.layoutButton.textContent=layout==='page'?'Alongside chat':'Expand page';$('topbar-title').textContent=session.doc.title||'Document';
  session.layoutButton.setAttribute('aria-label',layout==='page'?'Show document alongside chat':'Expand document to page');docsStatus(session);docsUpdateTools(session);docsSyncNavigation();
}
function docsUnmount(){
  docsClearPrint();
  const session=documentsUi.current;docsHideFloating();if(session?.bridge?.mounted)session.bridge.unmount();
  if(session?.shell.isConnected)session.shell.remove();documentsUi.current=null;documentsUi.layout=null;
  if($('document-page'))$('document-page').hidden=true;document.body.classList.remove('in-document');
  if(session&&typeof setViewMode==='function')setViewMode(Boolean(currentThread&&itemEls.size));
}
async function docsChangeLayout(layout){const session=documentsUi.current;if(!session)return;await docsFlush(session);if(docsCurrent(session))docsMountSession(session,layout);}
async function docsClosePage(){
  const session=documentsUi.current,ticket=++documentsUi.navigation;++desktop.navigation;await flushDocument();if(ticket!==documentsUi.navigation)return;
  docsUnmount();desktop.document=null;baseCloseDrawer();await showLibrary();
}
function docsSyncNavigation(){
  syncShellNavigation();if(documentsUi.current&&documentsUi.layout==='page')document.querySelectorAll('[data-rail-view]').forEach(button=>{const active=button.dataset.railView==='library';button.classList.toggle('active',active);if(active)button.setAttribute('aria-current','page');else button.removeAttribute('aria-current');});
}
const documentsBaseFlush=flushDocument;
flushDocument=async function(){
  do{for(const session of documentsUi.sessions.values())if(!session.deleted)await docsFlush(session);}
  while([...documentsUi.sessions.values()].some(session=>!session.deleted&&docsDirty(session)));
  if(!documentsUi.sessions.size)return documentsBaseFlush();
};
const documentsBaseOpen=openDocument;
openDocument=async function(id,options={}){
  const ticket=++documentsUi.navigation,navigation=++desktop.navigation;await flushDocument();if(ticket!==documentsUi.navigation||navigation!==desktop.navigation)return;
  const result=await rpcLocalCall('desktop/document/read',{id});if(ticket!==documentsUi.navigation||navigation!==desktop.navigation)return;
  await flushDocument();if(ticket!==documentsUi.navigation||navigation!==desktop.navigation)return;
  const layout=options.layout||((desktop.library||!currentThread)?'page':'canvas');
  let session=documentsUi.sessions.get(id);
  if(session&&session.doc.revision<result.document.revision){
    if(docsDirty(session)){docsConflict(session,{conflict:true,document:result.document});throw new Error(session.error);}
    session.bridge?.destroy();documentsUi.sessions.delete(id);session=null;
  }
  const previous=documentsUi.current;if(previous&&previous!==session){previous.bridge?.unmount();previous.shell.remove();}
  if(!session)session=docsBuildSession(result.document);
  docsMountSession(session,layout);session.title.focus();
};
const documentsBaseCreate=createDocument;
createDocument=async function(content='',title='Untitled document',options={}){
  const ticket=documentsUi.navigation,navigation=desktop.navigation;
  const projectId=typeof remoteContext==='function'&&remoteContext()?null:desktop.projectId;
  const result=await rpcLocalCall('desktop/document/create',{title,content,kind:options.kind||'document',projectId});
  if(ticket===documentsUi.navigation&&navigation===desktop.navigation)await openDocument(result.document.id,options);return result.document;
};
function docsModal(title){openCustomModal(title,'<div class="page-owned-modal"></div>');return $('modal-body').querySelector('.page-owned-modal');}
function docsDate(value,seconds=false){return new Date(seconds&&typeof value==='number'?value*1000:value).toLocaleString();}
function docsVersionReason(value){return ({create:'Created',edit:'Edited',restore:'Restored',comment_created:'Comment added',comment_updated:'Comment edited',comment_deleted:'Comment deleted',project_deleted:'Project removed'})[value]||'Saved';}
function docsActions(...buttons){const row=document.createElement('div');row.className='page-panel-actions';row.append(...buttons);return row;}
function docsSelectionValid(session,selection,allowEmpty=false){
  if(!session.bridge||!selection||selection.from>selection.to||selection.from===selection.to&&!allowEmpty)return false;
  try{return session.bridge.editor.state.doc.textBetween(selection.from,selection.to,'\n','\uFFFC')===selection.quote;}catch{return false;}
}
function docsLock(session,locked,method=null){if(locked&&session.locked)throw new Error('Wait for the current document action to finish');session.locked=locked;session.lockOwner=locked?method:null;session.title.disabled=locked;if(session.source)session.source.disabled=locked;session.bridge?.editor.setEditable(!locked);docsUpdateTools(session);}
function docsMutationAllowed(session,method){if(session.locked&&session.lockOwner!==method)throw new Error('Wait for the current document action to finish');}
async function docsMutate(session,method,fields={},validate){
  docsMutationAllowed(session,method);
  await docsFlush(session);
  return docsQueue(session,async()=>{
    docsMutationAllowed(session,method);
    if(validate&&!validate())throw new Error('The document changed. Review the current text and try again.');
    const result=await rpcLocalCall(method,{id:session.doc.id,expectedRevision:session.doc.revision,...fields});
    if(docsConflict(session,result))throw new Error(session.error);
    if(result.document)docsUpdateMetadata(session,result.document);return result;
  });
}
function docsReplaceSession(session,documentValue){
  const layout=documentsUi.layout||'page',active=docsCurrent(session);clearTimeout(session.timer);session.bridge?.destroy();session.deleted=true;session.shell.remove();documentsUi.sessions.delete(session.doc.id);
  const replacement=docsBuildSession(documentValue);if(active)docsMountSession(replacement,layout);return replacement;
}
function docsShowConflict(session,force=false){
  if(!force&&$('modal-backdrop')?.classList.contains('open')){toast('This document has a newer saved version',{label:'Review conflict',run:async()=>docsShowConflict(session,true)});return;}
  const root=docsModal('Document has a newer version');root.append(docsText('p','Your edits are kept in this window. Save a separate copy, or review the latest saved version.'));
  const latest=session.conflict;
  root.append(docsActions(docsButton('Keep my edits as a new document',async()=>{
    if(!root.isConnected||session.deleted)return;const snapshot=docsSnapshot(session),{fingerprint,...fields}=snapshot;
    docsLock(session,true);let result;try{result=await rpcLocalCall('desktop/document/create',{...fields,title:snapshot.title+' (my edits)',kind:session.doc.kind});}finally{docsLock(session,false);}
    if(!root.isConnected||session.deleted){toast('Your edits were saved as a new document');return;}
    const layout=documentsUi.layout||'page';session.conflict=null;session.error='';session.deleted=true;clearTimeout(session.timer);session.bridge?.destroy();session.shell.remove();documentsUi.sessions.delete(session.doc.id);
    if(documentsUi.current===session){documentsUi.current=null;desktop.document=null;}closeModal();await openDocument(result.document.id,{layout});
  }),docsButton('Review latest saved version',()=>{
    if(!root.isConnected)return;root.replaceChildren(docsText('p','Saved revision '+latest.revision+' · '+latest.title));
    const preview=docsText('pre',latest.content||'This document is empty.','page-proposal-text');root.append(preview,docsActions(docsButton('Keep editing my copy',()=>closeModal()),docsButton('Discard my edits and load this version',async()=>{
      if(!root.isConnected||session.deleted)return;const result=await rpcLocalCall('desktop/document/read',{id:session.doc.id});if(!root.isConnected||session.deleted)return;
      docsReplaceSession(session,result.document);closeModal();
    },'btn-pill danger')));
  }),docsButton('Keep editing',()=>closeModal())));
}
async function docsExportDialog(session){
  const ticket=documentsUi.modalRevision;await docsFlush(session);if(!docsCurrent(session)||ticket!==documentsUi.modalRevision)return;const root=docsModal('Export document');root.append(docsText('p','Download or copy the current saved revision. Word preserves text formatting, lists, tables and images. JSON includes the full structure and original Markdown.'));
  const status=docsText('p','','settings-sub');status.setAttribute('role','status');
  for(const [format,label] of [['docx','Download Word (.docx)'],['markdown','Download Markdown'],['json','Download JSON']])root.append(docsButton(label,async()=>{
    await docsFlush(session);if(!root.isConnected)return;const result=await rpcLocalCall('desktop/document/export',{id:session.doc.id,format,revision:session.doc.revision});
    if(root.isConnected){docsDownloadExport(result);status.textContent=(result.warnings||[]).join(' ');}
  }));
  root.append(status);
  root.append(docsButton('Print / Save as PDF',()=>docsPrint(session)));
  root.append(docsActions(docsButton('Copy as Markdown',async()=>{await docsFlush(session);const result=await rpcLocalCall('desktop/document/export',{id:session.doc.id,format:'markdown'});if(!root.isConnected)return;await navigator.clipboard.writeText(result.content);toast('Markdown copied');}),docsButton('Delete document',()=>docsDelete(session),'btn-pill danger')));
}
function docsDownloadExport(result){
  let content;
  if(typeof result.dataBase64==='string')content=Uint8Array.from(atob(result.dataBase64),character=>character.charCodeAt(0));
  else if(typeof result.content==='string')content=result.content;
  else throw new Error('The exported file is unavailable');
  const url=URL.createObjectURL(new Blob([content],{type:result.mimeType||'application/octet-stream'}));
  const link=document.createElement('a');link.href=url;link.download=String(result.filename||'document').replace(/[\\/\u0000-\u001f]/g,'_');link.click();setTimeout(()=>URL.revokeObjectURL(url),1000);
}
function docsClearPrint(invalidate=true){if(invalidate)documentsUi.printRequest=(documentsUi.printRequest||0)+1;document.getElementById('page-print-root')?.remove();document.body.classList.remove('document-printing');}
async function docsPreparePrint(session=documentsUi.current){
  if(!session||!docsCurrent(session))throw new Error('Open a document to print');
  const request=documentsUi.printRequest=(documentsUi.printRequest||0)+1,navigation=documentsUi.navigation;await docsFlush(session);
  if(!docsCurrent(session)||navigation!==documentsUi.navigation||request!==documentsUi.printRequest)return null;
  const root=document.createElement('article');root.id='page-print-root';root.className='page-rich-content';root.append(docsText('h1',session.doc.title||'Document','page-print-title'));
  if(session.source)root.append(docsText('pre',session.doc.content||'','page-print-source'));
  else{
    const content=session.content.cloneNode(true);content.querySelectorAll('.editor-drag-handle,.column-resize-handle,.ProseMirror-separator,.ProseMirror-trailingBreak,button').forEach(node=>node.remove());
    content.querySelectorAll('[id],[contenteditable]').forEach(node=>{node.removeAttribute('id');node.removeAttribute('contenteditable');});
    for(const figure of content.querySelectorAll('.editor-attachment-image')){
      const picture=figure.querySelector('img');if(!picture)throw new Error('A document image could not be printed');
      picture.src=await docsImage(figure.dataset.attachmentId);
    }
    root.append(content);
  }
  if(!docsCurrent(session)||navigation!==documentsUi.navigation||request!==documentsUi.printRequest)return null;
  const frames=Array.from(root.querySelectorAll('iframe')).map(frame=>new Promise((resolve,reject)=>{const timeout=setTimeout(()=>reject(new Error('The visualization did not become ready to print')),10000);frame.onload=()=>{clearTimeout(timeout);resolve();};frame.onerror=()=>{clearTimeout(timeout);reject(new Error('The visualization could not be printed'));};}));
  docsClearPrint(false);document.body.append(root);document.body.classList.add('document-printing');
  try{
    await Promise.all(frames);
    if(document.fonts?.ready)await document.fonts.ready;
    for(const picture of root.querySelectorAll('img'))if(picture.decode)await picture.decode();
    if(!docsCurrent(session)||navigation!==documentsUi.navigation||request!==documentsUi.printRequest){if(root.isConnected)docsClearPrint(false);return null;}
    return root;
  }catch(error){if(root.isConnected)docsClearPrint(false);if(!docsCurrent(session)||navigation!==documentsUi.navigation||request!==documentsUi.printRequest)return null;throw error;}
}
async function docsPrint(session=documentsUi.current){
  const prepared=await docsPreparePrint(session);if(!prepared?.isConnected)return;
  if(window.__mcodexNative)rpcSend({__window:'print'});
  else try{window.print();}finally{docsClearPrint();}
}
// WKWebView can emit afterprint while its native print sheet still uses the
// snapshot. Keep that hidden snapshot until the next edit/navigation/print.
window.addEventListener('afterprint',()=>{if(!window.__mcodexNative)docsClearPrint();});
window.__documentPrintFailed=message=>{docsClearPrint();showError(new Error(message));};
function docsDelete(session){
  const root=docsModal('Delete document?');root.append(docsText('p','Permanently delete “'+session.title.value+'” and its saved revisions from this device?'));
  root.append(docsActions(docsButton('Cancel',()=>closeModal()),docsButton('Delete document',async()=>{
    if(!root.isConnected||!docsCurrent(session))return;docsLock(session,true,'desktop/document/delete');
    try{await docsMutate(session,'desktop/document/delete',{},()=>root.isConnected&&docsCurrent(session));session.deleted=true;clearTimeout(session.timer);session.bridge?.destroy();documentsUi.sessions.delete(session.doc.id);if(documentsUi.current===session){docsUnmount();desktop.document=null;baseCloseDrawer();if(root.isConnected)closeModal();await showLibrary();}toast('Document deleted');}
    finally{if(!session.deleted)docsLock(session,false);}
  },'btn-pill danger')));
}
async function docsOpenSide(session,kind){
  if(!docsCurrent(session))return;docsHideFloating();const ticket=++session.sideRequest;
  session.sideKind=kind;session.side.hidden=false;session.side.replaceChildren(docsText('h2',titleCase(kind)),docsButton('Close',()=>{session.side.hidden=true;session.sideKind=null;++session.sideRequest;}));
  const body=document.createElement('div');session.side.append(body);body.append(docsText('p','Loading…'));
  const valid=()=>docsCurrent(session)&&session.sideRequest===ticket&&body.isConnected;
  try{
    await docsFlush(session);if(!valid())return;
    if(kind==='comments'){
      const result=await rpcLocalCall('desktop/document/comments/list',{id:session.doc.id});if(!valid())return;body.replaceChildren(docsText('p','Comments are saved on this device.'));
      body.append(docsButton('Add comment',()=>docsAddComment(session,docsSelection(session))));
      const comments=result.items||[];if(!comments.length)body.append(docsText('p','Select text to attach a comment, or add a document comment.'));
      for(const comment of comments.filter(item=>!item.parentId))docsRenderComment(session,body,comment,comments.filter(item=>item.parentId===comment.id));
    }else if(kind==='history'){
      const result=await rpcLocalCall('desktop/document/history/list',{id:session.doc.id,limit:100});if(!valid())return;body.replaceChildren();
      const retention=result.history;body.append(docsText('p','Saved revisions on this device. '+(retention.prunedCount?retention.prunedCount+' older revisions were pruned.':'Up to '+retention.maxVersions+' revisions are kept within the local size limit.')));
      for(const version of result.items||[]){const card=document.createElement('div');card.className='page-version-card';card.append(docsText('strong',version.title),docsText('small','Revision '+version.revision+' · '+docsDate(version.savedAt)),docsText('small',docsVersionReason(version.reason)),docsButton('Review revision',()=>docsReviewVersion(session,version.revision)));body.append(card);}
    }else if(kind==='suggestions'){
      const result=await rpcLocalCall('desktop/document/ai/list',{id:session.doc.id});if(!valid())return;body.replaceChildren();
      if(!result.data?.length)body.append(docsText('p','Select text, then choose Ask for change. Suggestions need your review before they change the document.'));
      for(const proposal of result.data||[]){documentsUi.proposals.set(proposal.proposalId,proposal);docsRenderProposal(session,body,proposal);}
    }
  }catch(error){if(valid())body.replaceChildren(docsText('p',error.message));}
}
function docsRenderComment(session,container,comment,replies=[]){
  const card=document.createElement('article');card.className='page-comment-card';card.append(docsText('strong','You'+(comment.resolved?' · Resolved':'')),docsText('time',docsDate(comment.createdAt)));
  if(comment.anchor){card.append(docsText('blockquote',comment.anchor.quote));card.append(docsText('small',comment.orphaned?'The document changed. This comment keeps its original quote.':'Attached to selected text.'));
    if(!comment.orphaned)card.append(docsButton('Show selected text',()=>{if(!session.bridge||!docsSelectionValid(session,comment.anchor))throw new Error('The original selection is unavailable');session.bridge.editor.chain().focus().setTextSelection({from:comment.anchor.from,to:comment.anchor.to}).run();}));
  }
  card.append(docsText('p',comment.body));
  const actions=docsActions(docsButton(comment.resolved?'Reopen':'Resolve',async()=>{await docsMutate(session,'desktop/document/comments/update',{commentId:comment.id,resolved:!comment.resolved});await docsOpenSide(session,'comments');}),docsButton('Edit',()=>docsEditComment(session,comment)),docsButton('Delete',async()=>{await docsMutate(session,'desktop/document/comments/delete',{commentId:comment.id});await docsOpenSide(session,'comments');}));
  if(!comment.resolved)actions.prepend(docsButton('Reply',()=>docsAddComment(session,null,comment.id)));card.append(actions);
  if(typeof docsCommentAI==='function')actions.append(docsButton('Ask ChatGPT',()=>docsCommentAI(session,comment)));
  for(const reply of replies){const sub=document.createElement('div');sub.className='page-comment-reply';sub.append(docsText('strong','You'),docsText('p',reply.body),docsActions(docsButton('Edit reply',()=>docsEditComment(session,reply)),docsButton('Delete reply',async()=>{await docsMutate(session,'desktop/document/comments/delete',{commentId:reply.id});await docsOpenSide(session,'comments');})));card.append(sub);}
  container.append(card);
}
function docsAddComment(session,selection,parentId){
  if(!docsCurrent(session))return;const selected=selection&&!selection.empty?{...selection}:null,bodyAtOpen=session.bridge?.editor.state.doc;
  const root=docsModal(parentId?'Reply':'Add comment');if(selected)root.append(docsText('blockquote',selected.quote));
  const input=document.createElement('textarea');input.className='modal-input';input.rows=4;input.maxLength=16000;input.setAttribute('aria-label',parentId?'Reply text':'Comment text');root.append(input,docsActions(docsButton('Cancel',()=>closeModal()),docsButton('Save comment',async()=>{
    if(!input.value.trim())throw new Error('Enter a comment');if(!root.isConnected||!docsCurrent(session))return;
    await docsFlush(session);if(!root.isConnected||!docsCurrent(session))return;
    const anchor=selected?{revision:session.doc.revision,from:selected.from,to:selected.to,quote:selected.quote}:undefined;
    await docsMutate(session,'desktop/document/comments/create',{body:input.value,...(anchor?{anchor}:{}),...(parentId?{parentId}:{})},()=>root.isConnected&&docsCurrent(session)&&(!selected||(bodyAtOpen===session.bridge.editor.state.doc&&docsSelectionValid(session,selected))));
    if(root.isConnected){closeModal();await docsOpenSide(session,'comments');}
  },'btn-pill primary')));input.focus();
}
function docsEditComment(session,comment){
  const root=docsModal('Edit comment'),input=document.createElement('textarea');input.className='modal-input';input.rows=4;input.maxLength=16000;input.value=comment.body;input.setAttribute('aria-label','Comment text');root.append(input,docsActions(docsButton('Cancel',()=>closeModal()),docsButton('Save',async()=>{
    if(!input.value.trim())throw new Error('Enter a comment');await docsMutate(session,'desktop/document/comments/update',{commentId:comment.id,body:input.value},()=>root.isConnected&&docsCurrent(session));if(root.isConnected){closeModal();await docsOpenSide(session,'comments');}
  },'btn-pill primary')));
}
async function docsReviewVersion(session,revision){
  const modal=documentsUi.modalRevision,navigation=documentsUi.navigation;
  const result=await rpcLocalCall('desktop/document/history/read',{id:session.doc.id,revision});if(!docsCurrent(session)||modal!==documentsUi.modalRevision||navigation!==documentsUi.navigation)return;
  const root=docsModal('Revision '+revision);root.append(docsText('p',result.version.title));const preview=document.createElement('div');preview.className='msg-agent page-version-preview';preview.innerHTML=renderMarkdown(result.version.content)||'This revision is empty.';root.append(preview,docsActions(docsButton('Close',()=>closeModal()),docsButton('Restore this revision',async()=>{
    if(!root.isConnected||!docsCurrent(session))return;docsLock(session,true,'desktop/document/restore');
    try{const restored=await docsMutate(session,'desktop/document/restore',{revision},()=>root.isConnected&&docsCurrent(session));const replacement=docsReplaceSession(session,restored.document);if(root.isConnected)closeModal();if(docsCurrent(replacement))await docsOpenSide(replacement,'history');toast('Revision restored as a new saved revision');}
    finally{if(!session.deleted)docsLock(session,false);}
  },'btn-pill primary')));
}
async function docsAskChange(session,selection,purpose='change',initialInstructions=''){
  if(!docsCurrent(session)||!docsSelectionValid(session,selection,purpose!=='change'))throw new Error('Select the text you want to change');
  const selected={...selection},bodyAtOpen=session.bridge.editor.state.doc,modal=documentsUi.modalRevision;await docsFlush(session);if(!docsCurrent(session)||bodyAtOpen!==session.bridge.editor.state.doc||modal!==documentsUi.modalRevision)return;
  const revision=session.doc.revision,root=docsModal(purpose==='visualize'?'Create visualization':purpose==='generate'?'Generate content':'Ask for change');root.append(docsText('blockquote',selected.quote,'page-proposal-text'),docsText('p',purpose==='change'?'The model receives this selection and its selected files. Review its suggestion before applying it.':'Describe what to create. Review the draft before inserting it into this page.'));
  const instructions=document.createElement('textarea');instructions.className='modal-input';instructions.rows=3;instructions.maxLength=4000;instructions.placeholder='Describe the change';instructions.setAttribute('aria-label','Change instructions');
  instructions.value=initialInstructions;
  const model=document.createElement('input');model.className='modal-input';model.setAttribute('aria-label','Document model');model.placeholder='Model ID for your local account';model.setAttribute('list','page-model-options');
  const models=document.createElement('datalist');models.id='page-model-options';
  const note=docsText('p','Checking the local account…'),submit=docsButton('Request suggestion',async()=>{
    if(!root.isConnected||!docsCurrent(session))return;if(!instructions.value.trim()||!model.value.trim())throw new Error('Enter instructions and a model');
    await docsFlush(session);if(!root.isConnected||!docsCurrent(session))return;
    if(session.doc.revision!==revision||bodyAtOpen!==session.bridge.editor.state.doc||!docsSelectionValid(session,selected,purpose!=='change'))throw new Error('The document changed. Select the text again.');
    const result=await rpcLocalCall('desktop/document/ai/propose',{id:session.doc.id,expectedRevision:revision,selection:{from:selected.from,to:selected.to,quote:selected.quote},instructions:instructions.value,model:model.value.trim(),reasoningEffort:'auto',...(purpose==='change'?{}:{purpose})});
    documentsUi.proposals.set(result.proposal.proposalId,result.proposal);if(root.isConnected){closeModal();if(docsCurrent(session))await docsOpenSide(session,'suggestions');}else toast('The suggestion is available in this document');
  },'btn-pill primary');submit.disabled=true;root.append(instructions,model,models,note,docsActions(docsButton('Cancel',()=>closeModal()),submit));instructions.focus();
  const results=await Promise.allSettled([rpcLocalCall('desktop/document/ai/capabilities'),rpcLocalCall('model/list')]);if(!root.isConnected)return;
  const capability=results[0].status==='fulfilled'?results[0].value:null,catalog=results[1].status==='fulfilled'?results[1].value:null;
  for(const item of catalog?.data||[]){const option=document.createElement('option');option.value=item.model;option.label=item.displayName||item.model;models.append(option);}
  model.value=capability?.defaultModel||catalog?.defaultModel||'';note.textContent=capability?.available?'Uses the account connected on this computer.':capability?.reason||'Could not check the document suggestion service';submit.disabled=!capability?.available;
}
function docsRenderProposal(session,container,proposal){
  const card=document.createElement('article');card.className='page-version-card';const stale=proposal.stale||proposal.baseRevision!==session.doc.revision;
  card.append(docsText('strong',titleCase(proposal.status)+(stale?' · Earlier revision':'')),docsText('small',docsDate(proposal.createdAt,true)+' · '+proposal.model),docsText('p',proposal.instructions));
  if(proposal.error)card.append(docsText('p',proposal.error));
  if(proposal.status==='ready')card.append(docsButton('Review suggestion',()=>docsReviewProposal(session,proposal.proposalId)));
  if(['queued','running'].includes(proposal.status))card.append(docsButton('Cancel request',async()=>{await rpcLocalCall('desktop/document/ai/cancel',{proposalId:proposal.proposalId});if(docsCurrent(session))await docsOpenSide(session,'suggestions');}));
  container.append(card);
}
async function docsReviewProposal(session,proposalId){
  const modal=documentsUi.modalRevision,navigation=documentsUi.navigation;
  const result=await rpcLocalCall('desktop/document/ai/read',{proposalId});if(!docsCurrent(session)||modal!==documentsUi.modalRevision||navigation!==documentsUi.navigation)return;const proposal=result.proposal;documentsUi.proposals.set(proposalId,proposal);
  if(proposal.status!=='ready')throw new Error(proposal.error||'This suggestion is not ready');
  const bodyAtOpen=session.bridge?.editor.state.doc,root=docsModal('Review suggested change');
  root.append(docsText('h3',proposal.purpose&&proposal.purpose!=='change'?'Insertion point':'Original selection'),docsText('pre',proposal.selection.quote||'At the cursor position when this request started.','page-proposal-text'),docsText('h3','Suggested replacement'));
  const visual=proposal.purpose==='visualize',inserting=proposal.purpose&&proposal.purpose!=='change';
  const preview=document.createElement(visual?'iframe':'div');preview.className=visual?'page-visualization-preview':'msg-agent page-proposal-preview';
  const visualHTML=visual?proposal.proposedMarkdown.replace(/^```(?:html)?\s*\n/,'').replace(/\n```\s*$/,''):'';
  if(visual){preview.title='Suggested visualization';preview.setAttribute('sandbox','allow-scripts');preview.srcdoc=EditorBridge.sandboxHTML(visualHTML);}else preview.innerHTML=renderMarkdown(proposal.proposedMarkdown);root.append(preview);
  const imported=visual?{lossless:true,json:{content:[docsWorkNode('visualize',{html:visualHTML,title:'Visualization'})]}}:EditorBridge.importMarkdown(proposal.proposedMarkdown),stale=proposal.stale||proposal.baseRevision!==session.doc.revision||!docsSelectionValid(session,proposal.selection,Boolean(inserting));
  const note=docsText('p',stale?'The document changed after this request. This suggestion cannot replace its old selection.':!imported.lossless?'This suggestion contains formatting the editor cannot preserve. You can copy its Markdown.':inserting?'Applying inserts this draft at the original cursor position. Undo can restore the previous page.':'Applying replaces only the selected range. Undo can restore the previous text.');root.append(note);
  const apply=docsButton('Apply suggestion',async()=>{
    if(!root.isConnected||!docsCurrent(session))return;await docsFlush(session);if(!root.isConnected||!docsCurrent(session))return;
    const latest=await rpcLocalCall('desktop/document/ai/read',{proposalId});if(!root.isConnected||!docsCurrent(session))return;
    if(latest.proposal.status!=='ready'||latest.proposal.stale||session.doc.revision!==proposal.baseRevision||bodyAtOpen!==session.bridge?.editor.state.doc||!docsSelectionValid(session,proposal.selection,Boolean(inserting)))throw new Error('The document changed. Request a new suggestion for the current text.');
    if(!imported.lossless)throw new Error('Copy the Markdown to preserve this suggestion');
    const changed=session.bridge.editor.chain().focus().insertContentAt({from:proposal.selection.from,to:proposal.selection.to},imported.json.content).run();if(!changed)throw new Error('This replacement does not fit the selected block. Copy the Markdown or select a different range.');
    await docsFlush(session);if(root.isConnected){closeModal();await docsOpenSide(session,'suggestions');toast('Suggestion applied');}
  },'btn-pill primary');apply.disabled=stale||!imported.lossless||!session.bridge;
  root.append(docsActions(docsButton('Keep document unchanged',()=>closeModal()),docsButton('Copy suggestion Markdown',async()=>{await navigator.clipboard.writeText(proposal.proposedMarkdown);toast('Suggestion copied');}),apply));
}
// Every document remains local, including a page opened alongside an SSH task.
async function docsLeave(){
  const ticket=++documentsUi.navigation;++desktop.navigation;await flushDocument();if(ticket!==documentsUi.navigation)return false;
  docsUnmount();desktop.document=null;$('inspector-drawer')?.classList.remove('document-open');baseCloseDrawer();return true;
}
for(const name of ['newThread','openThread','switchMode','openSettingsView','openScheduledView','showLibrary','showFilesLibrary','showImagesLibrary','remoteSelect']){
  const original=window[name];if(typeof original!=='function')continue;
  window[name]=async function(...args){if(!await docsLeave())return;return original.apply(this,args);};
}
const documentsBaseCloseDrawer=closeDrawer;
closeDrawer=function(){
  if(!documentsUi.current)return documentsBaseCloseDrawer();
  return docsLeave().catch(showError);
};
const documentsBaseRenderDrawer=renderDrawerContent;
renderDrawerContent=async function(){if(currentDrawerTab==='document'&&documentsUi.current){docsMountSession(documentsUi.current,'canvas');return;}if(documentsUi.current&&!await docsLeave())return;return documentsBaseRenderDrawer();};
const documentsBaseNotification=handleServerNotification;
handleServerNotification=function(method,params){
  documentsBaseNotification(method,params);
  if(method==='desktop/document/ai/updated'&&params.proposal){const proposal=params.proposal,prior=documentsUi.proposals.get(proposal.proposalId);if(prior&&prior.revision>proposal.revision)return;documentsUi.proposals.set(proposal.proposalId,proposal);
    const session=documentsUi.current;if(session?.doc.id===proposal.id&&session.sideKind==='suggestions'){clearTimeout(session.proposalTimer);session.proposalTimer=setTimeout(()=>{if(docsCurrent(session)&&session.sideKind==='suggestions')docsOpenSide(session,'suggestions').catch(showError);},120);}
  }
};
const documentsBaseOpenModal=openCustomModal;
openCustomModal=function(...args){++documentsUi.modalRevision;docsHideFloating();return documentsBaseOpenModal(...args);};
const documentsBaseCloseModal=closeModal;
closeModal=function(...args){++documentsUi.modalRevision;return documentsBaseCloseModal(...args);};
window.addEventListener('beforeunload',event=>{if([...documentsUi.sessions.values()].some(session=>!session.deleted&&(session.saving||docsDirty(session)))){event.preventDefault();event.returnValue='';}});
document.addEventListener('keydown',event=>{
  const session=documentsUi.current;if(!session||event.isComposing)return;
  if((event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==='p'){event.preventDefault();event.stopImmediatePropagation();docsPrint(session).catch(showError);return;}
  if((event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==='f'&&!$('modal-backdrop')?.classList.contains('open')){event.preventDefault();event.stopImmediatePropagation();docsToggleFind(session,true);}
},true);
