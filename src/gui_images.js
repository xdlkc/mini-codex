// Image requests stay in Rust. A retry is always a new, explicit request.
const imagesUi={capabilities:null,jobs:new Map(),library:false,libraryRequest:0,dialog:null,settingsRequest:0,filter:'all',search:'',savedPage:0,previewQueue:[],previewActive:0,previewObservers:new Set(),savedFiles:[]};
function imagesError(error){return String(error?.message||error||'The image request could not finish').slice(0,1000);}
function imagesActive(job){return job?.status==='queued'||job?.status==='running';}
function imagesRemember(job){
  if(!job?.id)return null;const old=imagesUi.jobs.get(job.id);
  if(old&&Number(old.revision||0)>Number(job.revision||0))return old;
  if(old&&old.revision&&old.revision===job.revision)return old;
  if(old&&!old.revision&&!job.revision&&(old.updatedAt>job.updatedAt||(!imagesActive(old)&&imagesActive(job))))return old;
  imagesUi.jobs.set(job.id,job);return job;
}
function imagesButton(label,action,className='btn-pill'){
  const button=document.createElement('button');button.type='button';button.className=className;button.textContent=label;
  button.onclick=async()=>{button.disabled=true;try{await action();}catch(error){showError(error);}finally{button.disabled=false;}};return button;
}
function imagesText(tag,text,className=''){const node=document.createElement(tag);node.textContent=text;node.className=className;return node;}
function imagesOptions(select,values,selected){select.replaceChildren();for(const item of values){const option=document.createElement('option');option.value=typeof item==='string'?item:item[0];option.textContent=typeof item==='string'?item:item[1];select.append(option);}select.value=values.some(item=>(typeof item==='string'?item:item[0])===selected)?selected:select.options[0]?.value||'';}
function imagesDialogCurrent(dialog){return imagesUi.dialog===dialog&&dialog.navigation===desktop.navigation&&dialog.root.isConnected&&$('modal-backdrop')?.classList.contains('open');}
function imagesLibraryCurrent(ticket,root){return ticket===imagesUi.libraryRequest&&imagesUi.library&&desktop.library&&root.isConnected&&$('images-library')===root;}
function imagesInvalidate(){
  imagesUi.library=false;imagesUi.libraryRequest++;imagesUi.settingsRequest++;imagesUi.previewQueue=[];for(const item of imagesUi.previewObservers)item.observer.disconnect();imagesUi.previewObservers.clear();
  const dialog=imagesUi.dialog;imagesUi.dialog=null;if(dialog?.root.isConnected&&$('modal-backdrop')?.classList.contains('open'))closeModal();
}
async function imagesCapabilities(){const capabilities=await rpcLocalCall('images/capabilities');imagesUi.capabilities=capabilities;return capabilities;}
function imagesConnect(){closeModal();return openSettingsView('account');}
function imagesModelOptions(capabilities,model){return capabilities.modelOptions?.[model]||{qualities:(capabilities.qualities||['auto','low','medium','high']).filter(value=>!['xhigh','max'].includes(value)||model.startsWith('gpt-image-2.5-')),customSizes:Boolean(capabilities.customSizes?.models?.includes(model)),outputFormats:capabilities.outputFormats||['png','jpeg','webp']};}
function imagesCaptureDraft(dialog){
  if(!imagesDialogCurrent(dialog))return dialog.draft;const draft=dialog.draft;
  for(const key of ['kind','prompt','model','size','quality','background','outputFormat','n','width','height','maskId']){const node=$('images-'+key);if(node)draft[key]=node.value;}
  return draft;
}
function imagesSyncLimits(dialog){
  if(!imagesDialogCurrent(dialog))return;imagesCaptureDraft(dialog);const draft=dialog.draft,options=imagesModelOptions(dialog.capabilities,draft.model);
  imagesOptions($('images-quality'),options.qualities,draft.quality||'auto');draft.quality=$('images-quality').value;
  imagesOptions($('images-outputFormat'),options.outputFormats,draft.outputFormat||'png');
  const sizes=dialog.capabilities.sizes||['auto','1024x1024','1536x1024','1024x1536'];imagesOptions($('images-size'),options.customSizes?[...sizes,['custom','Custom size…']]:sizes,draft.size||'auto');draft.size=$('images-size').value;
  $('images-custom-size').hidden=draft.size!=='custom';$('images-edit-inputs').hidden=draft.kind!=='edit';
  const transparent=draft.background==='transparent';for(const option of $('images-outputFormat').options)option.disabled=transparent&&option.value==='jpeg';
  if(transparent&&$('images-outputFormat').value==='jpeg')$('images-outputFormat').value='png';draft.outputFormat=$('images-outputFormat').value;
  $('images-model-note').textContent=transparent&&options.transparentBackground==='preview'?'Transparent backgrounds are a preview feature for this model.':draft.kind==='edit'?'The first selected image defines the mask dimensions. Transparent mask areas will be edited.':'';
  $('images-submit').textContent=draft.kind==='edit'?'Edit image':'Generate image';
}
function imagesRequest(draft,capabilities){
  const prompt=String(draft.prompt||'').trim();if(!prompt)throw new Error('Describe the image you want to create or change.');
  if(Array.from(prompt).length>(capabilities.limits?.maxPromptCharacters||32000))throw new Error('The image instructions exceed this model’s prompt limit.');
  if(!capabilities.available)throw new Error(capabilities.reason||'Connect an OpenAI API key to create images.');
  if(!capabilities.models?.includes(draft.model))throw new Error('Choose a supported image model.');
  const options=imagesModelOptions(capabilities,draft.model);if(!options.qualities.includes(draft.quality))throw new Error('This model does not support the selected quality.');
  let size=draft.size;
  if(size==='custom'){
    const width=Number(draft.width),height=Number(draft.height),limits=capabilities.customSizes||{};
    if(!options.customSizes||!Number.isSafeInteger(width)||!Number.isSafeInteger(height)||width<=0||height<=0||width%(limits.step||16)||height%(limits.step||16)||Math.max(width,height)>(limits.maxEdge||3840)||width*height<(limits.minPixels||655360)||width*height>(limits.maxPixels||8294400)||Math.max(width,height)/Math.min(width,height)>(limits.maxAspectRatio||3))throw new Error('Custom size must use multiples of 16, meet this model’s pixel limits, and have an aspect ratio of at most 3:1.');
    size=width+'x'+height;
  }else if(!capabilities.sizes?.includes(size))throw new Error('Choose a supported image size.');
  if(!capabilities.backgrounds?.includes(draft.background)||!options.outputFormats.includes(draft.outputFormat))throw new Error('Choose a supported background and file format.');
  if(draft.background==='transparent'&&draft.outputFormat==='jpeg')throw new Error('Transparent images require PNG or WebP.');
  const n=Number(draft.n);if(!Number.isInteger(n)||n<1||n>(capabilities.limits?.maxImages||4))throw new Error('Choose a valid image count.');
  const params={prompt,model:draft.model,size,quality:draft.quality,background:draft.background,outputFormat:draft.outputFormat,n};
  if(!['edit','generate'].includes(draft.kind))throw new Error('Choose an image task.');
  if(draft.kind==='edit'){
    const ids=[...new Set(draft.imageIds||[])];if(!ids.length||ids.length>(capabilities.limits?.maxInputImages||8))throw new Error('Select 1 to '+(capabilities.limits?.maxInputImages||8)+' reference images from Library.');
    params.imageIds=ids;if(draft.maskId)params.maskId=draft.maskId;
  }
  return {method:draft.kind==='edit'?'images/edit':'images/generate',params};
}
async function imagesSubmit(dialog){
  if(!imagesDialogCurrent(dialog)||dialog.submitting)return;imagesCaptureDraft(dialog);const errorNode=$('images-form-error');errorNode.textContent='';
  let request;try{request=imagesRequest(dialog.draft,dialog.capabilities);}catch(error){errorNode.textContent=imagesError(error);return;}
  dialog.submitting=true;$('images-submit').disabled=true;
  try{
    const result=await rpcLocalCall(request.method,request.params);const job=imagesRemember(result.job);
    if(dialog.onUse){imagesUi.pageUses??=new Map();imagesUi.pageUses.set(job.id,dialog.onUse);}
    if(!imagesDialogCurrent(dialog))return;await openImageJob(job.id,job);
  }catch(error){if(imagesDialogCurrent(dialog)){errorNode.textContent=imagesError(error);errorNode.append(imagesText('span',' No automatic retry was made.'));}}
  finally{dialog.submitting=false;if(imagesDialogCurrent(dialog))$('images-submit').disabled=!dialog.capabilities.available;}
}
function imagesRenderEditor(dialog){
  if(!imagesDialogCurrent(dialog))return;const draft=dialog.draft,capabilities=dialog.capabilities;
  dialog.root.innerHTML=`<form id="images-form" class="images-form"><p id="images-access-note" class="settings-sub"></p><div id="images-connect-actions" class="image-actions"></div><label>Task<select id="images-kind"><option value="generate">Create image</option><option value="edit">Edit existing images</option></select></label><label>Instructions<textarea id="images-prompt" rows="4" placeholder="Describe the image or the changes to make"></textarea></label><div class="images-form-grid"><label>Model<select id="images-model"></select></label><label>Quality<select id="images-quality"></select></label><label>Size<select id="images-size"></select></label><label>Images<input id="images-n" type="number" min="1"></label><label>Background<select id="images-background"></select></label><label>Format<select id="images-outputFormat"></select></label></div><div id="images-custom-size" class="images-form-grid" hidden><label>Width<input id="images-width" type="number" step="16" min="16"></label><label>Height<input id="images-height" type="number" step="16" min="16"></label></div><p id="images-model-note" class="settings-sub"></p><section id="images-edit-inputs"><div class="image-actions" id="images-import-actions"></div><fieldset><legend>Reference images · selection order is the input order</legend><div id="images-reference-list" class="images-reference-list"></div></fieldset><label>Mask (optional)<select id="images-maskId"></select></label><p class="settings-sub">A mask must be a PNG with an alpha channel, match the first reference image, and be smaller than 4 MiB. Import the mask to Library first.</p></section><p class="settings-sub">This starts a new API request and may incur charges. Requests are never retried automatically.</p><div id="images-form-error" role="alert" class="images-error"></div><div id="images-form-actions" class="image-actions"><button id="images-submit" type="submit" class="btn-pill primary">Generate image</button></div></form>`;
  $('images-access-note').textContent=capabilities.available?'Images use your connected OpenAI API key. Model access is checked when the provider receives a request.':capabilities.reason||'Connect an OpenAI API key to create or edit images.';
  if(!capabilities.available)$('images-connect-actions').append(imagesButton('Connect API key',imagesConnect));
  for(const key of ['kind','prompt','n','width','height'])$('images-'+key).value=draft[key]||({n:1,width:1024,height:1024}[key]??'');
  $('images-prompt').maxLength=capabilities.limits?.maxPromptCharacters||32000;$('images-n').max=capabilities.limits?.maxImages||4;
  imagesOptions($('images-model'),capabilities.models||[],draft.model);imagesOptions($('images-background'),capabilities.backgrounds||['auto','opaque','transparent'],draft.background);
  imagesOptions($('images-maskId'),[['','No mask'],...dialog.files.filter(file=>file.mimeType==='image/png'&&file.size<4*1024*1024).map(file=>[file.id,file.name])],draft.maskId||'');
  for(const file of dialog.files){const label=document.createElement('label'),check=document.createElement('input');check.type='checkbox';check.checked=draft.imageIds.includes(file.id);check.onchange=()=>{if(check.checked){if(draft.imageIds.length>=(capabilities.limits?.maxInputImages||8)){check.checked=false;$('images-form-error').textContent='Too many reference images selected.';return;}draft.imageIds.push(file.id);}else draft.imageIds=draft.imageIds.filter(id=>id!==file.id);imagesRenderSelectedOrder(dialog);};label.append(check,imagesText('span',file.name));$('images-reference-list').append(label);}
  if(!dialog.files.length)$('images-reference-list').append(imagesText('p','No images in Library yet. Import PNG, JPEG, or WebP files.','settings-sub'));
  $('images-import-actions').append(imagesButton('Import image or mask…',async()=>{imagesCaptureDraft(dialog);dialog.input.click();}));
  const order=imagesText('p','','settings-sub');order.id='images-reference-order';$('images-edit-inputs').append(order);imagesRenderSelectedOrder(dialog);
  $('images-form-actions').append(imagesButton('Cancel',async()=>closeModal()));$('images-submit').disabled=!capabilities.available;
  for(const key of ['kind','model','size','background','outputFormat'])$('images-'+key).onchange=()=>imagesSyncLimits(dialog);
  $('images-form').onsubmit=event=>{event.preventDefault();imagesSubmit(dialog).catch(showError);};
  // Populate dependent fields before reading them into the draft.
  imagesOptions($('images-quality'),imagesModelOptions(capabilities,draft.model).qualities,draft.quality);imagesOptions($('images-outputFormat'),imagesModelOptions(capabilities,draft.model).outputFormats,draft.outputFormat);
  imagesOptions($('images-size'),[...(capabilities.sizes||['auto']),...(imagesModelOptions(capabilities,draft.model).customSizes?[['custom','Custom size…']]:[])],draft.size);imagesSyncLimits(dialog);if(dialog.input)dialog.root.append(dialog.input);
}
function imagesRenderSelectedOrder(dialog){if(!imagesDialogCurrent(dialog))return;const order=$('images-reference-order');if(order)order.textContent=dialog.draft.imageIds.map((id,index)=>(index+1)+'. '+(dialog.files.find(file=>file.id===id)?.name||'Unavailable image')).join(' · ');}
async function openImageEditor(seed={}){
  const navigation=desktop.navigation;openCustomModal(seed.retry?'Retry as a new image request':'Create or edit images','<div id="images-editor"><p class="settings-sub">Loading image options…</p></div>');
  const dialog={root:$('images-editor'),navigation,draft:null,capabilities:null,files:[],submitting:false,onUse:seed.onUse};imagesUi.dialog=dialog;
  try{
    const [capabilities,listed]=await Promise.all([imagesCapabilities(),rpcLocalCall('files/list')]);if(!imagesDialogCurrent(dialog))return;
    const standard=capabilities.sizes?.includes(seed.size||'auto');const dimensions=String(seed.size||'').split('x');
    dialog.capabilities=capabilities;dialog.files=(listed.data||[]).filter(file=>file.kind==='image');dialog.draft={kind:seed.kind==='edit'?'edit':'generate',prompt:seed.prompt||'',model:seed.model||capabilities.defaultModel,size:standard?seed.size||'auto':'custom',quality:seed.quality||'auto',background:seed.background||'auto',outputFormat:seed.outputFormat||'png',n:seed.n||1,imageIds:[...(seed.imageIds||[])],maskId:seed.maskId||'',width:dimensions[0]||1024,height:dimensions[1]||1024};
    const input=document.createElement('input');input.type='file';input.accept='image/png,image/jpeg,image/webp';input.multiple=true;input.hidden=true;dialog.input=input;dialog.root.append(input);
    input.onchange=async()=>{
      const files=Array.from(input.files||[]);input.value='';if(!files.length||!imagesDialogCurrent(dialog))return;
      try{for(const file of files){if(file.size>(capabilities.limits?.maxImageBytes||10485760))throw new Error(file.name+' exceeds the image size limit.');const imported=await rpcLocalCall('files/import',{name:file.name,mimeType:file.type,dataBase64:await fileBase64(file)});if(!imagesDialogCurrent(dialog))return;if(imported.attachment.kind!=='image')throw new Error('Choose an image file.');dialog.files.unshift(imported.attachment);}imagesCaptureDraft(dialog);imagesRenderEditor(dialog);}catch(error){if(imagesDialogCurrent(dialog))$('images-form-error').textContent=imagesError(error);}
    };
    imagesRenderEditor(dialog);dialog.root.append(input);$('images-prompt').focus();
  }catch(error){if(imagesDialogCurrent(dialog)){dialog.root.replaceChildren(imagesText('p',imagesError(error),'images-error'),imagesButton('Close',async()=>closeModal()));}}
}
function imagesPrunePreviews(){for(const item of imagesUi.previewObservers)if(!item.isCurrent()||!item.image.isConnected){item.observer.disconnect();imagesUi.previewObservers.delete(item);}imagesUi.previewQueue=imagesUi.previewQueue.filter(item=>item.isCurrent()&&item.image.isConnected);}
function imagesThumbnail(id,image,isCurrent){
  imagesPrunePreviews();const enqueue=()=>{if(!isCurrent()||!image.isConnected)return;imagesUi.previewQueue.push({id,image,isCurrent});imagesDrainPreviews();};
  if(typeof IntersectionObserver==='function'){const item={image,isCurrent,observer:null};item.observer=new IntersectionObserver(entries=>{if(entries.some(entry=>entry.isIntersecting)){item.observer.disconnect();imagesUi.previewObservers.delete(item);enqueue();}else if(!isCurrent()||!image.isConnected){item.observer.disconnect();imagesUi.previewObservers.delete(item);}},{rootMargin:'120px'});imagesUi.previewObservers.add(item);item.observer.observe(image);}else queueMicrotask(enqueue);
}
function imagesDrainPreviews(){
  while(imagesUi.previewActive<3&&imagesUi.previewQueue.length){const request=imagesUi.previewQueue.shift();if(!request.isCurrent()||!request.image.isConnected)continue;imagesUi.previewActive++;
    imagesLoadPreview(request).finally(()=>{imagesUi.previewActive--;imagesDrainPreviews();});
  }
}
async function imagesLoadPreview({id,image,isCurrent}){
  try{const result=await rpcLocalCall('files/read',{id,includeOriginal:true});if(!isCurrent()||!image.isConnected)return;const mime=result.attachment?.mimeType,data=result.content?.dataBase64;if(!['image/png','image/jpeg','image/webp'].includes(mime)||typeof data!=='string'||!/^[A-Za-z0-9+/=\s]+$/.test(data))return;image.src='data:'+mime+';base64,'+data;image.alt=result.attachment.name||'Generated image';}catch{if(isCurrent()&&image.isConnected)image.alt='Image preview unavailable. Open the file to see details.';}
}
function imagesResultCard(file,isCurrent,jobId=null){
  const card=document.createElement('article');card.className='image-result-card';const image=document.createElement('img');image.alt=file.name||'Generated image';image.loading='lazy';card.append(image,imagesText('strong',file.name||'Generated image'));
  const actions=document.createElement('div');actions.className='image-actions';actions.append(imagesButton('Open',()=>openAttachment(file.id,{local:true,allowAttach:false})),imagesButton('Download',()=>downloadAttachment(file.id,{local:true})),imagesButton('Edit',()=>openImageEditor({kind:'edit',imageIds:[file.id]})));const use=imagesUi.pageUses?.get(jobId);if(use)actions.prepend(imagesButton('Insert in page',async()=>{if(isCurrent()){await use(file);imagesUi.pageUses.delete(jobId);}}));card.append(actions);imagesThumbnail(file.id,image,isCurrent);return card;
}
function imagesRenderJob(root,job,isCurrent){
  root.replaceChildren();const heading=document.createElement('div');heading.className='image-job-heading';heading.append(imagesText('strong',job.status==='cancelled'?'Local waiting stopped':job.status==='interrupted'?'Interrupted':job.status==='completed'?'Completed':job.status==='failed'?'Failed':job.status==='queued'?'Queued':'Generating images'));
  if(imagesActive(job)){const progress=document.createElement('progress');progress.setAttribute('aria-label','Image request in progress');heading.append(progress);}root.append(heading,imagesText('p',job.prompt,'image-job-prompt'),imagesText('p',[job.model,job.size,job.quality,job.n+' image'+(job.n===1?'':'s'),new Date(job.createdAt*1000).toLocaleString()].join(' · '),'settings-sub'));
  if(job.error)root.append(imagesText('p',job.error,'images-error'));if(job.cancelNote)root.append(imagesText('p',job.cancelNote,'settings-sub'));
  const actions=document.createElement('div');actions.className='image-actions';
  if(imagesActive(job))actions.append(imagesButton('Stop local waiting',()=>imagesCancelDialog(job)));
  else actions.append(imagesButton(job.status==='completed'?'Create another':'Retry as new request',()=>openImageEditor({...job,retry:job.status!=='completed'})));
  root.append(actions);const results=document.createElement('div');results.className='image-results';root.append(results);
  const attachments=job.attachments?.length?job.attachments:(job.attachmentIds||[]).map(id=>({id,name:'Generated image'}));for(const file of attachments)results.append(imagesResultCard(file,isCurrent,job.id));
}
async function openImageJob(id,known){
  const navigation=desktop.navigation;openCustomModal('Image request','<div id="images-job-detail"><p class="settings-sub">Loading request…</p></div>');
  const dialog={root:$('images-job-detail'),navigation,jobId:id};imagesUi.dialog=dialog;
  try{const job=imagesRemember(known||((await rpcLocalCall('images/job/read',{id})).job));if(imagesDialogCurrent(dialog))imagesRenderJob(dialog.root,job,()=>imagesDialogCurrent(dialog));}
  catch(error){if(imagesDialogCurrent(dialog))dialog.root.replaceChildren(imagesText('p',imagesError(error),'images-error'));}
}
function imagesCancelDialog(job){
  openCustomModal('Stop local waiting?','<div id="images-cancel"><p id="images-cancel-note"></p><div id="images-cancel-actions" class="image-actions"></div></div>');const navigation=desktop.navigation,root=$('images-cancel'),dialog={root,navigation};imagesUi.dialog=dialog;
  $('images-cancel-note').textContent=imagesUi.capabilities?.cancelNote||'This only stops waiting on this device. The provider may continue the request and charge for it. Late results will not be saved. No automatic retry will be made.';
  $('images-cancel-actions').append(imagesButton('Keep waiting',()=>openImageJob(job.id)),imagesButton('Stop local waiting',async()=>{const result=await rpcLocalCall('images/job/cancel',{id:job.id});imagesRemember(result.job);if(imagesDialogCurrent(dialog))await openImageJob(job.id,result.job);},'btn-pill danger'));
}
async function showImagesLibrary(){
  if(typeof voiceNavigationCleanup==='function')voiceNavigationCleanup();const navigation=++desktop.navigation,ticket=++imagesUi.libraryRequest;saveDraft();await flushDocument();if(navigation!==desktop.navigation||ticket!==imagesUi.libraryRequest)return;
  closeSettingsView();closeScheduledView();closeFloatingMenu();closeDrawer();closeModal();imagesUi.dialog=null;imagesUi.library=true;imagesUi.savedFiles=[];desktop.library=true;
  $('home-hero').classList.add('hidden');$('conversation-wrap').classList.add('hidden');$('composer-host').style.display='none';$('work-library').classList.remove('hidden');$('topbar-title').textContent='Library';
  $('work-library').innerHTML='<div class="library-shell" id="images-library"><div class="library-header"><h1>Library</h1><div id="images-library-tabs" class="image-actions"></div></div><div class="images-library-heading"><h2>Images</h2><div id="images-library-actions" class="image-actions"></div></div><p id="images-library-note" class="settings-sub"></p><div class="images-library-toolbar"><label>Status<select id="images-job-filter"><option value="all">All requests</option><option value="running">In progress</option><option value="completed">Completed</option><option value="failed">Failed</option><option value="cancelled">Stopped or interrupted</option></select></label><input id="images-job-search" type="search" aria-label="Search image requests" placeholder="Search image instructions"></div><div id="images-jobs" class="images-jobs" aria-live="polite">Loading requests…</div><h2>Saved images</h2><div id="images-saved" class="image-results"></div><div id="images-saved-pages" class="image-actions"></div></div>';
  const root=$('images-library');$('images-library-tabs').append(imagesButton('Documents',()=>showLibrary()),imagesButton('Files',()=>showFilesLibrary()),imagesButton('Images',()=>showImagesLibrary(),'btn-pill primary'));
  $('images-library-actions').append(imagesButton('Create image',()=>openImageEditor()),imagesButton('Refresh',()=>showImagesLibrary()));
  $('images-job-filter').value=imagesUi.filter;$('images-job-filter').onchange=()=>{imagesUi.filter=$('images-job-filter').value;imagesRenderJobs();};$('images-job-search').value=imagesUi.search;$('images-job-search').oninput=()=>{imagesUi.search=$('images-job-search').value;imagesRenderJobs();};
  try{const [capabilities,jobs,files]=await Promise.all([imagesCapabilities(),rpcLocalCall('images/job/list',{limit:1000}),rpcLocalCall('files/list')]);if(!imagesLibraryCurrent(ticket,root))return;
    $('images-library-note').textContent=capabilities.available?'Image requests use your API key. Job history and saved results stay on this device.':capabilities.reason||'Connect an OpenAI API key to create images.';
    if(!capabilities.available)$('images-library-actions').append(imagesButton('Connect API key',imagesConnect));
    for(const job of jobs.data||[])imagesRemember(job);imagesRenderJobs();
    const saved=(files.data||[]).filter(file=>file.kind==='image');for(const file of imagesUi.savedFiles)if(!saved.some(item=>item.id===file.id))saved.unshift(file);imagesUi.savedFiles=saved;imagesRenderSaved(saved,ticket,root);
  }catch(error){if(imagesLibraryCurrent(ticket,root))$('images-jobs').replaceChildren(imagesText('p',imagesError(error),'images-error'));}syncShellNavigation();
}
function imagesRenderSaved(files,ticket,root){
  if(!imagesLibraryCurrent(ticket,root))return;const pageSize=24,last=Math.max(0,Math.ceil(files.length/pageSize)-1);imagesUi.savedPage=Math.min(imagesUi.savedPage,last);
  const grid=$('images-saved'),pages=$('images-saved-pages');grid.replaceChildren();pages.replaceChildren();imagesPrunePreviews();
  for(const file of files.slice(imagesUi.savedPage*pageSize,(imagesUi.savedPage+1)*pageSize))grid.append(imagesResultCard(file,()=>imagesLibraryCurrent(ticket,root)));
  if(!files.length)grid.append(imagesText('p','Generated and imported images will appear here.','empty-note'));
  if(last){const previous=imagesButton('Previous',async()=>{imagesUi.savedPage--;imagesRenderSaved(files,ticket,root);}),next=imagesButton('Next',async()=>{imagesUi.savedPage++;imagesRenderSaved(files,ticket,root);});previous.disabled=imagesUi.savedPage===0;next.disabled=imagesUi.savedPage===last;pages.append(previous,imagesText('span',(imagesUi.savedPage+1)+' / '+(last+1),'settings-sub'),next);}
}
function imagesRenderJobs(){
  const root=$('images-jobs');if(!imagesUi.library||!desktop.library||!root)return;root.replaceChildren();
  const jobs=Array.from(imagesUi.jobs.values()).filter(job=>{const matches=imagesUi.filter==='all'||(imagesUi.filter==='running'?imagesActive(job):imagesUi.filter==='cancelled'?['cancelled','interrupted'].includes(job.status):job.status===imagesUi.filter);return matches&&job.prompt.toLowerCase().includes(imagesUi.search.toLowerCase());}).sort((a,b)=>b.createdAt-a.createdAt);
  for(const job of jobs){const row=document.createElement('article');row.className='image-job-row';const open=imagesButton(job.prompt,()=>openImageJob(job.id),'image-job-open');const status=imagesText('span',job.status==='cancelled'?'Local waiting stopped':job.status,'image-job-status');row.append(open,status,imagesText('small',new Date(job.createdAt*1000).toLocaleString()));root.append(row);}
  if(!jobs.length)root.append(imagesText('p',imagesUi.filter==='all'?'No image requests yet. Create an image to start.':'No image requests match this filter.','empty-note'));
}
async function imagesSettings(){
  const request=++imagesUi.settingsRequest,box=$('settings-body');currentSettingsSection='images';document.querySelectorAll('.settings-nav-btn').forEach(button=>button.classList.toggle('active',button.dataset.sec==='images'));
  box.innerHTML='<h1 class="settings-h1">Images</h1><div id="images-settings"><p class="settings-sub">Loading image settings…</p></div>';const root=$('images-settings');
  try{const capabilities=await imagesCapabilities();if(request!==imagesUi.settingsRequest||currentSettingsSection!=='images'||!document.body.classList.contains('in-settings')||$('images-settings')!==root)return;
    root.replaceChildren(imagesText('p',capabilities.available?'OpenAI API key connected. Image model access is checked when you send a request.':capabilities.reason||'Connect an OpenAI API key.','settings-sub'));
    const actions=document.createElement('div');actions.className='image-actions';actions.append(imagesButton('Connect API key',imagesConnect),imagesButton('Open Images',()=>showImagesLibrary()),imagesButton('Create image',()=>openImageEditor()));root.append(actions,imagesText('p','ChatGPT sign-in does not provide access to the standard Images API. Image requests may incur API charges.','settings-sub'),imagesText('p','Up to '+(capabilities.limits?.maxActiveJobs||2)+' requests can run at once. A request can return up to '+(capabilities.limits?.maxImages||4)+' images. Cancellation stops local waiting; the provider may continue and charge.','settings-sub'),imagesText('p','Requests are never retried automatically. Saved images and request history remain in the local Library.','settings-sub'));
  }catch(error){if(request===imagesUi.settingsRequest&&$('images-settings')===root)root.replaceChildren(imagesText('p',imagesError(error),'images-error'));}
}
const imagesBaseNotification=handleServerNotification;
handleServerNotification=function(method,params){
  if(method==='images/job/updated'){
    const job=imagesRemember(params.job);if(!job)return;imagesRenderJobs();if(imagesUi.library&&$('images-library')){let added=false;for(const file of job.attachments||[])if(!imagesUi.savedFiles.some(saved=>saved.id===file.id)){imagesUi.savedFiles.unshift(file);added=true;}if(added)imagesRenderSaved(imagesUi.savedFiles,imagesUi.libraryRequest,$('images-library'));}const dialog=imagesUi.dialog;if(dialog?.jobId===job.id&&imagesDialogCurrent(dialog))imagesRenderJob(dialog.root,job,()=>imagesDialogCurrent(dialog));return;
  }
  if(method==='account/updated'){imagesUi.capabilities=null;if(currentSettingsSection==='images'&&document.body.classList.contains('in-settings'))imagesSettings().catch(showError);}
  imagesBaseNotification(method,params);
};
const imagesReady=desktopReady.then(()=>{
  const settings=document.querySelector('.settings-nav');if(settings){const button=imagesButton('Images',()=>renderSettingsSection('images'),'settings-nav-btn');button.dataset.sec='images';settings.append(button);}
  const baseSettings=renderSettingsSection;renderSettingsSection=async function(section){imagesUi.settingsRequest++;if(section==='images')return imagesSettings();return baseSettings(section);};
  const baseAttachment=showAttachmentMenu;showAttachmentMenu=function(event){baseAttachment(event);$('floating-menu').append(menuButton('Create image',()=>openImageEditor()));};
  for(const name of ['newThread','openThread','switchMode','openSettingsView','showLibrary','showFilesLibrary','openScheduledView','openProject','openDocument','openSearch','useWorkspace']){
    const base=window[name];if(typeof base!=='function')continue;window[name]=async function(...args){imagesInvalidate();const request=imagesUi.libraryRequest,result=await base.apply(this,args);if((name==='showLibrary'||name==='showFilesLibrary')&&request===imagesUi.libraryRequest&&desktop.library&&!imagesUi.library)$('work-library')?.querySelector('.library-header')?.append(imagesButton('Images',()=>showImagesLibrary()));return result;};
  }
});
imagesReady.catch(showError);
