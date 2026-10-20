// Pages tools operate on one captured ProseMirror document. Rust owns files and persistence.
let docsToolSerial=0;
function docsToolSnapshot(session){
  if(!docsCurrent(session)||!session.bridge||session.bridge.destroyed||session.conflict||session.locked)return null;
  const editor=session.bridge.editor,selection=docsSelection(session);return {session,bridge:session.bridge,id:session.doc.id,doc:editor.state.doc,navigation:documentsUi.navigation,selection,node:editor.state.selection.node||null};
}
function docsToolCurrent(snapshot){return Boolean(snapshot&&docsCurrent(snapshot.session)&&snapshot.session.bridge===snapshot.bridge&&!snapshot.bridge.destroyed&&!snapshot.session.conflict&&!snapshot.session.locked&&snapshot.session.doc.id===snapshot.id&&snapshot.navigation===documentsUi.navigation&&snapshot.bridge.editor.state.doc===snapshot.doc);}
function docsToolSelection(snapshot){if(!docsToolCurrent(snapshot))return null;const editor=snapshot.bridge.editor;const selected=snapshot.selection;if(snapshot.node)editor.commands.setNodeSelection(selected.from);else editor.commands.setTextSelection({from:selected.from,to:selected.to});return docsSelection(snapshot.session);}
function docsToolModalOwns(owner){return Boolean(owner&&owner.root.isConnected&&$('modal-backdrop')?.classList.contains('open')&&$('modal-body')?.contains(owner.root));}
function docsToolOwns(owner){return docsToolModalOwns(owner)&&docsToolCurrent(owner.snapshot);}
function docsToolDialog(snapshot,title){
  if(!docsToolCurrent(snapshot))return null;docsHideFloating();const id='document-tool-'+(++docsToolSerial);openCustomModal(title,'<div id="'+id+'" class="document-tool-dialog"></div>');return {snapshot,root:$(id),serial:docsToolSerial};
}
function docsToolClose(owner){if(!docsToolModalOwns(owner))return;closeModal();if(docsToolCurrent(owner.snapshot))owner.snapshot.bridge.focus();}
function docsToolError(owner,error,allowChanged=false){if(!docsToolModalOwns(owner)||(!docsToolCurrent(owner.snapshot)&&!(allowChanged&&docsCurrent(owner.snapshot.session)&&owner.snapshot.navigation===documentsUi.navigation)))return;let note=owner.root.querySelector('[data-docs-error]');if(!note){note=docsText('p','','document-tool-error');note.dataset.docsError='true';note.setAttribute('role','alert');owner.root.append(note);}note.textContent=error?.message||String(error);}
function docsToolLabel(label,control){const wrap=document.createElement('label');wrap.className='document-tool-field';wrap.append(docsText('span',label),control);return wrap;}
function docsToolInput(label,value='',type='text'){const input=document.createElement('input');input.type=type;input.value=String(value??'');input.setAttribute('aria-label',label);return input;}
function docsToolButton(label,run,className='btn-pill'){const button=docsButton(label,run,className);button.onmousedown=event=>event.preventDefault();return button;}
function docsToolRect(snapshot){
  try{const editor=snapshot.bridge.editor;if(snapshot.node){const node=editor.view.nodeDOM(snapshot.selection.from);if(node?.getBoundingClientRect)return node.getBoundingClientRect();}const a=editor.view.coordsAtPos(snapshot.selection.from),b=editor.view.coordsAtPos(snapshot.selection.to);return {top:Math.min(a.top,b.top),bottom:Math.max(a.bottom,b.bottom),left:Math.min(a.left,b.left),right:Math.max(a.right,b.right)};}catch{return null;}
}
function docsToolsTableLayout(session){
  if(!session.bridge?.mounted||!session.content)return;const editor=session.bridge.editor,tables=[];editor.state.doc.descendants((node,pos)=>{if(node.type.name!=='table')return;const dom=editor.view.nodeDOM(pos),table=dom?.matches?.('table')?dom:dom?.querySelector?.('table');if(table){table.dataset.layout=node.attrs.layout||'normal';tables.push(table);}});for(const table of tables){
    const layout=table.dataset.layout,wrapper=table.closest('.tableWrapper');table.style.width=layout==='normal'?'auto':'100%';table.style.maxWidth=layout==='normal'?'100%':'none';table.style.tableLayout=layout==='full'?'fixed':'auto';
    if(wrapper){wrapper.style.width='';wrapper.style.marginLeft='';if(layout==='full'&&session.scroll){const style=getComputedStyle(session.scroll),available=session.scroll.clientWidth-parseFloat(style.paddingLeft||0)-parseFloat(style.paddingRight||0),normal=session.content.clientWidth;if(available>normal&&normal>0){wrapper.style.width=available+'px';wrapper.style.marginLeft=-(available-normal)/2+'px';}}}
  }
}
function docsSelectionTools(session){
  if(!docsCurrent(session))return;docsToolsTableLayout(session);if(session._docsComposing){docsHideFloating();return;}if(session._docsFind&&!session.findBar?.hidden&&session._docsFind.doc!==(session.bridge?.editor.state.doc||session.source?.value))docsFindRefresh(session,false);
  docsSlash(session);let toolbar=documentsUi.selectionToolbar;const snapshot=docsToolSnapshot(session);
  if(!snapshot||snapshot.selection.empty||$('modal-backdrop')?.classList.contains('open')){if(toolbar)toolbar.hidden=true;return;}
  const editor=snapshot.bridge.editor;if(!editor.isFocused&&!toolbar?.contains(document.activeElement)){if(toolbar)toolbar.hidden=true;return;}
  const rect=docsToolRect(snapshot);if(!rect){if(toolbar)toolbar.hidden=true;return;}
  if(!toolbar){toolbar=document.createElement('div');toolbar.className='page-selection-toolbar';toolbar.setAttribute('role','toolbar');toolbar.setAttribute('aria-label','Selection formatting');document.body.append(toolbar);documentsUi.selectionToolbar=toolbar;}
  toolbar.replaceChildren();toolbar._docsSession=session;
  toolbar.append(docsToolButton('Ask for change',()=>{const selection=docsToolSelection(snapshot);if(selection)return docsAskChange(session,selection);}),docsToolButton('Comment',()=>{const selection=docsToolSelection(snapshot);if(selection)return docsAddComment(session,selection);}));
  const type=snapshot.node?.type.name;
  if(type==='image'||type==='fileAttachment'){
    toolbar.append(docsToolButton(type==='image'?'Replace / image options':'Replace file',()=>docsPickAttachments(session,type==='image'?'image':'file')));
    if(type==='image')toolbar.append(docsToolButton('Edit image',()=>{if(docsToolCurrent(snapshot))return openImageEditor({kind:'edit',imageIds:[snapshot.node.attrs.attachmentId]});}));
    toolbar.append(docsToolButton('Open',()=>{if(docsToolCurrent(snapshot))return openAttachment(snapshot.node.attrs.attachmentId,{local:true,allowAttach:false});}),docsToolButton('Delete',()=>{if(docsToolSelection(snapshot))snapshot.bridge.editor.chain().focus().deleteSelection().run();}));
  }else if(type==='workBlock'){
    toolbar.append(docsToolButton('Edit block',()=>docsWorkBlockAction(session,{action:'edit'})),docsToolButton('Delete block',()=>{if(docsToolSelection(snapshot))snapshot.bridge.editor.chain().focus().deleteSelection().run();}));
  }else if(type==='inlineMath'||type==='blockMath')toolbar.append(docsToolButton('Edit formula',()=>docsInsertMath(session)));
  else for(const [label,command] of [['B','bold'],['I','italic'],['U','underline'],['S','strike'],['<>','code']]){
    const button=docsToolButton(label,()=>{if(docsToolSelection(snapshot))docsCommand(session,command);});button.setAttribute('aria-label',command);button.setAttribute('aria-pressed',String(editor.isActive(command)));toolbar.append(button);
  }
  if(!snapshot.node)toolbar.append(docsToolButton('Link',()=>docsLinkDialog(session)));
  docsPlaceFloating(toolbar,rect,true);
}
function docsToolChain(snapshot,range){
  if(!docsToolCurrent(snapshot))return null;const chain=snapshot.bridge.editor.chain().focus();if(range)return chain.setTextSelection(range.to).deleteRange(range);return snapshot.node?chain.setNodeSelection(snapshot.selection.from):chain.setTextSelection({from:snapshot.selection.from,to:snapshot.selection.to});
}
function docsToolApply(owner,action){
  if(!docsToolModalOwns(owner))return false;if(!docsToolCurrent(owner.snapshot)){docsToolError(owner,new Error('The document changed. Close this dialog and try again.'),true);return false;}try{const result=action();if(!result){docsToolError(owner,new Error('This change is not available at this position.'));return false;}closeModal();owner.snapshot.bridge.focus();return true;}catch(error){docsToolError(owner,error);return false;}
}
function docsLinkDialog(session,context={}){
  const snapshot=context.snapshot||docsToolSnapshot(session),owner=docsToolDialog(snapshot,'Link');if(!owner)return;
  const editor=snapshot.bridge.editor,old=editor.getAttributes('link').href||'',href=docsToolInput('Link URL',old),label=docsToolInput('Link text',snapshot.selection.quote||'');href.placeholder='https://example.com';
  owner.root.append(docsToolLabel('URL',href));const needsLabel=snapshot.selection.empty&&!old;if(needsLabel)owner.root.append(docsToolLabel('Text',label));
  owner.root.append(docsText('p','Use an http, https, or mailto link.','settings-sub'));const actions=document.createElement('div');actions.className='document-tool-actions';owner.root.append(actions);
  actions.append(docsButton('Cancel',()=>docsToolClose(owner)),docsButton(old?'Update link':'Insert link',()=>{
    if(!docsToolOwns(owner))return;const value=href.value.trim();if(!EditorBridge.safeLink(value)){docsToolError(owner,new Error('Enter a valid http, https, or mailto URL without credentials.'));return;}
    docsToolApply(owner,()=>{let chain=docsToolChain(snapshot,context.range);if(needsLabel||context.range)return chain.insertContent({type:'text',text:label.value.trim()||value,marks:[{type:'link',attrs:{href:value}}]}).run();if(snapshot.selection.empty)chain=chain.extendMarkRange('link');return chain.setLink({href:value}).run();});
  },'btn-pill primary'));
  if(old)actions.prepend(docsButton('Remove link',()=>docsToolApply(owner,()=>docsToolChain(snapshot).extendMarkRange('link').unsetLink().run()),'btn-pill danger'));
  href.focus();
}
const docsBlockChoices=[['Text','paragraph'],['Heading 1','heading',1],['Heading 2','heading',2],['Heading 3','heading',3],['Bulleted list','bullet'],['Numbered list','ordered'],['Checklist','task'],['Quote','quote'],['Code block','codeBlock'],['Divider','rule'],['Table','table'],['Image','image'],['File','file'],['Math formula','math'],['Link','link']];
function docsToolRunChoice(snapshot,choice,range){
  if(!docsToolCurrent(snapshot))return false;docsHideFloating();const [label,name,argument]=choice,session=snapshot.session,context={snapshot,range};
  if(name==='image'||name==='file')return docsPickAttachments(session,name,context);if(name==='math')return docsInsertMath(session,context);if(name==='link')return docsLinkDialog(session,context);
  let chain=docsToolChain(snapshot,range);if(name==='paragraph')chain=chain.clearNodes().setParagraph();else if(name==='heading')chain=chain.setHeading({level:argument});else{const commands={bullet:'toggleBulletList',ordered:'toggleOrderedList',task:'toggleTaskList',quote:'toggleBlockquote',codeBlock:'toggleCodeBlock',rule:'setHorizontalRule',table:'insertTable'};chain=name==='table'?chain.insertTable({rows:3,cols:3,withHeaderRow:true}):chain[commands[name]]();}const result=chain.run();docsUpdateTools(session);return result;
}
function docsToolMenu(snapshot,choices,rect,range=null,kind='insert'){
  let menu=documentsUi.slashMenu;if(!menu){menu=document.createElement('div');menu.className='page-slash-menu';menu.setAttribute('role','listbox');menu.setAttribute('aria-label','Insert block');menu.tabIndex=-1;document.body.append(menu);documentsUi.slashMenu=menu;menu.onkeydown=event=>{if(menu._docsState)docsEditorKey(menu._docsState.snapshot.session,event);};}
  menu.replaceChildren();const state={snapshot,choices,index:0,range,kind};menu._docsState=state;
  choices.forEach((choice,index)=>{const button=docsToolButton(choice[0],()=>docsToolRunChoice(snapshot,choice,range),'document-block-choice');button.setAttribute('role','option');button.id='document-block-'+(++docsToolSerial);button.setAttribute('aria-selected',String(index===0));menu.append(button);});
  if(!choices.length)menu.append(docsText('p','No matching blocks.','settings-sub'));docsPlaceFloating(menu,rect,false);docsToolMenuIndex(menu,0);if(kind==='insert')menu.focus();
}
function docsToolMenuIndex(menu,index){const state=menu._docsState;if(!state||!state.choices.length)return;state.index=(index+state.choices.length)%state.choices.length;const buttons=menu.querySelectorAll('[role=option]');buttons.forEach((button,i)=>button.setAttribute('aria-selected',String(i===state.index)));const active=buttons[state.index];menu.setAttribute('aria-activedescendant',active.id);active.scrollIntoView?.({block:'nearest'});}
function docsInsertMenu(session){const snapshot=docsToolSnapshot(session);if(!snapshot)return;const anchor=Array.from(session.toolbar?.querySelectorAll('button')||[]).find(button=>button.textContent==='Insert');const rect=anchor?.getBoundingClientRect()||docsToolRect(snapshot);if(rect)docsToolMenu(snapshot,docsBlockChoices,rect);}
function docsSlash(session){
  if(!docsCurrent(session))return;const snapshot=docsToolSnapshot(session),menu=documentsUi.slashMenu;if(session._docsComposing){if(menu?._docsState?.kind==='slash')menu.hidden=true;return;}if(!snapshot||!snapshot.selection.empty||$('modal-backdrop')?.classList.contains('open')){if(menu?._docsState?.kind==='slash')menu.hidden=true;return;}
  const editor=snapshot.bridge.editor,from=editor.state.selection.$from;if(!editor.isFocused){if(menu?._docsState?.kind==='slash')menu.hidden=true;return;}if(!from.parent.isTextblock||from.parent.type.name==='codeBlock'){if(menu?._docsState?.kind==='slash')menu.hidden=true;return;}
  const prefix=from.parent.textBetween(0,from.parentOffset,'\uFFFC','\uFFFC'),match=prefix.match(/^\/([^\r\n/]{0,80})$/);if(!match||session._docsSlashDismissed?.doc===snapshot.doc&&session._docsSlashDismissed?.from===snapshot.selection.from){if(menu?._docsState?.kind==='slash')menu.hidden=true;return;}
  const query=match[1].trim().toLowerCase(),choices=docsBlockChoices.filter(choice=>choice[0].toLowerCase().includes(query));const rect=docsToolRect(snapshot);if(rect)docsToolMenu(snapshot,choices,rect,{from:from.start(),to:snapshot.selection.from},'slash');
}
function docsEditorKey(session,event){
  if(!docsCurrent(session)||event.isComposing||event.keyCode===229||session._docsComposing)return false;const key=event.key?.toLowerCase(),command=event.metaKey||event.ctrlKey;
  const consume=()=>{event.preventDefault();event.stopPropagation();event.stopImmediatePropagation?.();return true;};
  if(command&&key==='f'){docsToggleFind(session,true);return consume();}
  if(session.source&&!session.locked&&!session.conflict&&command&&(key==='z'||key==='y')){const state=session._docsFind,redo=key==='y'||event.shiftKey;const stack=redo?state?.sourceRedo:state?.sourceUndo,item=stack?.at(-1);if(item&&session.source.value===(redo?item.before:item.after)){stack.pop();session.source.value=redo?item.after:item.before;(redo?state.sourceUndo:state.sourceRedo).push(item);docsChanged(session);docsFindRefresh(session,false);return consume();}}
  const menu=documentsUi.slashMenu,state=menu?._docsState;if(!menu?.hidden&&state?.snapshot.session===session&&docsToolCurrent(state.snapshot)){
    if(key==='arrowdown'||key==='arrowup'){docsToolMenuIndex(menu,state.index+(key==='arrowdown'?1:-1));return consume();}
    if(key==='enter'){const choice=state.choices[state.index];docsHideFloating();if(choice)Promise.resolve(docsToolRunChoice(state.snapshot,choice,state.range)).catch(showError);return consume();}
    if(key==='escape'){session._docsSlashDismissed={doc:session.bridge.editor.state.doc,from:session.bridge.editor.state.selection.from};docsHideFloating();session.bridge.focus();return consume();}
  }
  if(key==='escape'&&!session.findBar?.hidden){docsToggleFind(session,false);return consume();}return false;
}
function docsBuildFind(session){
  const bar=document.createElement('div');bar.className='page-find-bar';bar.hidden=true;bar.setAttribute('role','search');bar.setAttribute('aria-label','Find and replace in document');const query=docsToolInput('Find text'),replacement=docsToolInput('Replace with'),count=docsText('span','0 matches','page-find-count'),matchCase=document.createElement('input');matchCase.type='checkbox';matchCase.setAttribute('aria-label','Match case');
  const state={query,replacement,count,matchCase,matches:[],index:-1,doc:null,sourceUndo:[],sourceRedo:[]};session._docsFind=state;
  const run=fn=>{if(docsCurrent(session))return fn();};bar.append(query,replacement,docsToolLabel('Match case',matchCase),count,docsButton('Previous',()=>run(()=>docsFindStep(session,-1))),docsButton('Next',()=>run(()=>docsFindStep(session,1))),docsButton('Replace',()=>run(()=>docsFindReplace(session,false))),docsButton('Replace all',()=>run(()=>docsFindReplace(session,true))),docsButton('Close',()=>docsToggleFind(session,false)));
  query.oninput=()=>run(()=>docsFindRefresh(session,true));matchCase.onchange=()=>run(()=>docsFindRefresh(session,true));query.onkeydown=event=>{if(event.key==='Enter'){event.preventDefault();docsFindStep(session,event.shiftKey?-1:1);}if(event.key==='Escape'){event.preventDefault();docsToggleFind(session,false);}};
  session.shell.addEventListener('input',event=>{if(event.target===session.source&&!session.findBar?.hidden)docsFindRefresh(session,false);});
  session.shell.addEventListener('compositionstart',()=>{session._docsComposing=true;docsHideFloating();},true);session.shell.addEventListener('compositionend',()=>{session._docsComposing=false;queueMicrotask(()=>{if(docsCurrent(session))docsSlash(session);});},true);
  session.shell.addEventListener('keydown',event=>{if((event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==='f'||session.source&&event.target===session.source)docsEditorKey(session,event);},true);return bar;
}
function docsToggleFind(session,force){if(!docsCurrent(session)||!session.findBar)return;const show=force??session.findBar.hidden;session.findBar.hidden=!show;if(show){documentsUi.find=session._docsFind;docsFindRefresh(session,false);session._docsFind.query.focus();session._docsFind.query.select();}else if(session.bridge)session.bridge.focus();else session.source?.focus();}
function docsFindRefresh(session,select=false){
  const state=session._docsFind;if(!state||!docsCurrent(session))return;const query=state.query.value;state.matches=[];const flags=state.matchCase.checked?'gu':'giu';let pattern;if(query&&query.length<=4096)pattern=new RegExp(query.replace(/[.*+?^${}()|[\]\\]/g,'\\$&'),flags);
  if(session.bridge){const doc=session.bridge.editor.state.doc;state.doc=doc;if(pattern)doc.descendants((node,pos)=>{if(!node.isTextblock)return;const content=node.textBetween(0,node.content.size,'\n','\uFFFC');for(const match of content.matchAll(pattern))if(!match[0].includes('\uFFFC'))state.matches.push({from:pos+1+match.index,to:pos+1+match.index+match[0].length});return false;});}
  else if(session.source){state.doc=session.source.value;if(pattern)for(const match of session.source.value.matchAll(pattern))state.matches.push({from:match.index,to:match.index+match[0].length});}
  state.index=state.matches.length?Math.max(0,Math.min(state.index,state.matches.length-1)):-1;docsFindCount(state);if(select&&state.index>=0)docsFindSelect(session,state.index);
}
function docsFindCount(state){state.count.textContent=state.matches.length?(state.index+1)+' of '+state.matches.length:'0 matches';}
function docsFindSelect(session,index){if(!docsCurrent(session))return;const state=session._docsFind,match=state.matches[index];if(!match)return;state.index=index;docsFindCount(state);if(session.bridge){session.bridge.editor.commands.setTextSelection(match);session.bridge.editor.view.dispatch(session.bridge.editor.state.tr.scrollIntoView());}else session.source.setSelectionRange(match.from,match.to);}
function docsFindStep(session,direction){docsFindRefresh(session,false);const state=session._docsFind;if(state.matches.length)docsFindSelect(session,(state.index+direction+state.matches.length)%state.matches.length);}
function docsFindReplace(session,all){
  if(!docsCurrent(session)||session.conflict||session.locked)return;docsFindRefresh(session,false);const state=session._docsFind,chosen=all?state.matches:[state.matches[state.index]].filter(Boolean);if(!chosen.length)return;const replacement=state.replacement.value;
  if(session.bridge){const editor=session.bridge.editor,tr=editor.state.tr;for(const match of [...chosen].reverse())tr.insertText(replacement,match.from,match.to);editor.view.dispatch(tr);}
  else if(session.source){const source=session.source,before=source.value;let after=before;for(const match of [...chosen].reverse())after=after.slice(0,match.from)+replacement+after.slice(match.to);let native=false;const active=document.activeElement;if(typeof document.execCommand==='function'){try{source.focus();source.setSelectionRange(0,before.length);native=document.execCommand('insertText',false,after)&&source.value===after;}catch{}}if(!native){state.sourceUndo.push({before,after});while(state.sourceUndo.length>1&&(state.sourceUndo.length>20||state.sourceUndo.reduce((size,item)=>size+item.before.length+item.after.length,0)>16*1024*1024))state.sourceUndo.shift();state.sourceRedo=[];source.value=after;}if(active&&active!==source&&active.isConnected)active.focus();docsChanged(session);}
  docsFindRefresh(session,false);
}
async function docsReadFiles(snapshot,files,owner=null){
  const current=()=>docsToolCurrent(snapshot)&&(!owner||docsToolOwns(owner)),metadata=[];for(const file of Array.from(files||[])){
    if(!current())return [];if(file.size>20*1024*1024)throw new Error(file.name+' exceeds 20 MiB');const dataBase64=await fileBase64(file);if(!current())return [];
    const result=await rpcLocalCall('files/import',{name:file.name,mimeType:file.type||undefined,dataBase64});if(!current())return [];metadata.push(result.attachment);
  }return metadata;
}
function docsFileNode(file,kind,values={}){
  if(kind==='image'||kind!=='file'&&file.kind==='image')return {type:'image',attrs:{attachmentId:file.id,name:file.name,alt:values.alt??file.name,title:values.title||'',width:values.width??null,height:values.height??null}};
  return {type:'fileAttachment',attrs:{attachmentId:file.id,name:file.name,mimeType:file.mimeType||'application/octet-stream',size:file.size??null}};
}
async function docsImportFiles(session,files){
  const snapshot=docsToolSnapshot(session);if(!snapshot)return [];try{const imported=await docsReadFiles(snapshot,files);if(!docsToolCurrent(snapshot))return [];
    if(imported.length)docsToolChain(snapshot).insertContent(imported.map(file=>docsFileNode(file))).run();return imported;
  }catch(error){if(docsToolCurrent(snapshot))throw error;return [];}
}
async function docsPickAttachments(session,kind,context={}){
  const snapshot=context.snapshot||docsToolSnapshot(session);if(!snapshot)return;kind=kind||(snapshot.node?.type.name==='image'?'image':'file');const owner=docsToolDialog(snapshot,kind==='image'?'Image':'File');if(!owner)return;
  const loading=docsText('p','Loading local Library…','settings-sub');owner.root.append(loading);
  try{const result=await rpcLocalCall('files/list');if(!docsToolOwns(owner))return;let files=(result.data||[]).filter(file=>kind!=='image'||file.kind==='image'),selected=snapshot.node?.type.name===(kind==='image'?'image':'fileAttachment')?snapshot.node.attrs.attachmentId:null;
    if(selected&&!files.some(file=>file.id===selected))files.unshift({id:selected,kind:kind==='image'?'image':'file',name:snapshot.node.attrs.name,...snapshot.node.attrs});
    owner.root.replaceChildren();const list=document.createElement('div');list.className='document-attachment-list';list.setAttribute('role','radiogroup');list.setAttribute('aria-label','Local Library files');const actions=document.createElement('div');actions.className='document-tool-actions';
    const input=document.createElement('input');input.type='file';input.multiple=true;input.hidden=true;if(kind==='image')input.accept='image/png,image/jpeg,image/webp';
    const alt=docsToolInput('Alternative text',snapshot.node?.attrs.alt||''),title=docsToolInput('Image title',snapshot.node?.attrs.title||''),width=docsToolInput('Width in pixels',snapshot.node?.attrs.width||'','number'),height=docsToolInput('Height in pixels',snapshot.node?.attrs.height||'','number');for(const field of [width,height]){field.min='1';field.max='100000';field.placeholder='Auto';}
    const imageFields=document.createElement('div');imageFields.className='document-image-fields';if(kind==='image')imageFields.append(docsToolLabel('Alternative text',alt),docsToolLabel('Title',title),docsToolLabel('Width',width),docsToolLabel('Height',height));let altEdited=Boolean(alt.value);alt.oninput=()=>altEdited=true;
    const render=()=>{list.replaceChildren();for(const file of files){const label=document.createElement('label'),radio=document.createElement('input');radio.type='radio';radio.name='document-file-'+owner.serial;radio.checked=file.id===selected;radio.onchange=()=>{selected=file.id;if(!altEdited)alt.value=file.name;};label.append(radio,docsText('span',file.name),docsText('small',file.mimeType||file.kind||'File'));list.append(label);}if(!files.length)list.append(docsText('p','No matching files. Import a file to continue.','settings-sub'));};render();
    owner.root.append(list,imageFields,input,actions);
    actions.append(docsButton('Import…',()=>{if(docsToolOwns(owner))input.click();}),docsButton('Cancel',()=>docsToolClose(owner)),docsButton(selected?'Save / replace':'Insert',()=>{
      if(!docsToolOwns(owner))return;const file=files.find(file=>file.id===selected);if(!file){docsToolError(owner,new Error('Select a file from Library first.'));return;}
      const dimension=field=>{if(!field.value.trim())return null;const value=Number(field.value);if(!Number.isSafeInteger(value)||value<1||value>100000)throw new Error('Image dimensions must be positive integers up to 100000.');return value;};
      docsToolApply(owner,()=>{const node=docsFileNode(file,kind,{alt:alt.value,title:title.value,width:dimension(width),height:dimension(height)});EditorBridge.validateJSON({type:'doc',content:[node]});const chain=docsToolChain(snapshot,context.range);if(snapshot.node?.type.name===node.type&&!context.range)return chain.updateAttributes(node.type,node.attrs).run();return chain.insertContent(node).run();});
    },'btn-pill primary'));
    if(kind==='image')actions.prepend(docsButton('Edit image…',()=>{if(docsToolOwns(owner)&&selected)return openImageEditor({kind:'edit',imageIds:[selected]});}));
    input.onchange=async()=>{const picked=Array.from(input.files||[]);input.value='';try{const imported=await docsReadFiles(snapshot,picked,owner);if(!docsToolOwns(owner))return;files=[...imported.filter(file=>kind!=='image'||file.kind==='image'),...files];if(imported.length){selected=imported[0].id;if(!altEdited)alt.value=imported[0].name;}render();}catch(error){docsToolError(owner,error);}};
  }catch(error){docsToolError(owner,error);}
}
function docsInsertMath(session,context={}){
  const snapshot=context.snapshot||docsToolSnapshot(session),owner=docsToolDialog(snapshot,'Math formula');if(!owner)return;const current=snapshot.node&&['inlineMath','blockMath'].includes(snapshot.node.type.name)?snapshot.node:null;
  const source=document.createElement('textarea');source.rows=4;source.value=current?.attrs.latex||'';source.setAttribute('aria-label','LaTeX source');const display=document.createElement('select');display.setAttribute('aria-label','Formula layout');for(const [value,label] of [['inlineMath','Inline'],['blockMath','Block']]){const option=document.createElement('option');option.value=value;option.textContent=label;display.append(option);}display.value=current?.type.name||'inlineMath';owner.root.append(docsToolLabel('LaTeX',source),docsToolLabel('Layout',display),docsText('p','Formulas are rendered locally. JavaScript and untrusted HTML are not executed.','settings-sub'));
  const actions=document.createElement('div');actions.className='document-tool-actions';actions.append(docsButton('Cancel',()=>docsToolClose(owner)),docsButton(current?'Update formula':'Insert formula',()=>{if(!source.value.trim()){docsToolError(owner,new Error('Enter a formula.'));return;}docsToolApply(owner,()=>{EditorBridge.validateJSON({type:'doc',content:[display.value==='inlineMath'?{type:'paragraph',content:[{type:'inlineMath',attrs:{latex:source.value}}]}:{type:'blockMath',attrs:{latex:source.value}}]});const chain=docsToolChain(snapshot,context.range);if(current?.type.name===display.value&&!context.range)return chain.updateAttributes(display.value,{latex:source.value}).run();return chain.insertContent({type:display.value,attrs:{latex:source.value}}).run();});},'btn-pill primary'));owner.root.append(actions);source.focus();
}
window.addEventListener('resize',()=>{docsHideFloating();if(documentsUi.current)docsToolsTableLayout(documentsUi.current);});
