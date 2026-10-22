// Local Pages hierarchy, references and reusable content blocks.
const workBlockNames={prompt:'Prompt',task:'Task',instructions:'Agent Instructions',highlight:'Highlight',page:'Page reference',chat:'Chat reference',visualize:'Visualization'};
function docsWorkNode(kind,fields={}){return {type:'workBlock',attrs:{kind,title:'',text:'',targetId:'',html:'',...fields}};}
function docsWorkInsert(snapshot,node,range){
  if(!docsToolCurrent(snapshot))throw new Error('The document changed. Insert the block again.');
  EditorBridge.validateJSON({type:'doc',content:[node]});return docsToolChain(snapshot,range).insertContent(node).run();
}
async function docsWorkBlockAction(session,action){
  if(!docsCurrent(session))return;const snapshot=docsToolSnapshot(session);if(!snapshot?.node||snapshot.node.type.name!=='workBlock')return;
  const attrs=snapshot.node.attrs;
  if(action.action==='edit')return docsWorkDialog(snapshot,attrs.kind);
  if(action.action==='open'){
    if(attrs.kind==='page')return openDocument(attrs.targetId);
    if(attrs.kind==='chat'){const result=await rpcLocalCall('thread/read',{threadId:attrs.targetId});if(!docsToolCurrent(snapshot))return;if(result.thread)return openThread(attrs.targetId);}
    return;
  }
  if(action.action==='run'){
    let text=(attrs.title?attrs.title+'\n\n':'')+attrs.text;if(!text.trim())throw new Error('Write a request in this block first.');
    if(attrs.kind!=='instructions'){const guidance=[];snapshot.bridge.editor.state.doc.descendants(node=>{if(node.type.name==='workBlock'&&node.attrs.kind==='instructions'&&node.attrs.text)guidance.push(node.attrs.text);});if(guidance.length)text='Page guidance:\n'+guidance.join('\n\n')+'\n\nRequest:\n'+text;}
    await docsFlush(session);if(!docsToolCurrent(snapshot))return;
    if(typeof remoteContext==='function'&&remoteContext())await remoteSelect(null);
    await switchMode(attrs.kind==='task'?'codex':'work');await newThread(false);fillPrompt(text);saveDraft();await savePreferences({drafts:Object.fromEntries(desktop.drafts)});
  }
}
async function docsWorkDialog(snapshot,kind,range){
  const owner=docsToolDialog(snapshot,workBlockNames[kind]||'Work block');if(!owner)return;
  const prior=snapshot.node?.type.name==='workBlock'&&snapshot.node.attrs.kind===kind?snapshot.node.attrs:{};
  const title=docsToolInput('Block title',prior.title||''),text=document.createElement('textarea');text.rows=6;text.maxLength=65536;text.setAttribute('aria-label',kind==='visualize'?'Visualization HTML':'Block content');text.value=kind==='visualize'?prior.html||'<h2>Local calculator</h2><label>Value <input id="value" type="number" value="10"></label><p>Double: <output id="result">20</output></p><script>value.oninput=()=>result.value=Number(value.value)*2<\/script>':prior.text||'';
  owner.root.append(docsToolLabel('Title',title));
  const reference=document.createElement('select');reference.setAttribute('aria-label','Local reference');
  if(['page','chat'].includes(kind)){
    owner.root.append(docsToolLabel('Reference',reference));
    try{const result=await rpcLocalCall(kind==='page'?'desktop/document/list':'thread/list',{limit:1000});if(!docsToolOwns(owner))return;
      for(const item of result.items||result.data||[]){const option=document.createElement('option');option.value=item.id;option.textContent=item.title||item.name||item.preview||item.id;reference.append(option);}reference.value=prior.targetId||reference.options[0]?.value||'';
    }catch(error){docsToolError(owner,error);}
  }else owner.root.append(docsToolLabel(kind==='visualize'?'HTML':'Content',text));
  if(kind==='visualize')owner.root.append(docsText('p','The visualization runs in an isolated frame without network, local tools or access to this app.','settings-sub'));
  owner.root.append(docsActions(docsButton('Cancel',()=>docsToolClose(owner)),docsButton(prior.kind?'Save block':'Insert block',()=>{
    docsToolApply(owner,()=>{const node=docsWorkNode(kind,{title:title.value,text:kind==='visualize'?'':text.value,targetId:['page','chat'].includes(kind)?reference.value:'',html:kind==='visualize'?text.value:''});EditorBridge.validateJSON({type:'doc',content:[node]});const chain=docsToolChain(snapshot,range);return prior.kind&&!range?chain.updateAttributes('workBlock',node.attrs).run():chain.insertContent(node).run();});
  },'btn-pill primary')));
}
async function docsCreateSubpage(snapshot,range,sourceSession){
  const session=snapshot?.session||sourceSession;if(!session||!docsCurrent(session))return;
  const owner=snapshot?docsToolDialog(snapshot,'New subpage'):{root:docsModal('New subpage')};if(!owner)return;
  const current=()=>snapshot?docsToolOwns(owner):owner.root.isConnected&&docsCurrent(session);
  const title=docsToolInput('Page title');owner.root.append(docsToolLabel('Title',title),docsActions(docsButton('Cancel',()=>closeModal()),docsButton('Create page',async()=>{
    if(!current())return;if(!title.value.trim())throw new Error('Enter a page title');
    await docsFlush(session);if(!current())return;
    const result=await rpcLocalCall('desktop/document/create',{title:title.value,parentId:session.doc.id,projectId:session.doc.projectId,content:''});
    if(current()){if(snapshot){docsWorkInsert(snapshot,docsWorkNode('page',{targetId:result.document.id,title:result.document.title}),range);await docsFlush(session);}closeModal();await openDocument(result.document.id);}
    else toast('The subpage is saved in Library');
  },'btn-pill primary')));title.focus();
}
async function docsMovePage(session){
  await docsFlush(session);if(!docsCurrent(session))return;const result=await rpcLocalCall('desktop/document/list',{});if(!docsCurrent(session))return;
  const root=docsModal('Move page'),select=document.createElement('select');select.setAttribute('aria-label','Parent page');
  const byId=new Map(result.items.map(item=>[item.id,item]));const descendant=item=>{const seen=new Set();for(let parent=item;parent;parent=byId.get(parent.parentId)){if(parent.id===session.doc.id)return true;if(seen.has(parent.id))return true;seen.add(parent.id);}return false;};
  for(const item of [{id:'',title:'Library (top level)'},...result.items.filter(item=>!descendant(item))]){const option=document.createElement('option');option.value=item.id;option.textContent=item.title;select.append(option);}select.value=session.doc.parentId||'';
  root.append(docsToolLabel('Move under',select),docsActions(docsButton('Cancel',()=>closeModal()),docsButton('Move',async()=>{if(!root.isConnected||!docsCurrent(session))return;await docsMutate(session,'desktop/document/update',{parentId:select.value||null});closeModal();await docsWorkHeader(session);})));
}
async function docsWorkHeader(session){
  if(!docsCurrent(session))return;let toolbar=session.shell.querySelector('.page-local-actions');if(!toolbar){toolbar=document.createElement('div');toolbar.className='page-local-actions';session.shell.querySelector('.page-editor-header').append(toolbar);}
  toolbar.replaceChildren(docsButton('New subpage',()=>docsCreateSubpage(docsToolSnapshot(session),null,session)),docsButton('Move page',()=>docsMovePage(session)),docsButton('Generate',()=>docsGenerate(session,'generate')),docsButton('Visualize',()=>docsGenerate(session,'visualize')));
  if(session.doc.parentId){const id=session.doc.parentId;try{const result=await rpcLocalCall('desktop/document/read',{id});if(docsCurrent(session)&&session.doc.parentId===id)toolbar.prepend(docsButton('↑ '+result.document.title,()=>openDocument(id)));}catch{if(docsCurrent(session))toolbar.prepend(docsText('span','Parent page unavailable','settings-sub'));}}
}
const docsLocalOpen=openDocument;
openDocument=async function(...args){const result=await docsLocalOpen(...args);if(documentsUi.current)await docsWorkHeader(documentsUi.current);return result;};
renderLibraryDocuments=function(root,records){
  const byParent=new Map();for(const item of records){const key=item.parentId||'';if(!byParent.has(key))byParent.set(key,[]);byParent.get(key).push(item);}root.replaceChildren();root.classList.add('local-page-tree');
  const visited=new Set();const render=(parent,depth)=>{for(const page of byParent.get(parent)||[]){if(visited.has(page.id))continue;visited.add(page.id);const button=docsButton(page.title,()=>openDocument(page.id),'local-page-link');button.style.paddingLeft=(14+depth*20)+'px';button.setAttribute('aria-label',(depth?'Subpage: ':'Page: ')+page.title);root.append(button);render(page.id,depth+1);}};render('',0);for(const page of records)if(!visited.has(page.id))root.append(docsButton(page.title,()=>openDocument(page.id),'local-page-link'));
};
const docsLocalChoice=docsToolRunChoice;
docsToolRunChoice=function(snapshot,choice,range){
  if(choice[1]==='local')return docsWorkDialog(snapshot,choice[2],range);
  if(choice[1]==='subpage')return docsCreateSubpage(snapshot,range);
  if(choice[1]==='generate'||choice[1]==='visualize'){if(range){const chain=docsToolChain(snapshot,range);if(!chain?.run())return;}return docsGenerate(snapshot.session,choice[1]);}
  return docsLocalChoice(snapshot,choice,range);
};
docsBlockChoices.push(['Generate','generate'],['Visualize','visualize'],['Page','subpage'],['Link to page','local','page'],['Pages reference','local','page'],['Chats reference','local','chat'],['Prompt','local','prompt'],['Task','local','task'],['Agent Instructions','local','instructions'],['Highlight','local','highlight']);
async function docsGenerate(session,purpose){
  if(!docsCurrent(session)||!session.bridge)throw new Error('Generation requires a rich text page. Create a new page to keep the original source unchanged.');const selection=docsSelection(session);return docsAskChange(session,{from:selection.from,to:selection.from,quote:'',empty:true},purpose);
}
function docsCommentAI(session,comment){
  if(!docsCurrent(session)||!session.bridge)throw new Error('Open a rich text page to request a draft from this comment.');
  const instructions=comment.body.replace(/@ChatGPT\s*/ig,'').trim();
  if(comment.anchor&&!comment.orphaned&&docsSelectionValid(session,comment.anchor))return docsAskChange(session,comment.anchor,'change',instructions);
  const selection=docsSelection(session);return docsAskChange(session,{from:selection.from,to:selection.from,quote:''},'generate',instructions);
}
const docsWorkPickAttachments=docsPickAttachments;
docsPickAttachments=async function(session,kind,context={}){
  if(kind!=='image')return docsWorkPickAttachments(session,kind,context);
  const snapshot=context.snapshot||docsToolSnapshot(session),owner=docsToolDialog(snapshot,'Image');if(!owner)return;
  owner.root.append(docsActions(docsButton('Attach from Library',()=>docsWorkPickAttachments(session,'image',{...context,snapshot})),docsButton('Generate image',()=>{
    if(!docsToolOwns(owner))return;
    return openImageEditor({kind:'generate',onUse:async file=>{
      if(!docsToolCurrent(snapshot))throw new Error('The page changed. The image is saved in Library; insert it at the current position.');
      const result=await rpcLocalCall('files/read',{id:file.id});if(!docsToolCurrent(snapshot))return;
      docsWorkInsert(snapshot,docsFileNode(result.attachment,'image'),context.range);await docsFlush(session);closeModal();
    }});
  })));
};
