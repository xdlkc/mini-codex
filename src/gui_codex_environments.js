// Local environment files, explicit setup and project toolbar actions.
const codexEnvironmentUi={viewRevision:0,chromeRevision:0,entries:[],selection:null,loading:false,error:null,platform:'darwin',runs:new Map(),dismissed:new Set(),timer:null,archiveObservers:new Set(),outputPending:new Set(),actionFieldId:0};
const codexEnvText=(en,zh)=>typeof codexText==='function'?codexText(en,zh):en;
const codexEnvHost=()=>typeof remoteContext==='function'?remoteContext()||'local':'local';
const codexEnvPath=cwd=>String(cwd||'').replace(/\/+$/,'')||'/';
const codexEnvKey=(host,cwd)=>JSON.stringify([host,codexEnvPath(cwd)]);
const codexEnvDisplayName=entry=>entry.environment?.name||baseName(entry.configPath);
function codexEnvResolveSelection(entries,selected){
  if(selected==null)return null;
  const entry=entries.find(entry=>codexEnvPath(entry.configPath)===codexEnvPath(selected));
  if(entry)return entry.type==='success'?entry.configPath:null;
  const valid=entries.filter(entry=>entry.type==='success');
  return (valid.find(entry=>baseName(entry.configPath)==='environment.toml')||valid[0])?.configPath||null;
}
function codexEnvActionIcon(value){return typeof codexIcon==='function'?codexIcon(({tool:'environmentTool',run:'environmentRun',debug:'environmentDebug',test:'environmentTest'})[value]||'environmentTool'):'';}
function codexEnvActionIconButton(value){
  const button=document.createElement('button');button.type='button';button.className='btn-pill codex-env-action-icon';button.value=value?value.icon||'':'tool';button.setAttribute('aria-haspopup','menu');
  const labels={tool:['Tool','工具'],run:['Run','运行'],debug:['Debug','调试'],test:['Test','测试']};
  const paint=()=>{const choice=button.value||'tool';button.innerHTML=codexEnvActionIcon(choice);button.setAttribute('aria-label',codexEnvText(...labels[choice]));};paint();
  button.onclick=event=>{event?.stopPropagation();openFloatingMenuAt('codex-environment-icon',button,'',false);const menu=$('floating-menu');for(const [key,label] of Object.entries(labels)){const item=menuButton(codexEnvText(...label),()=>{button.value=key;paint();button.dispatchEvent(new Event('change',{bubbles:true}));closeFloatingMenu();button.focus();});item.insertAdjacentHTML('afterbegin',codexEnvActionIcon(key));item.setAttribute('role','menuitemradio');item.setAttribute('aria-checked',String(key===(button.value||'tool')));menu.append(item);}codexMenuKeyboard(menu,button);menu.querySelector('[aria-checked=true]')?.focus();};return button;
}
function codexEnvironmentContext(){return {...workspaceCapture(),host:codexEnvHost()};}
function codexEnvironmentRun(){return codexEnvironmentUi.runs.get(codexEnvKey(codexEnvHost(),currentCwd));}
function codexEnvironmentBusy(){const run=codexEnvironmentRun();return desktop.mode==='codex'&&run?.status==='running'&&run.scriptType!=='action';}
function codexEnvButton(label,run,primary=false){return workspaceButton(label,run,'btn-pill'+(primary?' primary':''));}
function codexEnvironmentRemember(run,host=codexEnvHost()){
  if(!run?.id||!run.workspaceRoot)return;
  const previous=codexEnvironmentUi.runs.get(codexEnvKey(host,run.workspaceRoot));
  if(previous&&((previous.startedAt||0)>(run.startedAt||0)||(previous.id===run.id&&((previous.status!=='running'&&run.status==='running')||((previous.outputSequence||0)>(run.outputSequence||0)&&run.status==='running')))))return;
  codexEnvironmentUi.runs.set(codexEnvKey(host,run.workspaceRoot),run);
  for(const observer of codexEnvironmentUi.archiveObservers){if(!observer.box.isConnected){codexEnvironmentUi.archiveObservers.delete(observer);continue;}if(observer.host===host&&codexEnvPath(observer.root)===codexEnvPath(run.workspaceRoot))observer.paint(run);}
  if(host===codexEnvHost()&&codexEnvPath(run.workspaceRoot)===codexEnvPath(currentCwd))codexEnvironmentPaint();
}
function codexEnvironmentPaint(){
  if(typeof codexPermissionsInert==='function')codexPermissionsInert();
  const panel=$('codex-environment-status');if(!panel)return;
  const outputOpen=panel.querySelector('details')?.open||false;panel.replaceChildren();const run=codexEnvironmentRun();if(run?.scriptType==='action'){panel.hidden=true;codexEnvironmentTerminal(run);return;}
  panel.hidden=desktop.mode!=='codex'||!run||codexEnvironmentUi.dismissed.has(run.id);
  if(panel.hidden)return;
  const context=codexEnvironmentContext(),title=document.createElement('strong');
  title.textContent=run.status==='running'?(run.scriptType==='cleanup'?codexEnvText('Running cleanup script…','正在运行清理脚本…'):codexEnvText('Setting up local environment…','正在设置本地环境…')):run.status==='succeeded'?codexEnvText('Local environment is ready','本地环境已就绪'):codexEnvText('Environment needs attention','环境需要处理');
  const header=document.createElement('div');header.className='codex-env-run-heading';header.append(title);
  const actions=document.createElement('div');actions.className='workspace-actions';
  if(run.status==='running')actions.append(codexEnvButton(codexEnvText('Stop','停止'),async()=>{await context.call('workspace/environment/cancel',{id:run.id});}));
  else {
    if(run.status!=='succeeded')actions.append(codexEnvButton(codexEnvText('Retry','重试'),()=>codexEnvironmentStart({...run,cwd:run.workspaceRoot},context)));
    actions.append(codexEnvButton(codexEnvText('Dismiss','关闭'),async()=>{codexEnvironmentUi.dismissed.add(run.id);codexEnvironmentPaint();}));
  }
  header.append(actions);panel.append(header);
  if(run.error){const error=document.createElement('p');error.className='workspace-error';error.textContent=run.error;panel.append(error);}
  const details=document.createElement('details');details.open=outputOpen;const summary=document.createElement('summary');summary.textContent=run.scriptType==='cleanup'?codexEnvText('Cleanup output','清理输出'):codexEnvText('Setup output','设置输出');const output=document.createElement('pre');output.textContent=run.log||codexEnvText('No output','暂无输出');details.append(summary,output);panel.append(details);
  if(run.status==='running'&&!codexEnvironmentUi.timer){
    codexEnvironmentUi.timer=setTimeout(async()=>{codexEnvironmentUi.timer=null;try{const result=await context.call('workspace/environment/status',{id:run.id});codexEnvironmentRemember(result.run,context.host||context.key.split('@')[0]);}catch(error){if(workspaceCurrent(context))showError(error);}},700);
  }
}
async function codexEnvironmentStart(request,context=codexEnvironmentContext()){
  workspaceRequire(context);
  const result=await context.call('workspace/environment/run',{cwd:request.cwd||currentCwd,configPath:request.configPath,configSourceCwd:request.configSourceCwd,sourceTreePath:request.sourceTreePath,scriptType:request.scriptType||'setup',actionIndex:request.actionIndex,threadId:request.threadId});
  codexEnvironmentRemember(result.run,context.host||context.key.split('@')[0]);if(request.scriptType==='action'&&workspaceCurrent(context))codexEnvironmentTerminal(result.run,true);return result.run;
}
async function codexEnvironmentRefresh(){
  const environmentNav=document.querySelector('[data-sec="environments"]');if(environmentNav){environmentNav._environmentOriginalLabel??=environmentNav.textContent;environmentNav.textContent=desktop.mode==='codex'?codexEnvText('Local environments','本地环境'):environmentNav._environmentOriginalLabel;}
  const ticket=++codexEnvironmentUi.chromeRevision;
  if(desktop.mode!=='codex'){codexEnvironmentPaint();return;}
  const cwd=currentCwd,context=codexEnvironmentContext();codexEnvironmentUi.loading=true;codexEnvironmentUi.entries=[];codexEnvironmentUi.selection=null;codexEnvironmentUi.error=null;codexEnvironmentToolbar();
  try{
    const [result,selected]=await Promise.all([context.call('workspace/environment/list',{cwd}),context.call('workspace/environment/selection/read',{cwd}).catch(()=>({configPath:null}))]);
    if(ticket!==codexEnvironmentUi.chromeRevision||!workspaceSameHost(context)||cwd!==currentCwd)return;
    codexEnvironmentUi.entries=result.environments||[];codexEnvironmentUi.platform=result.platform||'darwin';codexEnvironmentUi.selection=codexEnvResolveSelection(codexEnvironmentUi.entries,selected.configPath);codexEnvironmentRemember(result.latestRun);
  }catch(error){if(ticket===codexEnvironmentUi.chromeRevision&&workspaceSameHost(context)&&cwd===currentCwd)codexEnvironmentUi.error=error.message;}
  finally{if(ticket===codexEnvironmentUi.chromeRevision){codexEnvironmentUi.loading=false;codexEnvironmentToolbar();codexEnvironmentPaint();}}
}
function codexEnvironmentTerminal(run,open=false){
  if(desktop.mode!=='codex'||codexEnvPath(run.workspaceRoot)!==codexEnvPath(currentCwd)||(run.threadId&&run.threadId!==currentThread)||typeof toggleDrawer!=='function')return;
  if(open&&(!drawerOpen||currentDrawerTab!=='terminal'))toggleDrawer('terminal');
  if(!drawerOpen||currentDrawerTab!=='terminal')return;
  const box=$('drawer-content');if(!box)return;
  let section=$('codex-environment-terminal');
  if(!section){section=document.createElement('section');section.id='codex-environment-terminal';box.prepend(section);}
  section.replaceChildren();const title=document.createElement('strong');title.textContent=run.actionName||run.environmentName||codexEnvText('Action','操作');const status=document.createElement('span');status.textContent=run.status;const output=document.createElement('pre');output.className='code-block-pre';output.textContent=run.log||'';section.append(title,status,output);
  if(run.status==='running'){const context=codexEnvironmentContext();section.append(codexEnvButton(codexEnvText('Stop','停止'),()=>context.call('workspace/environment/cancel',{id:run.id})));}
}
function codexEnvironmentArchiveControls(box,worktree,context){
  const node=document.createElement('section');node.className='codex-env-archive-output';box.append(node);
  const observer={box,root:worktree.path,host:context.key.split('@')[0],paint:run=>{
    node.replaceChildren();const label=document.createElement('p');label.textContent=run.status==='running'?codexEnvText('Running cleanup script…','正在运行清理脚本…'):run.error||codexEnvText('Cleanup finished','清理已结束');const output=document.createElement('pre');output.textContent=run.log||'';node.append(label,output);
    if(run.status==='running')node.append(codexEnvButton(codexEnvText('Stop cleanup','停止清理'),()=>context.call('workspace/environment/cancel',{id:run.id})));
  }};codexEnvironmentUi.archiveObservers.add(observer);
}
function codexEnvironmentToolbar(){
  const toolbar=$('codex-environment-actions');if(!toolbar)return;toolbar.replaceChildren();
  if(desktop.mode!=='codex'||codexEnvironmentUi.loading||!currentThread||document.body.classList.contains('in-settings'))return;
  const environment=codexEnvironmentUi.entries.find(e=>e.type==='success'&&e.configPath===codexEnvironmentUi.selection);
  for(const [index,action] of (environment?.environment.actions||[]).entries()){
    if(action.platform&&action.platform!==codexEnvironmentUi.platform)continue;
    const context=codexEnvironmentContext(),cwd=currentCwd;const button=codexEnvButton(action.name,()=>codexEnvironmentStart({cwd,configPath:environment.configPath,scriptType:'action',actionIndex:index,threadId:currentThread},context));button.className='pill-badge';button.insertAdjacentHTML('afterbegin',codexEnvActionIcon(action.icon));button.title=action.command;toolbar.append(button);
  }
}
async function codexEnvironmentMenu(anchor){
  const context=codexEnvironmentContext(),cwd=currentCwd;
  const result=await context.call('workspace/environment/list',{cwd});workspaceRequire(context);if(cwd!==currentCwd)return;
  const selected=await context.call('workspace/environment/selection/read',{cwd}).catch(()=>({configPath:null}));workspaceRequire(context);
  const resolved=codexEnvResolveSelection(result.environments||[],selected.configPath);
  openFloatingMenuAt('codex-environment',anchor,'',false);const menu=$('floating-menu');
  const heading=document.createElement('div');heading.className='menu-section-title';heading.textContent=codexEnvText('Environment','环境');menu.append(heading);
  for(const entry of [{configPath:null,environment:{name:codexEnvText('No environment','不使用环境')},type:'success'},...(result.environments||[])]){
    const label=codexEnvDisplayName(entry);
    const button=menuButton(label,async()=>{await context.call('workspace/environment/selection/set',{cwd,configPath:entry.configPath});if(workspaceCurrent(context)){closeFloatingMenu();await codexEnvironmentRefresh();}});
    button.disabled=entry.type!=='success';button.setAttribute('role','menuitemradio');button.setAttribute('aria-checked',String(entry.configPath===resolved));menu.append(button);
  }
  menu.append(menuButton(codexEnvText('Edit local environments','编辑本地环境'),()=>openSettingsView('environments')));
  codexMenuKeyboard(menu,anchor);
}
function codexEnvField(parent,label,value='',multiline=false){
  const row=document.createElement('label');row.className='codex-env-field';const title=document.createElement('span');title.textContent=label;
  const input=document.createElement(multiline?'textarea':'input');input.className='modal-input';input.value=value;if(multiline){input.rows=7;input.spellcheck=false;}row.append(title,input);parent.append(row);return input;
}
function codexEnvScripts(parent,title,initial={script:''}){
  const box=document.createElement('section');box.className='codex-env-script';const heading=document.createElement('h3');heading.textContent=title;
  const select=document.createElement('select');select.className='settings-select';select.setAttribute('aria-label',title+' '+codexEnvText('platform','平台'));
  for(const [key,en,zh] of [['script','All platforms','所有平台'],['darwin','macOS','macOS'],['linux','Linux','Linux'],['win32','Windows','Windows']]){const option=document.createElement('option');option.value=key;option.textContent=codexEnvText(en,zh);select.append(option);}
  const drafts={script:initial.script||'',darwin:initial.darwin?.script||'',linux:initial.linux?.script||'',win32:initial.win32?.script||''};let current='script';
  const isSetup=title===codexEnvText('Setup script','设置脚本'),placeholder=platform=>isSetup?(platform==='win32'?'python -m pip install -r requirements.txt\npnpm install':'cd \"$CODEX_WORKTREE_PATH\"\npip install -r requirements.txt\nnpm install\n./run/setup.sh'):(platform==='win32'?'docker compose down --remove-orphans':'docker compose down --remove-orphans\nrm -rf .cache/tmp');
  const input=document.createElement('textarea');input.className='modal-input';input.rows=7;input.spellcheck=false;input.value=drafts.script;input.setAttribute('aria-label',title);input.placeholder=placeholder(current);
  select.onchange=()=>{drafts[current]=input.value;current=select.value;input.value=drafts[current];input.placeholder=placeholder(current);};
  box.append(heading,select,input);parent.append(box);
  return ()=>{drafts[current]=input.value;const value={script:drafts.script};for(const key of ['darwin','linux','win32'])if(drafts[key].length)value[key]={script:drafts[key]};return value;};
}
function codexEnvToml(value){
  const quote=value=>{const text=String(value).replace(/\r\n?/g,'\n');if(!text.includes('\n'))return JSON.stringify(text);return text.includes("'''")?'"""\n'+text.replace(/\\/g,'\\\\').replace(/"""/g,'\\"""')+'"""':"'''\n"+text+"'''";};
  const lines=['# THIS IS AUTOGENERATED. DO NOT EDIT MANUALLY','version = '+(value.version||1),'name = '+quote(value.name.trim())];
  for(const name of ['setup','cleanup']){const script=value[name];if(!script)continue;const platforms=['darwin','linux','win32'].filter(platform=>script[platform]?.script?.length);if(name!=='setup'&&!script.script.length&&!platforms.length)continue;lines.push('','['+name+']','script = '+quote(script.script));if(platforms.length)lines.push('');for(const [index,platform] of platforms.entries()){lines.push('['+name+'.'+platform+']','script = '+quote(script[platform].script));if(index<platforms.length-1)lines.push('');}}
  const actions=(value.actions||[]).filter(action=>action.name.trim()&&action.command.trim());if(actions.length)lines.push('');for(const action of actions){lines.push('[[actions]]','name = '+quote(action.name.trim()));if(action.icon)lines.push('icon = '+quote(action.icon));lines.push('command = '+quote(action.command.trim()));if(action.platform)lines.push('platform = '+quote(action.platform));lines.push('');}return lines.join('\n').trimEnd()+'\n';
}
async function codexEnvironmentEditor(configPath,context=codexEnvironmentContext(),configSourceCwd=null){
  const cwd=context.environmentCwd||currentCwd,box=$('settings-body'),ticket=++codexEnvironmentUi.viewRevision;box.innerHTML='<p class="settings-sub"></p>';box.firstChild.textContent=codexEnvText('Loading environment…','正在加载环境…');
  const valid=()=>workspaceCurrent(context)&&ticket===codexEnvironmentUi.viewRevision&&currentSettingsSection==='environments';
  let loaded;try{loaded=await context.call('workspace/environment/read',{cwd,configPath,configSourceCwd});}catch(failure){
    if(!valid())return;box.replaceChildren();const heading=document.createElement('h1');heading.className='settings-h1';heading.textContent=codexEnvText('Could not read the environment','无法读取环境');const message=document.createElement('p');message.className='workspace-error';message.textContent=failure.message||String(failure);box.append(codexEnvButton(codexEnvText('Back','返回'),()=>renderSettingsSection('environments')),heading,message,codexEnvButton(codexEnvText('Retry','重试'),()=>codexEnvironmentEditor(configPath,context,configSourceCwd)));return;
  }
  if(!valid())return;
  const existing=loaded.environment?.type==='success'?loaded.environment.environment:null;let expectedRevision=loaded.revision,activateNew=!loaded.exists,conflict=false;
  box.replaceChildren();const heading=document.createElement('h1');heading.className='settings-h1';heading.textContent=codexEnvText('Edit local environment','编辑本地环境');box.append(codexEnvButton(codexEnvText('Back','返回'),()=>renderSettingsSection('environments')),heading);
  const form=document.createElement('div');form.className='codex-env-editor';box.append(form);
  if(loaded.environment?.type==='error'){const warning=document.createElement('p');warning.className='workspace-error';warning.textContent=codexEnvText('This environment file is invalid. Saving will replace its contents','此环境文件无效，保存将替换其内容');form.append(warning);}
  const name=codexEnvField(form,codexEnvText('Name','名称'),existing?existing.name:baseName(cwd)||'local');
  const setup=codexEnvScripts(form,codexEnvText('Setup script','设置脚本'),existing?.setup);
  const note=document.createElement('p');note.className='settings-sub';note.textContent=codexEnvText('Runs at the project root on worktree creation','在创建工作树时于项目根目录运行');form.append(note);
  const variables=document.createElement('details');variables.innerHTML='<summary></summary><code>CODEX_SOURCE_TREE_PATH</code><p></p><code>CODEX_WORKTREE_PATH</code><p></p>';variables.querySelector('summary').textContent=codexEnvText('Variables','变量');const descriptions=variables.querySelectorAll('p');descriptions[0].textContent=codexEnvText('Source workspace path','源工作区路径');descriptions[1].textContent=codexEnvText('New worktree path','新工作树路径');form.append(variables);
  const actionsHeading=document.createElement('h3');actionsHeading.textContent=codexEnvText('Actions','操作');const description=document.createElement('p');description.className='settings-sub';description.textContent=codexEnvText('These actions can run any command and will be displayed in the header','这些操作可运行命令，并会显示在顶部工具栏');const actions=document.createElement('div');actions.className='codex-env-actions-editor';form.append(actionsHeading,description,actions);
  const actionRows=[];
  const addAction=value=>{
    const row=document.createElement('div');row.className='codex-env-action-row';
    const nameLabel=document.createElement('label');nameLabel.className='codex-env-action-label';nameLabel.textContent=codexEnvText('Name','名称');
    const label=document.createElement('input');label.className='modal-input';label.id='codex-env-action-'+(++codexEnvironmentUi.actionFieldId);label.value=value?.name||'';nameLabel.htmlFor=label.id;
    const icon=codexEnvActionIconButton(value),nameControl=document.createElement('div');nameControl.className='codex-env-action-name';nameControl.append(icon,label);row.append(nameLabel,nameControl);
    const command=codexEnvField(row,codexEnvText('Action script','操作脚本'),value?.command||'',true);command.placeholder='npm run dev';
    const platform=document.createElement('select');platform.className='settings-select';platform.setAttribute('aria-label',codexEnvText('Platforms','平台'));for(const [id,text] of [['','All platforms'],['darwin','macOS'],['linux','Linux'],['win32','Windows']]){const option=document.createElement('option');option.value=id;option.textContent=id?text:codexEnvText(text,'所有平台');platform.append(option);}platform.value=value?.platform||'';
    const notice=document.createElement('p');notice.className='workspace-error codex-env-action-error';notice.setAttribute('role','alert');notice.hidden=true;
    const entry={row,label,command,platform,icon,notice};actionRows.push(entry);row.append(platform,notice,codexEnvButton(codexEnvText('Delete','删除'),async()=>{row.remove();actionRows.splice(actionRows.indexOf(entry),1);}));actions.append(row);
  };
  for(const action of existing?.actions||[])addAction(action);form.append(codexEnvButton(codexEnvText('Add action','添加操作'),async()=>addAction()));
  const advanced=document.createElement('details');const advancedLabel=document.createElement('summary');advancedLabel.textContent=codexEnvText('Advanced options','高级选项');advanced.append(advancedLabel);form.append(advanced);const cleanup=codexEnvScripts(advanced,codexEnvText('Cleanup script','清理脚本'),existing?.cleanup);const path=document.createElement('code');path.textContent=configPath;advanced.append(path);
  const error=document.createElement('p');error.className='workspace-error';error.setAttribute('role','alert');const footer=document.createElement('div');footer.className='codex-env-editor-footer';form.append(error,footer);
  const save=async()=>{
    workspaceRequire(context);if(conflict)return;
    if(!name.value.trim())throw new Error(codexEnvText('Add an environment name to save','请输入环境名称后再保存'));
    const actionValues=actionRows.map(({label,command,platform,icon})=>({name:label.value.trim(),command:command.value.trim(),platform:platform.value||undefined,icon:icon.value}));
    if(actionValues.some(action=>Boolean(action.name)!==Boolean(action.command)))throw new Error(codexEnvText('Add both a name and command for each action','请为每个操作填写名称和命令'));
    const cleaned=cleanup(),value={version:existing?.version||1,name:name.value.trim(),setup:setup(),actions:actionValues.filter(action=>action.name&&action.command)};if(cleaned.script||Object.keys(cleaned).length>1)value.cleanup=cleaned;
    const raw=codexEnvToml(value);const result=await context.call('workspace/environment/save',{cwd,configPath,configSourceCwd,expectedRevision,raw});
    if(!valid())return;
    if(result.type==='conflict'){conflict=true;footer.querySelectorAll('button').forEach(button=>button.disabled=true);error.textContent=codexEnvText('This environment changed on disk. Discard your edits before saving again','此环境已在磁盘上发生变化，请放弃编辑后重新加载');footer.append(codexEnvButton(codexEnvText('Discard edits','放弃编辑'),()=>codexEnvironmentEditor(configPath,context,configSourceCwd)));return;}
    expectedRevision=result.revision;
    try{if(activateNew){activateNew=false;await context.call('workspace/environment/selection/set',{cwd,configPath});}}catch(failure){error.textContent=codexEnvText('Saved the environment file, but could not select it','环境文件已保存，但未能将其选中')+': '+failure.message;return;}
    toast(codexEnvText('Saved local environment','已保存本地环境'));await codexEnvironmentRefresh();
    await renderSettingsSection('environments');
  };
  const saveButton=document.createElement('button');saveButton.type='button';saveButton.className='btn-pill primary';saveButton.textContent=codexEnvText('Save','保存');footer.append(saveButton);
  const value=()=>({name:name.value.trim(),setup:setup(),cleanup:cleanup(),actions:actionRows.map(({label,command,platform,icon})=>({name:label.value.trim(),command:command.value.trim(),platform:platform.value,icon:icon.value}))});
  const initial=JSON.stringify(value());let saving=false;
  const update=()=>{
    const draft=value();name.setAttribute('aria-invalid',String(!draft.name));
    for(const entry of actionRows){const named=Boolean(entry.label.value.trim()),scripted=Boolean(entry.command.value.trim());entry.label.setAttribute('aria-invalid',String(scripted&&!named));entry.command.setAttribute('aria-invalid',String(named&&!scripted));entry.notice.hidden=named===scripted;entry.notice.textContent=named&&!scripted?codexEnvText('Add a command for this action','输入操作命令'):scripted&&!named?codexEnvText('Add a name for this action','输入操作名称'):'';}
    saveButton.disabled=saving||conflict||!draft.name||draft.actions.some(action=>Boolean(action.name)!==Boolean(action.command))||(loaded.exists&&loaded.environment?.type!=='error'&&JSON.stringify(draft)===initial);
  };
  saveButton.onclick=async()=>{if(saving||conflict||!valid())return;saving=true;update();try{await save();}catch(failure){if(valid())error.textContent=failure.message||String(failure);}finally{saving=false;if(valid())update();}};
  form.addEventListener('input',update);form.addEventListener('change',update);new MutationObserver(update).observe(actions,{childList:true});update();name.focus();
}
function codexEnvironmentNewPath(cwd,entries){
  const directory=codexEnvPath(cwd)+'/.codex/environments/',used=new Set(entries.map(entry=>codexEnvPath(entry.configPath)));
  if(!used.has(directory+'environment.toml'))return directory+'environment.toml';
  let index=2;while(used.has(directory+'environment-'+index+'.toml'))index++;return directory+'environment-'+index+'.toml';
}
function codexEnvironmentOwner(path){return codexEnvPath(path).replace(/\/\.codex\/environments\/[^/]+$/,'');}
function codexEnvironmentPreview(parent,title,scripts,platform=codexEnvironmentUi.platform){
  const section=document.createElement('section');section.className='settings-card codex-env-card';const heading=document.createElement('h2');heading.textContent=title;
  const select=document.createElement('select');select.className='settings-select';select.setAttribute('aria-label',title+' '+codexEnvText('platform','平台'));
  for(const [id,label] of [['default',codexEnvText('All platforms','所有平台')],['darwin','macOS'],['linux','Linux'],['win32','Windows']]){const option=document.createElement('option');option.value=id;option.textContent=label;select.append(option);}select.value=platform;
  const output=document.createElement('pre');const paint=()=>{output.textContent=(select.value!=='default'&&scripts?.[select.value]?.script)||scripts?.script||codexEnvText('No script configured','未配置脚本');};select.onchange=paint;paint();section.append(heading,select,output);parent.append(section);
}
async function codexEnvironmentDetail(configPath,context){
  const cwd=context.environmentCwd||currentCwd,box=$('settings-body'),ticket=++codexEnvironmentUi.viewRevision;
  box.textContent=codexEnvText('Loading environment…','正在加载环境…');
  const loaded=await context.call('workspace/environment/read',{cwd,configPath});
  if(!workspaceCurrent(context)||ticket!==codexEnvironmentUi.viewRevision||currentSettingsSection!=='environments')return;
  box.replaceChildren();box.append(codexEnvButton(codexEnvText('Environments','环境'),()=>renderSettingsSection('environments')));
  const environment=loaded.environment?.type==='success'?loaded.environment.environment:null;
  const heading=document.createElement('h1');heading.className='settings-h1';heading.textContent=environment?.name||baseName(configPath);box.append(heading,codexEnvButton(codexEnvText('Edit','编辑'),()=>codexEnvironmentEditor(configPath,context)));
  if(!environment){const error=document.createElement('p');error.className='workspace-error';error.textContent=loaded.environment?.error?.message||codexEnvText('Environment file no longer exists','环境文件已不存在');box.append(error);return;}
  codexEnvironmentPreview(box,codexEnvText('Setup script','设置脚本'),environment.setup);
  codexEnvironmentPreview(box,codexEnvText('Cleanup script','清理脚本'),environment.cleanup);
  const actions=document.createElement('section');actions.className='settings-card codex-env-card';const label=document.createElement('h2');label.textContent=codexEnvText('Actions','操作');actions.append(label);
  for(const action of environment.actions||[]){const details=document.createElement('details');const name=document.createElement('summary');name.textContent=action.name+(action.platform?' · '+({darwin:'macOS',linux:'Linux',win32:'Windows'}[action.platform]):'');const command=document.createElement('pre');command.textContent=action.command;details.append(name,command);actions.append(details);}
  if(!environment.actions?.length){const empty=document.createElement('p');empty.className='settings-sub';empty.textContent=codexEnvText('No actions configured','未配置操作');actions.append(empty);}box.append(actions);
}
async function codexEnvironmentSettings(){
  const context=codexEnvironmentContext(),cwd=currentCwd,box=$('settings-body'),ticket=++codexEnvironmentUi.viewRevision;
  const valid=()=>workspaceCurrent(context)&&ticket===codexEnvironmentUi.viewRevision&&currentSettingsSection==='environments';
  box.textContent=codexEnvText('Loading projects…','正在加载项目…');
  const projects=await context.call('desktop/project/list',{mode:'codex'});if(!valid())return;
  const folders=[...(projects.items||[]).filter(project=>project.path)];if(!folders.some(project=>codexEnvPath(project.path)===codexEnvPath(cwd)))folders.unshift({path:cwd,name:baseName(cwd)});
  const seen=new Set(),groups=await Promise.all(folders.filter(project=>{const path=codexEnvPath(project.path);if(seen.has(path))return false;seen.add(path);return true;}).map(async project=>{try{return {project,result:await context.call('workspace/environment/list',{cwd:project.path})};}catch(error){return {project,error};}}));if(!valid())return;
  box.replaceChildren();const heading=document.createElement('h1');heading.className='settings-h1';heading.textContent=codexEnvText('Local environments','本地环境');const subtitle=document.createElement('p');subtitle.className='settings-sub';subtitle.textContent=codexEnvText('Local environments tell ChatGPT how to set up worktrees for a project.','本地环境告诉 ChatGPT 如何为项目设置工作树。');box.append(heading,subtitle);
  const title=document.createElement('h2');title.textContent=codexEnvText('Select a project','选择项目');box.append(title);
  if(typeof codexProjectDialog==='function')box.append(codexEnvButton(codexEnvText('Add project','添加项目'),()=>codexProjectDialog(null,'',()=>renderSettingsSection('environments'))));
  const list=document.createElement('div');list.className='codex-env-project-list';list.setAttribute('role','list');list.setAttribute('aria-label',codexEnvText('Available projects','可用项目'));box.append(list);
  for(const {project,result,error} of groups){
    const projectContext={...context,environmentCwd:project.path},card=document.createElement('section');card.className='settings-card codex-env-card';card.setAttribute('role','listitem');const name=document.createElement('strong');name.textContent=project.name||project.label||baseName(project.path);const path=document.createElement('small');path.textContent=project.path;card.append(name,path);
    if(error){const failure=document.createElement('p');failure.className='workspace-error';failure.textContent=error.message;card.append(failure,codexEnvButton(codexEnvText('Retry','重试'),()=>renderSettingsSection('environments')));list.append(card);continue;}
    const local=[],inherited=[];for(const entry of result.environments||[])(codexEnvPath(codexEnvironmentOwner(entry.configPath))===codexEnvPath(project.path)?local:inherited).push(entry);
    const appendEntry=(parent,entry,inherited=false)=>{const label=codexEnvDisplayName(entry);const button=codexEnvButton(label,()=>codexEnvironmentDetail(entry.configPath,projectContext));button.className='codex-env-entry';button.title=entry.configPath;const description=inherited?codexEnvText('From ','来自 ')+baseName(codexEnvironmentOwner(entry.configPath))+' · '+baseName(entry.configPath):entry.type==='error'?entry.error?.message:baseName(entry.configPath);if(description&&description!==label){const file=document.createElement('small');file.textContent=description;button.append(file);}if(entry.type==='error')button.classList.add('workspace-error');parent.append(button);};
    for(const entry of local)appendEntry(card,entry);
    if(inherited.length){const details=document.createElement('details');const summary=document.createElement('summary');summary.textContent=codexEnvText('Inherited environments','继承的环境')+' ('+inherited.length+')';details.append(summary);for(const entry of inherited)appendEntry(details,entry,true);card.append(details);}
    if(!local.length){const empty=document.createElement('p');empty.className='settings-sub';empty.textContent=codexEnvText('No local environment is configured for this project yet','此项目尚未配置本地环境');card.append(empty);}
    card.append(codexEnvButton(codexEnvText('Create local environment','创建本地环境'),()=>codexEnvironmentEditor(codexEnvironmentNewPath(project.path,result.environments||[]),projectContext)));list.append(card);
  }
}
function platformOsToEnvironment(){return codexEnvironmentUi.platform;}
async function codexEnvironmentCreateFields(box,cwd,context){
  if(desktop.mode!=='codex')return null;
  const result=await context.call('workspace/environment/list',{cwd});if(!workspaceCurrent(context)||!box.isConnected)return null;
  const label=document.createElement('label');label.textContent=codexEnvText('Environment','环境');const select=document.createElement('select');select.className='settings-select';select._host=codexEnvHost();select.setAttribute('aria-label',label.textContent);const none=document.createElement('option');none.value='';none.textContent=codexEnvText('No environment','不使用环境');select.append(none);
  for(const entry of result.environments||[]){const option=document.createElement('option');option.value=entry.configPath;option.textContent=codexEnvDisplayName(entry);option.disabled=entry.type!=='success';select.append(option);}if(cwd===currentCwd)select.value=codexEnvironmentUi.selection||'';box.append(label,select);return select;
}
const codexEnvironmentBaseSettings=renderSettingsSection;
renderSettingsSection=async function(section){
  if(desktop.mode!=='codex'||section!=='environments')return codexEnvironmentBaseSettings(section);
  currentSettingsSection=section;document.querySelectorAll('.settings-nav-btn').forEach(button=>button.classList.toggle('active',button.dataset.sec===section));const context=codexEnvironmentContext();
  try{return await codexEnvironmentSettings();}catch(error){
    if(!workspaceCurrent(context)||currentSettingsSection!==section)return;
    const box=$('settings-body');box.replaceChildren();const heading=document.createElement('h1');heading.className='settings-h1';heading.textContent=codexEnvText('Local environments unavailable','本地环境暂不可用');const message=document.createElement('p');message.className='settings-sub';message.textContent=error.message;box.append(heading,message,codexEnvButton(codexEnvText('Retry','重试'),()=>renderSettingsSection(section)));
  }
};
const codexEnvironmentBaseCwd=updateCwdUI;
updateCwdUI=function(...args){const result=codexEnvironmentBaseCwd(...args);codexEnvironmentRefresh().catch(showError);return result;};
const codexEnvironmentBaseOpen=openThread;
openThread=async function(...args){const result=await codexEnvironmentBaseOpen(...args);codexEnvironmentRefresh().catch(showError);return result;};
const codexEnvironmentBaseMode=switchMode;
switchMode=async function(...args){const result=await codexEnvironmentBaseMode(...args);await codexEnvironmentRefresh();return result;};
const codexEnvironmentBaseNotification=handleServerNotification;
handleServerNotification=function(method,params){
  if(method==='workspace/environment/outputDelta'){
    const host=params.connectionId?remoteUi.byConnection.get(params.connectionId)?.profileId:'local';if(!host)return;
    const run=codexEnvironmentUi.runs.get(codexEnvKey(host,params.workspaceRoot));if(!run||run.id!==params.id)return;
    if((params.sequence||0)<=(run.outputSequence||0))return;run.outputSequence=params.sequence;
    run.log=((run.log||'')+String(params.delta||'')).slice(-1048576);
    if(run.log.charCodeAt(0)>=0xdc00&&run.log.charCodeAt(0)<=0xdfff)run.log=run.log.slice(1);
    codexEnvironmentUi.outputPending.add(codexEnvKey(host,params.workspaceRoot));
    if(!codexEnvironmentUi.outputFrame){codexEnvironmentUi.outputFrame=requestAnimationFrame(()=>{codexEnvironmentUi.outputFrame=null;const pending=[...codexEnvironmentUi.outputPending];codexEnvironmentUi.outputPending.clear();for(const key of pending){const current=codexEnvironmentUi.runs.get(key);if(current)codexEnvironmentRemember(current,JSON.parse(key)[0]);}});}return;
  }
  if(method==='workspace/environment/updated'){const host=params.connectionId?remoteUi.byConnection.get(params.connectionId)?.profileId:'local';if(host)codexEnvironmentRemember(params.run,host);return;}
  return codexEnvironmentBaseNotification(method,params);
};
codexChromeReady.then(()=>{
  const panel=document.createElement('section');panel.id='codex-environment-status';panel.hidden=true;panel.setAttribute('aria-live','polite');$('composer-host').before(panel);
  const toolbar=document.createElement('div');toolbar.id='codex-environment-actions';document.querySelector('#topbar .topbar-right').prepend(toolbar);
  const gear=$('codex-workspace-settings');gear.title=codexEnvText('Configure local environment','配置本地环境');gear.setAttribute('aria-label',gear.title);gear.onclick=()=>codexEnvironmentMenu(gear).catch(showError);
  codexEnvironmentRefresh().catch(showError);
}).catch(showError);
