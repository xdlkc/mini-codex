// Codex sidebar contracts from the installed 26.1013.40934 app:
// app-initial-90eb42395b7d.js, sidebarElectron.groupByMenu / sortMenu.
// Load after gui_remote.js so this wraps the final transport-aware refresh.
const codexSidebarUi={request:0,cache:new Map(),preferences:null,write:Promise.resolve(),drag:null};
const codexSidebarInitialLimit=5;
function codexSidebarText(en,zh){return typeof codexText==='function'?codexText(en,zh):en;}
function codexSidebarHost(){return typeof remoteContext==='function'?remoteContext()||'local':'local';}
function codexSidebarPreferences(){
  if(!codexSidebarUi.preferences){
    const saved=desktop.state?.settings?.preferences?.codexSidebar||{};
    const sections=Array.isArray(saved.sections)?saved.sections.filter(section=>section&&typeof section.id==='string'&&section.id&&typeof section.name==='string'&&section.name.trim()).map(section=>({id:section.id,name:section.name.trim()})):[];
    const sectionIds=new Set(sections.map(section=>section.id));
    const placements=value=>Object.fromEntries(Object.entries(value&&typeof value==='object'?value:{}).filter(([,id])=>id===null||sectionIds.has(id)));
    codexSidebarUi.preferences={
      grouping:['project','connection','list'].includes(saved.grouping)?saved.grouping:'connection',
      sort:['updated_at','created_at','manual'].includes(saved.sort)?saved.sort:'updated_at',
      pinnedSort:['updated_at','manual'].includes(saved.pinnedSort)?saved.pinnedSort:'manual',
      order:Array.isArray(saved.order)?saved.order.filter(x=>typeof x==='string'):[],
      pinnedOrder:Array.isArray(saved.pinnedOrder)?saved.pinnedOrder.filter(x=>typeof x==='string'):[],
      collapsed:saved.collapsed&&typeof saved.collapsed==='object'?{...saved.collapsed}:{},
      limits:saved.limits&&typeof saved.limits==='object'?{...saved.limits}:{},
      sections,threadSections:placements(saved.threadSections),projectSections:placements(saved.projectSections),
    };
  }
  return codexSidebarUi.preferences;
}
function codexSidebarSave(patch){
  codexSidebarUi.preferences={...codexSidebarPreferences(),...patch};
  const value=JSON.parse(JSON.stringify(codexSidebarUi.preferences));
  if(desktop.state?.settings?.preferences)desktop.state.settings.preferences.codexSidebar=value;
  renderCodexSidebar();
  // Serial writes keep a quick second choice from being overwritten by the first.
  codexSidebarUi.write=codexSidebarUi.write.catch(()=>{}).then(()=>savePreferences({codexSidebar:value}));
  codexSidebarUi.write.catch(showError);return codexSidebarUi.write;
}
function codexSidebarKey(host,id){return JSON.stringify([host,id]);}
function codexSidebarTimestamp(value){
  if(typeof value==='number')return value<1e12?value*1000:value;
  if(typeof value==='string'){const numeric=Number(value);if(value.trim()&&Number.isFinite(numeric))return codexSidebarTimestamp(numeric);return Date.parse(value)||0;}
  return 0;
}
function codexSidebarSorted(items,sort,order){
  const positions=new Map(order.map((key,index)=>[key,index]));
  return [...items].sort((a,b)=>{
    if(sort==='manual'){
      const rank=(positions.get(a.key)??Infinity)-(positions.get(b.key)??Infinity);
      if(!Number.isNaN(rank)&&rank!==0)return rank;
    }
    const field=sort==='created_at'?'createdAt':'updatedAt';
    const delta=codexSidebarTimestamp(b.thread[field]??b.thread[field==='createdAt'?'created_at':'updated_at'])-codexSidebarTimestamp(a.thread[field]??a.thread[field==='createdAt'?'created_at':'updated_at']);
    return delta||a.key.localeCompare(b.key);
  });
}
function codexSidebarProfile(host){return typeof remoteUi!=='undefined'?remoteUi.profiles.find(profile=>profile.id===host):null;}
function codexSidebarConnectionState(host){
  if(host==='local')return '';
  return typeof remoteUi!=='undefined'?remoteUi.connections.get(host)?.state||'disconnected':'disconnected';
}
function codexSidebarLabel(host){return host==='local'?codexSidebarText('Local','本地'):codexSidebarProfile(host)?.name||codexSidebarText('SSH host','SSH 主机');}
function codexSidebarCollect(){
  const items=[];
  for(const [host,snapshot] of codexSidebarUi.cache){
    for(const thread of snapshot.threads){
      if(thread.mode!=='codex'||thread.archived)continue;
      // A collided resource must retain the transport's existing blocked behavior.
      items.push({host,key:codexSidebarKey(host,thread.id),thread});
    }
  }
  return items;
}
function codexSidebarProjects(host){
  return (desktop.state?.projects||[]).filter(project=>project.mode==='codex'&&(
    project.hostId||(typeof remoteUi==='undefined'?'local':remoteUi.projects.get(project.id)||'local'))===host);
}
function codexSidebarProjectForItem(item){
  const project=codexSidebarProjects(item.host).find(project=>project.id===item.thread.projectId);
  const projectId=project?.id||(typeof item.thread.projectId==='string'&&item.thread.projectId)||null;
  const cwd=typeof item.thread.cwd==='string'?item.thread.cwd.replace(/\/+$/,'')||'/':'';
  return {key:codexSidebarKey(item.host,projectId?'project:'+projectId:cwd?'cwd:'+cwd:'chats'),label:project?.name||(cwd?cwd.split('/').pop()||cwd:codexSidebarText('Chats','聊天')),host:item.host,project,kind:projectId||cwd?'project':'chats'};
}
function codexSidebarGroups(items,grouping){
  if(grouping==='list')return [{key:'chats',label:codexSidebarText('Chats','聊天'),kind:'chats',items}];
  const groups=new Map();
  const add=(key,label,host,project,kind=grouping)=>{if(!groups.has(key))groups.set(key,{key,label,host,project,kind,items:[]});return groups.get(key);};
  if(grouping==='connection'){
    for(const [host] of codexSidebarUi.cache)add('connection:'+host,codexSidebarLabel(host),host);
    for(const item of items)add('connection:'+item.host,codexSidebarLabel(item.host),item.host).items.push(item);
  }else{
    for(const [host] of codexSidebarUi.cache){
      for(const project of codexSidebarProjects(host))add(codexSidebarKey(host,'project:'+project.id),project.name,host,project);
    }
    for(const item of items){
      const group=codexSidebarProjectForItem(item);add(group.key,group.label,group.host,group.project,group.kind).items.push(item);
    }
  }
  return [...groups.values()];
}
function codexSidebarSectionForItem(item){
  const preferences=codexSidebarPreferences(),own=preferences.threadSections;
  const id=Object.prototype.hasOwnProperty.call(own,item.key)?own[item.key]:preferences.projectSections[codexSidebarProjectForItem(item).key];
  return preferences.sections.some(section=>section.id===id)?id:null;
}
function codexSidebarSectionViews(items){
  const preferences=codexSidebarPreferences(),projects=codexSidebarGroups(items,'project');
  return preferences.sections.map(section=>{
    const children=projects.filter(project=>project.kind==='project'&&preferences.projectSections[project.key]===section.id).map(project=>({...project,items:project.items.filter(item=>codexSidebarSectionForItem(item)===section.id),sectionId:section.id}));
    const nested=new Set(children.flatMap(project=>project.items.map(item=>item.key)));
    return {key:'custom:'+section.id,label:section.name,kind:'custom',sectionId:section.id,items:items.filter(item=>codexSidebarSectionForItem(item)===section.id&&!nested.has(item.key)),children};
  });
}
function codexSidebarRequireHost(host,threadId){
  if(host!=='local'){
    if(typeof remoteUi==='undefined'||remoteUi.connections.get(host)?.state!=='connected')throw new Error(codexSidebarText('The execution host is disconnected. Connect to it before moving this item.','执行主机已断开。请先连接，再移动此项目。'));
  }else if(codexSidebarUi.cache.get(host)?.error)throw new Error(codexSidebarText('The local execution host is unavailable. Refresh it before moving this item.','本地执行主机不可用。请刷新后再移动此项目。'));
  if(threadId&&typeof remoteUi!=='undefined'){
    if(remoteUi.ambiguous?.has(threadId)||remoteUi.threads.has(threadId)&&(remoteUi.threads.get(threadId)||'local')!==host)throw new Error(codexSidebarText('This chat has conflicting host ownership. Refresh its connection before continuing.','此聊天的主机归属冲突。请刷新连接后再继续。'));
  }
}
function codexSidebarTarget(target){
  if(target.kind==='thread'){
    const item=codexSidebarCollect().find(item=>item.key===target.key);if(!item)throw new Error(codexSidebarText('This chat is no longer available. Refresh the sidebar.','此聊天已不可用。请刷新侧边栏。'));
    codexSidebarRequireHost(item.host,item.thread.id);return item;
  }
  const group=codexSidebarGroups(codexSidebarCollect(),'project').find(group=>group.key===target.key&&group.kind==='project');
  if(!group)throw new Error(codexSidebarText('This project is no longer available. Refresh the sidebar.','此项目已不可用。请刷新侧边栏。'));
  codexSidebarRequireHost(group.host);return group;
}
async function codexSidebarPrepareMove(target,sectionId){
  const item=codexSidebarTarget(target);
  // Native custom-section moves unpin a pinned chat before changing placement.
  if(sectionId!==null&&target.kind==='thread'&&item.thread.pinned){
    const params={threadId:item.thread.id,pinned:false};
    if(item.host==='local')await (typeof remoteLocalRpc==='function'?remoteLocalRpc:rpcCall)('desktop/threadMetadata/set',params);
    else await remoteCall(remoteUi.connections.get(item.host),'desktop/threadMetadata/set',params);
    item.thread.pinned=false;
    for(const thread of codexSidebarUi.cache.get(item.host)?.threads||[])if(thread.id===item.thread.id)thread.pinned=false;
  }
  return item;
}
async function codexSidebarMoveToSection(target,sectionId){
  if(sectionId!==null&&!codexSidebarPreferences().sections.some(section=>section.id===sectionId))throw new Error(codexSidebarText('This section was removed. Choose another section.','此分区已移除。请选择其他分区。'));
  await codexSidebarPrepareMove(target,sectionId);
  codexSidebarTarget(target);
  const preferences=codexSidebarPreferences();
  if(sectionId!==null&&!preferences.sections.some(section=>section.id===sectionId))throw new Error(codexSidebarText('This section was removed. Choose another section.','此分区已移除。请选择其他分区。'));
  const field=target.kind==='thread'?'threadSections':'projectSections';
  await codexSidebarSave({[field]:{...preferences[field],[target.key]:sectionId}});
}
async function codexSidebarCreateSection(name,target=null){
  name=String(name).trim();if(!name)throw new Error(codexSidebarText('Enter a section name.','请输入分区名称。'));
  if(target)await codexSidebarPrepareMove(target,'new');
  if(target)codexSidebarTarget(target);
  const id='section-'+(globalThis.crypto?.randomUUID?.()||Date.now().toString(36)+'-'+Math.random().toString(36).slice(2)),preferences=codexSidebarPreferences();
  const patch={sections:[...preferences.sections,{id,name}]};
  if(target){const field=target.kind==='thread'?'threadSections':'projectSections';patch[field]={...preferences[field],[target.key]:id};}
  await codexSidebarSave(patch);return id;
}
function codexSidebarRenameSection(id,name){
  name=String(name).trim();if(!name)throw new Error(codexSidebarText('Enter a section name.','请输入分区名称。'));
  const preferences=codexSidebarPreferences();if(!preferences.sections.some(section=>section.id===id))throw new Error(codexSidebarText('This section was removed.','此分区已移除。'));
  return codexSidebarSave({sections:preferences.sections.map(section=>section.id===id?{...section,name}:section)});
}
function codexSidebarDeleteSection(id){
  const preferences=codexSidebarPreferences(),without=value=>Object.fromEntries(Object.entries(value).filter(([,section])=>section!==id));
  const collapsed={...preferences.collapsed},limits={...preferences.limits};delete collapsed['custom:'+id];delete limits['custom:'+id];
  return codexSidebarSave({sections:preferences.sections.filter(section=>section.id!==id),threadSections:without(preferences.threadSections),projectSections:without(preferences.projectSections),collapsed,limits});
}
function codexSidebarSectionDialog(section=null,target=null){
  openCustomModal(section?codexSidebarText('Rename section','重命名分区'):codexSidebarText('New section','新建分区'),'');
  const body=$('modal-body'),form=codexSidebarElement('form','codex-sidebar-section-form');
  const label=codexSidebarElement('label','',codexSidebarText('Section name','分区名称')),input=codexSidebarElement('input','modal-input');input.id='codex-section-name';input.value=section?.name||'';input.required=true;input.autocomplete='off';label.htmlFor=input.id;
  const error=codexSidebarElement('p','codex-sidebar-section-error');error.setAttribute('role','alert');
  const actions=codexSidebarElement('div','codex-sidebar-section-actions'),cancel=codexSidebarElement('button','btn-pill',codexSidebarText('Cancel','取消')),save=codexSidebarElement('button','btn-pill primary',section?codexSidebarText('Save','保存'):codexSidebarText('Create section','创建分区'));cancel.type='button';cancel.onclick=()=>closeModal();save.type='submit';actions.append(cancel,save);form.append(label,input,error,actions);body.append(form);
  form.onsubmit=async event=>{event.preventDefault();if(!input.value.trim()){input.focus();return;}save.disabled=true;try{if(section)await codexSidebarRenameSection(section.id,input.value);else await codexSidebarCreateSection(input.value,target);if(input.isConnected!==false)closeModal();}catch(failure){error.textContent=failure.message;save.disabled=false;}};input.focus();
}
function codexSidebarElement(tag,className,text){const node=document.createElement(tag);if(className)node.className=className;if(text!==undefined)node.textContent=text;return node;}
function codexSidebarIcon(name){
  const node=codexSidebarElement('span','codex-sidebar-icon');node.setAttribute('aria-hidden','true');
  if(name==='chevron'&&typeof CHEV_SVG==='string'){node.innerHTML=CHEV_SVG;node.classList.add('codex-sidebar-chevron');}
  else if(name==='folder'&&typeof FOLDER_SVG==='string')node.innerHTML=FOLDER_SVG;
  else if(name==='connection'&&typeof ICONS!=='undefined'&&ICONS.term)node.innerHTML=ICONS.term;
  else if(typeof shellSvg==='function')node.innerHTML=shellSvg(name==='options'?'more':'apps');
  else if(name==='options')node.textContent='…';
  return node;
}
function codexSidebarAction(label,run){
  const button=codexSidebarElement('button','menu-item',label);button.type='button';button.setAttribute('role','menuitem');
  button.onclick=()=>{closeFloatingMenu();return Promise.resolve().then(run).catch(showError);};return button;
}
function codexSidebarMenuKeyboard(menu,anchor){
  menu.classList.add('codex-sidebar-menu');menu.setAttribute('role','menu');codexSidebarUi.menuAnchor=anchor;
  menu.onkeydown=event=>{
    const buttons=Array.from(menu.querySelectorAll('button:not(:disabled)'));
    const current=buttons.indexOf(document.activeElement);let next;
    if(event.key==='ArrowDown')next=(current+1)%buttons.length;
    if(event.key==='ArrowUp')next=(current-1+buttons.length)%buttons.length;
    if(event.key==='Home')next=0;if(event.key==='End')next=buttons.length-1;
    if(next!==undefined){event.preventDefault();buttons[next]?.focus();}
    if(event.key==='Escape'){event.preventDefault();closeFloatingMenu();anchor.focus();}
  };
  (menu.querySelector('[aria-checked="true"]')||menu.querySelector('button'))?.focus();
}
function codexSidebarMoveMenu(anchor,target){
  const item=codexSidebarTarget(target),preferences=codexSidebarPreferences();
  const selected=target.kind==='thread'?codexSidebarSectionForItem(item):preferences.projectSections[target.key];
  openFloatingMenuAt('codex-section-move:'+target.key,anchor,'',false);const menu=$('floating-menu');if(!menu.classList.contains('open'))return;
  menu.append(codexSidebarElement('div','menu-section-title',codexSidebarText('Move to section','移到分区')));
  for(const section of preferences.sections){
    const button=codexSidebarAction(section.name,()=>codexSidebarMoveToSection(target,section.id===selected?null:section.id));button.setAttribute('role','menuitemradio');button.setAttribute('aria-checked',String(section.id===selected));button.dataset.sectionId=section.id;menu.append(button);
  }
  if(selected)menu.append(codexSidebarAction(codexSidebarText('Remove from section','从分区移除'),()=>codexSidebarMoveToSection(target,null)));
  menu.append(codexSidebarAction(codexSidebarText('New section…','新建分区…'),()=>codexSidebarSectionDialog(null,target)));codexSidebarMenuKeyboard(menu,anchor);
}
function codexSidebarGroupMenu(anchor,group){
  if(group.kind==='custom'){
    const section=codexSidebarPreferences().sections.find(section=>section.id===group.sectionId);if(!section)return;
    openFloatingMenuAt('codex-section:'+section.id,anchor,'',false);const menu=$('floating-menu');if(!menu.classList.contains('open'))return;
    menu.append(codexSidebarAction(codexSidebarText('Rename section','重命名分区'),()=>codexSidebarSectionDialog(section)),codexSidebarAction(codexSidebarText('Remove section','移除分区'),()=>codexSidebarDeleteSection(section.id)));codexSidebarMenuKeyboard(menu,anchor);return;
  }
  const target={kind:'project',key:group.key};codexSidebarTarget(target);
  if(group.project&&typeof codexProjectMenu==='function')codexProjectMenu(group.project,anchor);
  else openFloatingMenuAt('codex-project-section:'+group.key,anchor,'',false);
  const menu=$('floating-menu');if(!menu.classList.contains('open'))return;
  menu.append(codexSidebarAction(codexSidebarText('Move to section','移到分区'),()=>codexSidebarMoveMenu(anchor,target)));codexSidebarMenuKeyboard(menu,anchor);
}
function codexSidebarMenu(anchor,pinned=false){
  openFloatingMenuAt(pinned?'codex-pinned-options':'codex-sidebar-options',anchor,'',false);
  const menu=$('floating-menu');if(!menu.classList.contains('open'))return;
  menu.classList.add('codex-sidebar-menu');menu.setAttribute('role','menu');
  codexSidebarUi.menuAnchor=anchor;
  const preferences=codexSidebarPreferences();
  const add=(label,choices,selected,select)=>{
    const heading=codexSidebarElement('div','menu-section-title',label);heading.setAttribute('role','presentation');menu.append(heading);
    for(const [value,text] of choices){
      const button=codexSidebarElement('button','menu-item codex-sidebar-menu-option');button.type='button';button.setAttribute('role','menuitemradio');button.setAttribute('aria-checked',String(value===selected));
      button.append(codexSidebarElement('span','codex-sidebar-check',value===selected?'✓':''),codexSidebarElement('span','',text));
      button.onclick=()=>{closeFloatingMenu();select(value);(pinned?document.querySelector('.codex-sidebar-pinned-options'):$('codex-sidebar-options'))?.focus();};menu.append(button);
    }
  };
  if(!pinned)add(codexSidebarText('Organize sidebar','组织侧边栏'),[['project',codexSidebarText('By project','按项目')],['connection',codexSidebarText('By connection','按连接')],['list',codexSidebarText('In one list','在一个列表中')]],preferences.grouping,value=>codexSidebarSave({grouping:value}));
  const updated=['updated_at',codexSidebarText('Last updated','最后更新')],manual=['manual',codexSidebarText('Manual order','手动排序')];
  add(pinned?codexSidebarText('Sort pinned chats by','置顶聊天排序方式'):codexSidebarText('Sort chats by','聊天排序方式'),pinned?[updated,manual]:[updated,['created_at',codexSidebarText('Created','创建时间')],manual],pinned?preferences.pinnedSort:preferences.sort,value=>codexSidebarSave(pinned?{pinnedSort:value}:{sort:value}));
  if(!pinned)menu.append(codexSidebarAction(codexSidebarText('New section','新建分区'),()=>codexSidebarSectionDialog()));
  codexSidebarMenuKeyboard(menu,anchor);
}
function codexSidebarManualMove(item,targetKey,after=false){
  const preferences=codexSidebarPreferences(),pinned=Boolean(item.thread.pinned),sort=pinned?preferences.pinnedSort:preferences.sort;
  if(sort!=='manual')return;
  const field=pinned?'pinnedOrder':'order',items=codexSidebarSorted(codexSidebarCollect().filter(other=>Boolean(other.thread.pinned)===pinned),'manual',preferences[field]);
  if(!items.some(other=>other.key===targetKey)||item.key===targetKey)return;
  const keys=items.map(other=>other.key).filter(key=>key!==item.key),index=keys.indexOf(targetKey);
  keys.splice(index+(after?1:0),0,item.key);codexSidebarSave({[field]:keys});
}
function codexSidebarRow(item,group){
  const row=threadRow(item.thread);row.dataset.codexThreadKey=item.key;
  row.title=codexSidebarLabel(item.host);
  const preferences=codexSidebarPreferences(),pinned=Boolean(item.thread.pinned),manual=(pinned?preferences.pinnedSort:preferences.sort)==='manual';
  if(manual){
    row.draggable=true;row.title=codexSidebarText('Drag to reorder, or press Option + ↑ / ↓','拖动排序，或按 Option + ↑ / ↓');
    row.addEventListener('dragstart',event=>{codexSidebarUi.drag=item;event.dataTransfer?.setData('text/plain',item.key);if(event.dataTransfer)event.dataTransfer.effectAllowed='move';});
    row.addEventListener('dragover',event=>{if(codexSidebarUi.drag&&Boolean(codexSidebarUi.drag.thread.pinned)===pinned){event.preventDefault();row.classList.add('codex-sidebar-drop-target');}});
    row.addEventListener('dragleave',()=>row.classList.remove('codex-sidebar-drop-target'));
    row.addEventListener('drop',event=>{event.preventDefault();row.classList.remove('codex-sidebar-drop-target');if(codexSidebarUi.drag)codexSidebarManualMove(codexSidebarUi.drag,item.key);codexSidebarUi.drag=null;});
    row.addEventListener('dragend',()=>{codexSidebarUi.drag=null;document.querySelectorAll('.codex-sidebar-drop-target').forEach(node=>node.classList.remove('codex-sidebar-drop-target'));});
    row.addEventListener('keydown',event=>{
      if(!event.altKey||!['ArrowUp','ArrowDown'].includes(event.key)||event.target!==row)return;
      event.preventDefault();const index=group.items.findIndex(other=>other.key===item.key),target=group.items[index+(event.key==='ArrowUp'?-1:1)];
      if(target){codexSidebarManualMove(item,target.key,event.key==='ArrowDown');Array.from($('thread-list').querySelectorAll('[data-codex-thread-key]')).find(node=>node.dataset.codexThreadKey===item.key)?.focus();}
    });
  }
  return row;
}
function codexSidebarRenderGroup(group,list,pinned=false){
  const groupKey=group.kind==='project'&&group.sectionId?'custom:'+group.sectionId+':'+group.key:group.key;
  const preferences=codexSidebarPreferences(),collapsed=Boolean(preferences.collapsed[groupKey]);
  const section=codexSidebarElement('section','codex-sidebar-group');section.dataset.codexGroup=groupKey;if(group.kind==='custom')section.classList.add('codex-sidebar-custom-section');
  const header=codexSidebarElement('div','codex-sidebar-group-header');
  const toggle=codexSidebarElement('button','codex-sidebar-group-toggle');toggle.type='button';toggle.setAttribute('aria-expanded',String(!collapsed));
  toggle.append(codexSidebarIcon('chevron'));
  if(!['project','connection'].includes(group.kind))toggle.classList.add('codex-sidebar-group-no-icon');
  if(group.kind==='project')toggle.append(codexSidebarIcon('folder'));
  if(group.kind==='connection')toggle.append(codexSidebarIcon('connection'));
  toggle.append(codexSidebarElement('span','codex-sidebar-group-name',group.label));
  toggle.onclick=()=>{codexSidebarSave({collapsed:{...preferences.collapsed,[groupKey]:!collapsed}});Array.from(list.querySelectorAll('[data-codex-group]')).find(node=>node.dataset.codexGroup===groupKey)?.querySelector('button')?.focus();};
  header.append(toggle);
  if(group.kind==='project'&&group.project){
    const open=codexSidebarElement('button','thread-act-btn codex-sidebar-open-project');open.type='button';open.setAttribute('aria-label',codexSidebarText('Open project','打开项目')+' '+group.label);open.append(codexSidebarIcon('folder'));open.onclick=()=>Promise.resolve().then(()=>{codexSidebarRequireHost(group.host);return openProject(group.project);}).catch(showError);header.append(open);
  }
  if(group.host){
    const snapshot=codexSidebarUi.cache.get(group.host),connection=codexSidebarConnectionState(group.host);
    const status=snapshot?.error?codexSidebarText('Could not refresh','无法刷新'):connection&&connection!=='connected'?codexSidebarText('Disconnected','已断开'):'';
    if(status){const state=codexSidebarElement('span','codex-sidebar-host-state',status);state.title=snapshot?.error||status;header.append(state);}
    else if(group.kind==='project'&&group.host!=='local'){const name=codexSidebarElement('span','codex-sidebar-host-state',codexSidebarLabel(group.host));header.append(name);}
  }
  if(pinned){const options=codexSidebarElement('button','thread-act-btn codex-sidebar-pinned-options','…');options.type='button';options.setAttribute('aria-label',codexSidebarText('Pinned options','置顶选项'));options.onclick=()=>codexSidebarMenu(options,true);header.append(options);}
  if(['custom','project'].includes(group.kind)){
    const options=codexSidebarElement('button','thread-act-btn codex-sidebar-group-options','…');options.type='button';options.setAttribute('aria-label',(group.kind==='custom'?codexSidebarText('Section options','分区选项'):codexSidebarText('Project actions','项目操作'))+' '+group.label);options.onclick=()=>Promise.resolve().then(()=>codexSidebarGroupMenu(options,group)).catch(showError);header.append(options);
    header.oncontextmenu=event=>{event.preventDefault();Promise.resolve().then(()=>codexSidebarGroupMenu(options,group)).catch(showError);};
  }
  section.append(header);
  if(!collapsed){
    const limit=Math.max(codexSidebarInitialLimit,Number(preferences.limits[groupKey])||0),body=codexSidebarElement('div','codex-sidebar-group-items');
    for(const item of group.items.slice(0,limit))body.append(codexSidebarRow(item,group));
    if(group.items.length>codexSidebarInitialLimit){const expanded=limit>=group.items.length;const more=codexSidebarElement('button','codex-sidebar-show-more',expanded?codexSidebarText('Show less','显示更少'):codexSidebarText('Show more','展开显示'));more.type='button';more.setAttribute('aria-label',group.label+' · '+more.textContent);more.onclick=()=>{codexSidebarSave({limits:{...preferences.limits,[groupKey]:expanded?codexSidebarInitialLimit:limit+codexSidebarInitialLimit}});};body.append(more);}
    for(const child of group.children||[])codexSidebarRenderGroup(child,body);
    if(!group.items.length&&!group.children?.length){const empty=codexSidebarElement('div','codex-sidebar-empty',group.kind==='custom'?codexSidebarText('No chats or projects','暂无聊天或项目'):codexSidebarText('No chats','暂无聊天'));body.append(empty);}
    section.append(body);
  }
  list.append(section);
}
function renderCodexSidebar(){
  const list=$('thread-list'),title=$('sidebar-section-title');if(!list)return;
  if(desktop.mode!=='codex'){$('codex-sidebar-options')?.remove();list.classList.remove('codex-sidebar-list');return;}
  const preferences=codexSidebarPreferences(),items=codexSidebarCollect();
  list.classList.add('codex-sidebar-list');list.replaceChildren();if(title)title.textContent=preferences.grouping==='connection'?codexSidebarText('Connections','连接'):preferences.grouping==='project'?codexSidebarText('Projects','项目'):codexSidebarText('Chats','聊天');
  if(!$('codex-sidebar-options')&&title?.parentElement){
    const button=codexSidebarElement('button','thread-act-btn codex-sidebar-options');button.id='codex-sidebar-options';button.type='button';button.title=codexSidebarText('Sidebar options','侧边栏选项');button.setAttribute('aria-label',button.title);button.append(codexSidebarIcon('options'));button.onclick=()=>codexSidebarMenu(button);title.parentElement.append(button);
  }
  const pinned=codexSidebarSorted(items.filter(item=>item.thread.pinned),preferences.pinnedSort,preferences.pinnedOrder);
  if(pinned.length)codexSidebarRenderGroup({key:'pinned',label:codexSidebarText('Pinned','置顶'),items:pinned},list,true);
  const recent=codexSidebarSorted(items.filter(item=>!item.thread.pinned),preferences.sort,preferences.order);
  for(const group of codexSidebarSectionViews(recent))codexSidebarRenderGroup(group,list);
  const unassigned=recent.filter(item=>!codexSidebarSectionForItem(item));
  for(const group of codexSidebarGroups(unassigned,preferences.grouping)){
    if(group.kind==='project'&&!group.items.length&&preferences.sections.some(section=>section.id===preferences.projectSections[group.key]))continue;
    codexSidebarRenderGroup(group,list);
  }
  if(!items.length&&!list.children.length)list.append(codexSidebarElement('div','empty-note',codexSidebarText('Your chats will appear here.','你的聊天将显示在此处。')));
  if($('thread-count'))$('thread-count').textContent=items.length||'';
  desktop.threads=items.map(item=>item.thread);
  if(typeof codexSyncChrome==='function')codexSyncChrome();
}
function codexSidebarRemember(host,threads){
  codexSidebarUi.cache.set(host,{threads:(threads||[]).filter(thread=>thread.mode==='codex'),error:null});
}
async function codexSidebarAggregate(request,activeHost){
  const targets=[];
  if(activeHost!=='local'&&typeof remoteLocalRpc==='function')targets.push({host:'local',read:()=>remoteLocalRpc('thread/list',{mode:'codex',limit:200},2200)});
  if(typeof remoteUi!=='undefined'&&typeof remoteCall==='function'){
    for(const [host,connection] of remoteUi.connections){
      if(host!==activeHost&&connection.state==='connected')targets.push({host,read:()=>remoteCall(connection,'thread/list',{mode:'codex',limit:200},2200)});
    }
  }
  const results=await Promise.allSettled(targets.map(async target=>{
    const result=await target.read();
    if(target.host==='local'&&typeof remoteBind==='function')remoteBind(result,null);
    return result.data||[];
  }));
  if(request!==codexSidebarUi.request)return;
  results.forEach((result,index)=>{
    const host=targets[index].host;
    if(result.status==='fulfilled')codexSidebarRemember(host,result.value);
    else codexSidebarUi.cache.set(host,{threads:codexSidebarUi.cache.get(host)?.threads||[],error:result.reason?.message||'Could not refresh'});
  });
  if(desktop.mode==='codex')renderCodexSidebar();
}
const codexSidebarBaseRefresh=refreshThreads;
refreshThreads=async function(...args){
  const request=++codexSidebarUi.request,mode=desktop.mode,host=codexSidebarHost();
  let result;
  try{result=await codexSidebarBaseRefresh(...args);}catch(error){
    if(request===codexSidebarUi.request&&mode==='codex'&&desktop.mode==='codex'&&host===codexSidebarHost()){
      codexSidebarUi.cache.set(host,{threads:codexSidebarUi.cache.get(host)?.threads||[],error:error.message||'Could not refresh'});
      renderCodexSidebar();codexSidebarAggregate(request,host).catch(showError);
    }
    throw error;
  }
  if(mode!=='codex'){if(desktop.mode!=='codex')renderCodexSidebar();return result;}
  if(request!==codexSidebarUi.request||desktop.mode!=='codex'||host!==codexSidebarHost())return result;
  codexSidebarRemember(host,desktop.threads);renderCodexSidebar();
  // Local rows render immediately; another connected host can fail independently.
  codexSidebarAggregate(request,host).catch(showError);return result;
};
if(typeof desktopReady!=='undefined')desktopReady.then(()=>{if(desktop.mode==='codex'){if(!codexSidebarUi.cache.has(codexSidebarHost()))codexSidebarRemember(codexSidebarHost(),desktop.threads);renderCodexSidebar();}}).catch(showError);
function codexSidebarMenuThread(event,thread){
  let node=event?.currentTarget||event?.target;
  while(node&&!node.dataset?.codexThreadKey)node=node.parentElement;
  const key=node?.dataset?.codexThreadKey;
  if(key){
    const item=codexSidebarCollect().find(item=>item.key===key&&item.thread.id===thread.id);
    if(!item)throw new Error(codexSidebarText('This chat is no longer available. Refresh the sidebar.','此聊天已不可用。请刷新侧边栏。'));
    return item;
  }
  const host=typeof remoteUi==='undefined'?'local':remoteUi.threads.get(thread.id)||'local';
  return codexSidebarCollect().find(item=>item.host===host&&item.thread.id===thread.id);
}
if(typeof threadMenu==='function'){
  const codexSidebarBaseThreadMenu=threadMenu;
  threadMenu=function(event,thread){
    if(desktop.mode!=='codex')return codexSidebarBaseThreadMenu(event,thread);
    let item;try{item=codexSidebarMenuThread(event,thread);if(item)codexSidebarRequireHost(item.host,item.thread.id);}catch(error){showError(error);return;}
    const result=codexSidebarBaseThreadMenu(event,thread),menu=$('floating-menu');
    if(item&&menu.classList.contains('open')){
      const anchor=event?.currentTarget||event?.target||$('codex-sidebar-options');
      menu.append(codexSidebarAction(codexSidebarText('Move to section','移到分区'),()=>codexSidebarMoveMenu(anchor,{kind:'thread',key:item.key})));codexSidebarMenuKeyboard(menu,anchor);
    }
    return result;
  };
}
// Floating menus share a host element. Clear this menu's semantics on reuse.
function codexSidebarResetMenu(){const menu=$('floating-menu');if(menu?.classList.contains('codex-sidebar-menu')){menu.classList.remove('codex-sidebar-menu');menu.removeAttribute('role');menu.onkeydown=null;}}
const codexSidebarBaseOpenMenu=openFloatingMenuAt;
openFloatingMenuAt=function(...args){codexSidebarResetMenu();return codexSidebarBaseOpenMenu(...args);};
const codexSidebarBaseCloseMenu=closeFloatingMenu;
closeFloatingMenu=function(...args){const menu=$('floating-menu'),anchor=menu?.classList.contains('codex-sidebar-menu')&&menu.contains?.(document.activeElement)?codexSidebarUi.menuAnchor:null;codexSidebarResetMenu();const result=codexSidebarBaseCloseMenu(...args);anchor?.focus();return result;};
if(typeof window!=='undefined'&&typeof window.__onServerMessage==='function'){
  const codexSidebarBaseMessage=window.__onServerMessage;
  window.__onServerMessage=function(message){const result=codexSidebarBaseMessage(message);if(message.method==='remote/status'&&desktop.mode==='codex')renderCodexSidebar();return result;};
}
