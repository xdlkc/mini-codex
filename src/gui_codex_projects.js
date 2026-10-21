// Codex projects bind a source folder to its execution host. Chat and Work retain
// their existing instruction-based projects. Load after desktop, remote and chrome.
const codexProjectsUi={request:0,dialog:0,intent:0,byHost:new Map()};
function codexProjectText(en,zh){return typeof codexText==='function'?codexText(en,zh):en;}
function codexProjectHost(project){
  if(project.hostId)return project.hostId;
  if(typeof remoteUi!=='undefined'&&remoteUi.projects.has(project.id))return remoteUi.projects.get(project.id)||'local';
  return 'local';
}
function codexProjectActiveHost(){return typeof remoteContext==='function'?remoteContext()||'local':'local';}
function codexProjectCapture(host=codexProjectActiveHost()){
  if(host!=='local'){
    const connection=remoteConnection(host);
    return {host,key:host+'@'+connection.id,navigation:desktop.navigation,intent:codexProjectsUi.intent,call:(method,params={})=>remoteCall(connection,method,params)};
  }
  const call=typeof remoteLocalRpc==='function'?remoteLocalRpc:rpcLocalCall;
  return {host,key:'local',navigation:desktop.navigation,intent:codexProjectsUi.intent,call:async(method,params={})=>{
    const result=await call(method,params);if(typeof remoteBind==='function')remoteBind(result,null);return result;
  }};
}
function codexProjectCurrent(context){
  try{return desktop.mode==='codex'&&context.navigation===desktop.navigation&&context.intent===codexProjectsUi.intent&&context.key===codexProjectCapture().key;}catch{return false;}
}
function codexProjectRemember(project,host){
  if(typeof remoteMark==='function')remoteMark(remoteUi.projects,project.id,host==='local'?null:host);
  const saved=codexProjectsUi.byHost.get(host)||[];
  codexProjectsUi.byHost.set(host,[project,...saved.filter(item=>item.id!==project.id)]);
  if(desktop.state){desktop.state.projects=[project,...(desktop.state.projects||[]).filter(item=>item.id!==project.id)];}
}
async function codexProjectValidate(context,path){
  if(typeof path!=='string'||!path.startsWith('/'))throw new Error(codexProjectText('Choose an absolute source folder path.','请选择源文件夹的绝对路径。'));
  const result=await context.call('workspace/gitStatus',{cwd:path});
  if(!result.cwd)throw new Error(codexProjectText('Could not verify this folder on the selected host.','无法在所选主机上验证此文件夹。'));
  return result.cwd;
}
function codexProjectButton(label,run,className='btn-pill'){
  const button=document.createElement('button');button.type='button';button.className=className;button.textContent=label;
  button.onclick=()=>Promise.resolve().then(run).catch(showError);return button;
}
function codexProjectField(box,labelText,id,value='',placeholder=''){
  const label=document.createElement('label');label.htmlFor=id;label.textContent=labelText;
  const input=document.createElement('input');input.id=id;input.className='modal-input';input.value=value;input.placeholder=placeholder;input.autocomplete='off';
  box.append(label,input);return input;
}
function codexProjectDialog(project=null,initialPath='',onSaved=null){
  closeFloatingMenu();
  const origin=codexProjectCapture(),dialog=++codexProjectsUi.dialog;
  let target=codexProjectCapture(project?codexProjectHost(project):origin.host),revision=0,busy=false;
  openCustomModal(project?codexProjectText('Choose source folder','选择源文件夹'):codexProjectText('Create project','创建项目'),'<form id="codex-project-form" class="workspace-form"></form>');
  const form=$('codex-project-form');
  const valid=()=>dialog===codexProjectsUi.dialog&&$('codex-project-form')===form&&codexProjectCurrent(origin);
  const name=codexProjectField(form,codexProjectText('Project name','项目名称'),'codex-project-name',project?.name||'',codexProjectText('Name your project','为项目命名'));
  const hostLabel=document.createElement('label');hostLabel.htmlFor='codex-project-host';hostLabel.textContent=codexProjectText('Device','设备');
  const hosts=document.createElement('select');hosts.id='codex-project-host';hosts.className='modal-input';
  const choices=[{id:'local',name:codexProjectText('This computer','此计算机')}];
  if(typeof remoteUi!=='undefined')for(const profile of remoteUi.profiles){if(remoteUi.connections.get(profile.id)?.state==='connected')choices.push(profile);}
  for(const host of choices){const option=document.createElement('option');option.value=host.id;option.textContent=host.name;hosts.append(option);}
  hosts.value=target.host;hosts.disabled=Boolean(project);form.append(hostLabel,hosts);
  const path=codexProjectField(form,codexProjectText('Source folders','源文件夹'),'codex-project-path',initialPath||project?.path||'','/path/to/project');
  const choose=codexProjectButton(codexProjectText('Choose folder…','选择文件夹…'),async()=>{
    if(!valid()||target.host!=='local'||busy)return;
    const context=target,ownedRevision=revision,result=await context.call('projects/pickFolder',{prompt:codexProjectText('Choose a source folder','选择源文件夹')});
    if(valid()&&!busy&&context===target&&ownedRevision===revision&&result.path){revision++;path.value=result.path;update();}
  });
  const error=document.createElement('p');error.id='codex-project-error';error.className='workspace-error';error.setAttribute('role','alert');error.hidden=true;path.setAttribute('aria-describedby',error.id);
  const actions=document.createElement('div');actions.className='workspace-actions';
  const cancel=codexProjectButton(codexProjectText('Cancel','取消'),()=>closeModal());
  const submit=document.createElement('button');submit.type='submit';submit.className='btn-pill primary';submit.textContent=project?codexProjectText('Use folder','使用文件夹'):codexProjectText('Create project','创建项目');
  actions.append(cancel,submit);form.append(choose,error,actions);
  function update(){choose.hidden=target.host!=='local';submit.disabled=busy||!path.value.trim().startsWith('/');}
  path.oninput=()=>{revision++;error.hidden=true;path.removeAttribute('aria-invalid');update();};
  hosts.onchange=()=>{
    if(!valid()||busy)return;
    try{target=codexProjectCapture(hosts.value);revision++;path.value='';error.hidden=true;update();path.focus();}catch(failure){hosts.value=target.host;showError(failure);}
  };
  form.onsubmit=async event=>{
    event.preventDefault();if(busy||!valid())return;
    const context=target,ownedRevision=revision,selectedPath=path.value.trim(),selectedName=name.value.trim()||baseName(selectedPath);
    busy=true;hosts.disabled=true;name.disabled=true;path.disabled=true;choose.disabled=true;cancel.disabled=true;update();error.hidden=true;
    try{
      // Validation and persistence both run on the captured host. The backend
      // validates again when it saves and never creates a missing source folder.
      const canonical=await codexProjectValidate(context,selectedPath);
      if(!valid()||context!==target||ownedRevision!==revision)return;
      const result=await context.call(project?'desktop/project/update':'desktop/project/create',{
        ...(project?{id:project.id}:{}),name:selectedName,mode:'codex',path:canonical,hostId:context.host,
      });
      if(!valid()||context!==target||ownedRevision!==revision)return;
      codexProjectRemember(result.project,context.host);closeModal();if(onSaved)await onSaved(result.project);else await openProject(result.project);
    }catch(failure){
      if(valid()){error.textContent=failure.message||String(failure);error.hidden=false;path.setAttribute('aria-invalid','true');}
    }finally{busy=false;if(valid()){hosts.disabled=Boolean(project);name.disabled=false;path.disabled=false;choose.disabled=false;cancel.disabled=false;update();}}
  };
  update();name.focus();return form;
}
const codexProjectsBaseCreate=createDesktopProject;
createDesktopProject=function(...args){return desktop.mode==='codex'?codexProjectDialog():codexProjectsBaseCreate(...args);};
const codexProjectsBaseFolderDialog=openCreateProjectDialog;
openCreateProjectDialog=function(...args){return desktop.mode==='codex'?codexProjectDialog():codexProjectsBaseFolderDialog(...args);};
const codexProjectsBaseOpen=openProject;
openProject=async function(project){
  if(project.mode!=='codex')return codexProjectsBaseOpen(project);
  const request=++codexProjectsUi.request;let navigation=++desktop.navigation;
  saveDraft();await flushDocument();if(request!==codexProjectsUi.request||navigation!==desktop.navigation)return;
  const host=codexProjectHost(project);
  if(desktop.mode!=='codex'){
    await switchMode('codex');if(request!==codexProjectsUi.request||desktop.mode!=='codex')return;
  }
  if(host!==codexProjectActiveHost()){
    const switching=remoteSelect(host==='local'?null:host),intent=codexProjectsUi.intent;await switching;
    if(intent!==codexProjectsUi.intent||request!==codexProjectsUi.request||codexProjectActiveHost()!==host)return;
  }
  const context=codexProjectCapture();
  if(!project.path){codexProjectDialog(project);return;}
  const canonical=await codexProjectValidate(context,project.path);
  if(request!==codexProjectsUi.request||!codexProjectCurrent(context))return;
  const starting=newThread(false,true),intent=codexProjectsUi.intent;await starting;
  if(intent!==codexProjectsUi.intent||request!==codexProjectsUi.request||codexProjectActiveHost()!==host)return;
  desktop.projectId=project.id;updateCwdUI(canonical);$('topbar-title').textContent=project.name;
  if(typeof remoteRemember==='function')remoteRemember();
  if(typeof codexRecordView==='function')codexRecordView();
  $('composer-input').focus();await refreshThreads();
};
const codexProjectsBaseRefresh=refreshProjects;
refreshProjects=async function(...args){
  if(desktop.mode!=='codex')return codexProjectsBaseRefresh(...args);
  const context=codexProjectCapture();
  const [bound,legacy]=await Promise.all([context.call('desktop/project/list',{mode:'codex'}),context.call('projects/list')]);
  if(!codexProjectCurrent(context))return;
  const projects=bound.items||[];codexProjectsUi.byHost.set(context.host,projects);
  const paths=new Set(projects.map(project=>project.path).filter(Boolean));
  projectsList=[...projects.filter(project=>project.path),...(legacy.projects||[]).filter(project=>!paths.has(project.path))]
    .map(project=>({...project,label:project.label||project.name||baseName(project.path)}));
};
const codexProjectsBaseSelect=selectProject;
selectProject=async function(path){
  if(desktop.mode!=='codex')return codexProjectsBaseSelect(path);
  closeFloatingMenu();if(!path)return;
  const context=codexProjectCapture(),request=++codexProjectsUi.request;
  const canonical=await codexProjectValidate(context,path);
  if(!codexProjectCurrent(context)||request!==codexProjectsUi.request)return;
  const result=await context.call('desktop/project/list',{mode:'codex'});
  if(!codexProjectCurrent(context)||request!==codexProjectsUi.request)return;
  let project=(result.items||[]).find(item=>item.path===canonical);
  if(!project){
    const created=await context.call('desktop/project/create',{name:baseName(canonical)||canonical,mode:'codex',path:canonical,hostId:context.host});
    if(!codexProjectCurrent(context)||request!==codexProjectsUi.request)return;project=created.project;
  }
  codexProjectRemember(project,context.host);await openProject(project);
};
function codexProjectMenu(project,anchor,options={}){
  if(!options.append){closeFloatingMenu();openFloatingMenuAt('codex-project-'+project.id,anchor,'',false);}
  const menu=$('floating-menu');
  menu.append(menuButton(codexProjectText('Rename','重命名'),()=>codexRenameProject(project)));
  menu.append(menuButton(codexProjectText('Remove project','移除项目'),()=>codexRemoveProject(project)));
  return menu;
}
function codexRenameProject(project){
  const context=codexProjectCapture(codexProjectHost(project)),origin=codexProjectCapture();
  openCustomModal(codexProjectText('Rename project','重命名项目'),'<form id="codex-project-rename" class="workspace-form"></form>');const form=$('codex-project-rename');
  const name=codexProjectField(form,codexProjectText('Project name','项目名称'),'codex-project-rename-name',project.name);
  const submit=document.createElement('button');submit.type='submit';submit.className='btn-pill primary';submit.textContent=codexProjectText('Save','保存');form.append(submit);
  form.onsubmit=async event=>{event.preventDefault();if(!name.value.trim()||submit.disabled||!codexProjectCurrent(origin))return;submit.disabled=true;
    try{const result=await context.call('desktop/project/update',{id:project.id,name:name.value.trim()});if($('codex-project-rename')!==form||!codexProjectCurrent(origin))return;codexProjectRemember(result.project,context.host);closeModal();await refreshThreads();}
    catch(error){showError(error);}finally{submit.disabled=false;}
  };name.focus();name.select();
}
async function codexRemoveProject(project){
  const context=codexProjectCapture(codexProjectHost(project)),origin=codexProjectCapture();
  await context.call('desktop/project/delete',{id:project.id});
  if(!codexProjectCurrent(origin))return;
  codexProjectsUi.byHost.set(context.host,(codexProjectsUi.byHost.get(context.host)||[]).filter(item=>item.id!==project.id));
  if(desktop.state)desktop.state.projects=(desktop.state.projects||[]).filter(item=>item.id!==project.id);
  if(desktop.projectId===project.id)desktop.projectId=null;
  await refreshThreads();
}
const codexProjectsBaseMove=moveThreadDialog;
moveThreadDialog=async function(thread){
  if(thread.mode!=='codex')return codexProjectsBaseMove(thread);
  const host=typeof codexThreadContext==='function'?codexThreadContext(thread.id).host:
    typeof remoteUi!=='undefined'?remoteUi.threads.get(thread.id)||'local':'local';
  const context=codexProjectCapture(host),origin=codexProjectCapture(),request=++codexProjectsUi.dialog;
  const result=await context.call('desktop/project/list',{mode:'codex'});
  if(!codexProjectCurrent(origin)||request!==codexProjectsUi.dialog)return;
  openCustomModal(codexProjectText('Move to project','移至项目'),'<div id="codex-move-project-options"></div>');const box=$('codex-move-project-options');
  for(const project of [{id:null,name:codexProjectText('No project','无项目')},...(result.items||[])]){
    box.append(menuButton(project.name,async()=>{
      if(!codexProjectCurrent(origin)||$('codex-move-project-options')!==box)return;
      await context.call('desktop/threadMetadata/set',{threadId:thread.id,projectId:project.id});
      if(!codexProjectCurrent(origin)||$('codex-move-project-options')!==box)return;
      if(currentThread===thread.id)desktop.projectId=project.id;closeModal();await refreshThreads();
    }));
  }
};
// Navigation layers may increment desktop.navigation after awaiting a document
// save. Track the caller's intent immediately so our own navigation remains
// distinguishable from a later click, regardless of those internal increments.
for(const name of ['newThread','openThread','switchMode','remoteSelect']){
  const base=window[name];if(typeof base!=='function')continue;
  window[name]=function(...args){codexProjectsUi.intent++;return base.apply(this,args);};
}
