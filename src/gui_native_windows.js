// Auxiliary windows share the Rust runner, with independent persisted drafts.
const nativeWindowInfo=window.__mcodexWindow||{kind:'main'};
const nativeDraftKeys=['drafts','attachmentDrafts','mentionDrafts','webSearchDrafts','sidebarCollapsed','sidebarWidth'];
function nativeScopedPreferences(message){
  const kind=nativeWindowInfo.kind;if(kind==='main')return message;
  if(message.method==='desktop/settings/update'){
    const params={...(message.params||{})},preferences={...(params.preferences||{})};
    if(params.mode!==undefined){preferences['native_'+kind+'_mode']=params.mode;delete params.mode;}
    for(const key of nativeDraftKeys)if(Object.hasOwn(preferences,key)){preferences['native_'+kind+'_'+key]=preferences[key];delete preferences[key];}
    message={...message,params:{...params,preferences}};
  }
  return message;
}
const nativeBaseSend=rpcSend;
rpcSend=function(message){const scoped=nativeScopedPreferences(message);return nativeBaseSend(window.__nativeToken?{...scoped,_nativeToken:window.__nativeToken}:scoped);};
const nativeBaseMessage=window.__onServerMessage;
window.__onServerMessage=function(message){
  const settings=message.result?.settings,kind=nativeWindowInfo.kind;
  if(kind!=='main'&&settings?.preferences){
    const preferences={...settings.preferences};
    for(const key of nativeDraftKeys)preferences[key]=preferences['native_'+kind+'_'+key]??(key==='sidebarCollapsed'?true:key==='sidebarWidth'?260:{});
    message={...message,result:{...message.result,settings:{...settings,mode:preferences['native_'+kind+'_mode']||'chat',preferences}}};
  }
  return nativeBaseMessage(message);
};
