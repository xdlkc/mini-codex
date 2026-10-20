// Provider catalog metadata controls the choices exposed by the composer.
const modelsUi={catalog:null,request:0,selection:0,saving:Promise.resolve(),tiers:new Map()};
function modelContextKey(){return (typeof remoteContext==='function'?remoteContext()||'local':'local')+'\0'+desktop.mode+'\0'+currentCwd;}
const modelsBaseCloseMenu=closeFloatingMenu;
closeFloatingMenu=function(){modelsBaseCloseMenu();$('floating-menu')?.classList.remove('model-catalog-menu');};
Object.assign(EFFORT_LABELS,{auto:'Auto',none:'None',minimal:'Minimal',low:'Low',medium:'Medium',high:'High',xhigh:'Extra High',max:'Max',ultra:'Ultra'});
function modelTierContext(){return typeof captureRpcContext==='function'?captureRpcContext().key:typeof remoteContext==='function'?remoteContext()||'local':'local';}
function selectedServiceTier(){return modelsUi.tiers.get(modelTierContext())||(fastMode?'priority':'default');}
function modelTiers(model){return model?.serviceTiers||model?.supportedServiceTiers||[];}
function modelFastTier(model){return modelTiers(model).find(tier=>['priority','fast'].includes(tier.id)||tier.name?.toLowerCase()==='fast');}
function modelName(model){return (model?.displayName||model?.model||currentModel).replace(/(GPT-[0-9.]+)-(.+)/i,'$1 $2');}
function modelSave(params){const call=typeof captureExecutionUi==='function'?captureExecutionUi().call:rpcCall;const pending=modelsUi.saving.catch(()=>{}).then(()=>call('settings/write',params));modelsUi.saving=pending;return pending;}
function applyModelCatalog(result,restoreSelection=false){
  modelCatalog=Array.isArray(result.data)?result.data:[];modelsUi.catalog=result.catalog||null;
  if(restoreSelection){currentModel=result.defaultModel||currentModel;currentEffort=result.defaultReasoningEffort||'auto';modelsUi.tiers.set(modelTierContext(),result.serviceTier||'default');fastMode=['fast','priority','ultrafast'].includes(result.serviceTier);}
  const selected=modelCatalog.find(model=>model.model===currentModel);if(selected?.reasoningMetadataKnown&&!modelEffortOptions(selected).some(option=>option.reasoningEffort===currentEffort))currentEffort=selected.defaultReasoningEffort||'auto';
  updateModelAndEffortUI();
}
async function refreshModelCatalog(refresh=false,restoreSelection=false){
  const request=++modelsUi.request,selection=modelsUi.selection,context=modelContextKey(),result=await rpcCall('model/list',{refresh,includeHidden:true},20000);
  if(request===modelsUi.request&&context===modelContextKey())applyModelCatalog(result,restoreSelection&&selection===modelsUi.selection);
  return result;
}
function modelCatalogNote(){
  const catalog=modelsUi.catalog;
  if(!catalog)return 'Configured models';
  if(['codex-cache','codex-bundled'].includes(catalog.source))return catalog.source==='codex-cache'?'Codex App model catalog':'Codex model catalog';
  if(catalog.stale)return 'Saved catalog · '+(catalog.error||'Refresh failed');
  if(!catalog.available)return catalog.error||'Provider catalog is not connected';
  return 'Available to this account · Updated '+new Date(catalog.fetchedAt*1000).toLocaleTimeString();
}
function modelEffortOptions(model){
  const reported=(model?.supportedReasoningEfforts||[]).filter(option=>option.reasoningEffort!=='persistent');
  if(model?.reasoningMetadataKnown&&reported.length)return reported;
  const options=[{reasoningEffort:'auto',label:'Auto',description:'Use the model’s default thinking effort.'}];
  if(currentEffort!=='auto')options.push({reasoningEffort:currentEffort,label:EFFORT_LABELS[currentEffort]||currentEffort,description:'Current manual setting. The provider has not confirmed support.'});
  return options;
}

toggleModelPicker=function(event){
  event.stopPropagation();const anchor=event.currentTarget;openFloatingMenuAt('model',anchor,'',true);if(!$('floating-menu').classList.contains('open'))return;
  const menu=$('floating-menu');menu.classList.add('model-catalog-menu');
  const title=document.createElement('div');title.className='menu-section-title';title.textContent='Select model';
  const status=document.createElement('p');status.className='model-catalog-note';status.textContent=modelCatalogNote();
  const search=document.createElement('input');search.className='menu-search';search.placeholder='Search models';search.setAttribute('aria-label','Search models');
  const list=document.createElement('div');list.className='model-catalog-list';
  const render=()=>{list.replaceChildren();for(const model of modelCatalog){if((model.hidden||model.upgrade)&&model.model!==currentModel)continue;if(![model.model,model.displayName].join(' ').toLowerCase().includes(search.value.toLowerCase()))continue;
    const button=menuButton('',()=>selectModel(model.model));button.classList.toggle('active',model.model===currentModel);
    const text=document.createElement('span');text.className='menu-item-text';const name=document.createElement('strong');name.textContent=modelName(model);
    const detail=document.createElement('small');detail.textContent=model.description||(model.source==='configured'?'Configured model':'');text.append(name,detail);button.append(text);
    if(model.model===currentModel){const check=document.createElement('span');check.textContent='✓';button.append(check);}list.append(button);
  }if(!list.children.length){const empty=document.createElement('p');empty.className='empty-note';empty.textContent='No matching models';list.append(empty);}};
  search.oninput=render;menu.append(title,status,search,list);
  const refresh=document.createElement('button');refresh.className='menu-item';refresh.textContent='Refresh models';refresh.onclick=async()=>{refresh.disabled=true;status.textContent='Refreshing models…';try{await refreshModelCatalog(true);if(menu.contains(status)){status.textContent=modelCatalogNote();render();}}catch(error){if(menu.contains(status))status.textContent=error.message;}finally{refresh.disabled=false;}};menu.append(refresh);
  const selected=modelCatalog.find(model=>model.model===currentModel);
  if(modelFastTier(selected))menu.append(menuButton((fastMode?'✓ ':'')+'Fast mode',()=>toggleFastMode()));
  render();search.focus();
};
selectModel=async function(id){
  const request=++modelsUi.selection,context=modelContextKey(),tierContext=modelTierContext();
  const model=modelCatalog.find(model=>model.model===id),efforts=(model?.supportedReasoningEfforts||[]).map(item=>item.reasoningEffort);
  const effort=efforts.includes(currentEffort)?currentEffort:model?.defaultReasoningEffort||'auto';
  const tier=modelTiers(model).some(option=>option.id===selectedServiceTier())?selectedServiceTier():model?.defaultServiceTier||'default';
  await modelSave({model:id,reasoningEffort:effort,serviceTier:tier});modelsUi.tiers.set(tierContext,tier);if(request!==modelsUi.selection||context!==modelContextKey())return;currentModel=id;currentEffort=effort;fastMode=tier!=='default';updateModelAndEffortUI();closeFloatingMenu();
};
toggleEffortPicker=function(event){
  event.stopPropagation();const model=modelCatalog.find(item=>item.model===currentModel);openFloatingMenuAt('effort',event.currentTarget,'',true);const menu=$('floating-menu');if(!menu.classList.contains('open'))return;
  const title=document.createElement('div');title.className='menu-section-title';title.textContent='Reasoning effort';menu.append(title);
  if(!model?.reasoningMetadataKnown){const note=document.createElement('p');note.className='model-catalog-note';note.textContent='This catalog does not report supported reasoning levels.';menu.append(note);}
  for(const option of modelEffortOptions(model)){const button=menuButton('',()=>selectEffort(option.reasoningEffort));const text=document.createElement('span');text.className='menu-item-text';const name=document.createElement('strong');name.textContent=(option.reasoningEffort===currentEffort?'✓ ':'')+(EFFORT_LABELS[option.reasoningEffort]||option.label||option.reasoningEffort);const detail=document.createElement('small');detail.textContent=option.description||'';text.append(name,detail);button.append(text);menu.append(button);}
};
selectEffort=async function(effort){
  const request=++modelsUi.selection,context=modelContextKey();
  const model=modelCatalog.find(item=>item.model===currentModel);if(!modelEffortOptions(model).some(item=>item.reasoningEffort===effort))throw new Error('This effort is not reported for the selected model');
  await modelSave({model:currentModel,reasoningEffort:effort,serviceTier:selectedServiceTier()});if(request!==modelsUi.selection||context!==modelContextKey())return;currentEffort=effort;updateModelAndEffortUI();closeFloatingMenu();
};
toggleFastMode=async function(){
  const model=modelCatalog.find(item=>item.model===currentModel),fast=modelFastTier(model);if(!fast)throw new Error('Fast mode is not available for this model');
  return selectServiceTier(selectedServiceTier()===fast.id?'default':fast.id);
};
async function selectServiceTier(tier){
  const request=++modelsUi.selection,context=modelContextKey(),tierContext=modelTierContext(),model=modelCatalog.find(item=>item.model===currentModel);
  if(tier!=='default'&&!modelTiers(model).some(option=>option.id===tier))throw new Error('This service tier is not available for the selected model');
  await modelSave({model:currentModel,reasoningEffort:currentEffort,serviceTier:tier});modelsUi.tiers.set(tierContext,tier);if(request!==modelsUi.selection||context!==modelContextKey())return;
  fastMode=tier!=='default';updateModelAndEffortUI();closeFloatingMenu();
}
const modelsBaseSettings=renderSettingsSection;
renderSettingsSection=async function(section){
  await modelsBaseSettings(section);if(section!=='configuration'||currentSettingsSection!==section)return;
  const card=$('settings-body').querySelector('.settings-card'),model=modelCatalog.find(item=>item.model===currentModel);
  if(card){
    const rows=card.querySelectorAll('.settings-row');
    const bind=(row,label,choices,value,change)=>{const prior=row.querySelector('select,button');if(prior)prior.remove();const select=document.createElement('select');select.className='settings-select';select.setAttribute('aria-label',label);for(const choice of choices){const option=document.createElement('option');option.value=choice.value;option.textContent=choice.label;select.append(option);}select.value=value;select.disabled=choices.length===0;select.onchange=async()=>{const context=modelContextKey();select.disabled=true;try{await change(select.value);if(context===modelContextKey()&&currentSettingsSection==='configuration')await renderSettingsSection('configuration');}catch(error){showError(error);select.disabled=false;}};row.append(select);};
    bind(rows[0],'Default model',modelCatalog.filter(item=>!item.hidden||item.model===currentModel).map(item=>({value:item.model,label:modelName(item)})),currentModel,selectModel);
    bind(rows[1],'Reasoning effort',modelEffortOptions(model).map(option=>({value:option.reasoningEffort,label:EFFORT_LABELS[option.reasoningEffort]||option.label||option.reasoningEffort})),currentEffort,selectEffort);
    rows[2].querySelector('.settings-row-label').textContent='Speed';
    bind(rows[2],'Speed',[{value:'default',label:'Standard'},...modelTiers(model).map(tier=>({value:tier.id,label:tier.name}))],selectedServiceTier(),selectServiceTier);
    const inherit=document.createElement('button');inherit.className='btn-pill';inherit.textContent='Use Codex model defaults';inherit.onclick=async()=>{const context=modelContextKey();await modelSave({inheritModelDefaults:true});if(context!==modelContextKey())return;await refreshModelCatalog(false,true);if(context===modelContextKey()&&currentSettingsSection==='configuration')await renderSettingsSection('configuration');};card.append(inherit);
  }
  const status=document.createElement('p');status.className='model-catalog-note';status.textContent=modelCatalogNote();
  const refresh=document.createElement('button');refresh.className='btn-pill';refresh.textContent='Refresh available models';refresh.onclick=async()=>{refresh.disabled=true;try{await refreshModelCatalog(true);if(currentSettingsSection===section)status.textContent=modelCatalogNote();}catch(error){showError(error);}finally{refresh.disabled=false;}};$('settings-body').append(status,refresh);
};
const modelsBaseNotification=handleServerNotification;
handleServerNotification=function(method,params){modelsBaseNotification(method,params);if(method==='account/updated')refreshModelCatalog(false,true).catch(showError);};
