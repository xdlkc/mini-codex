// Interactive local workspaces use the same Rust Git backend in native and preview.
const workspaceUi = { request:0, repository:null, worktrees:[], branches:[], filter:'active' };
function workspaceCapture(){const context=typeof captureRpcContext==='function'?captureRpcContext():{key:'local',call:rpcCall};return {...context,navigation:desktop.navigation};}
function workspaceSameHost(context){try{return context.key===workspaceCapture().key;}catch{return false;}}
function workspaceCurrent(context){return context.navigation===desktop.navigation&&workspaceSameHost(context);}
function workspaceRequire(context){if(!workspaceCurrent(context))throw new Error('The execution environment changed. Reopen this action in the current environment.');}
function workspaceButton(label,run,className='btn-pill') {
  const button=document.createElement('button');button.type='button';button.className=className;button.textContent=label;
  button.onclick=async()=>{button.disabled=true;try{await run();}catch(error){showError(error);}finally{button.disabled=false;}};return button;
}
function workspaceField(labelText,id,value='',placeholder='') {
  const label=document.createElement('label');label.htmlFor=id;label.textContent=labelText;
  const input=document.createElement('input');input.id=id;input.className='modal-input';input.value=value;input.placeholder=placeholder;input.autocomplete='off';
  return {label,input};
}
async function useWorkspace(worktree,context=workspaceCapture()) {
  workspaceRequire(context);
  const cwd=worktree.cwd||worktree.workspacePath||worktree.path;
  const ticket=++desktop.navigation;saveDraft();await flushDocument();if(ticket!==desktop.navigation||!workspaceSameHost(context))return;
  await newThread(false,true);if(!workspaceSameHost(context))throw new Error('The execution environment changed. Open the workspace again in Codex.');updateCwdUI(cwd);await refreshGitStatus();
  $('topbar-title').textContent=worktree.name||baseName(cwd);$('composer-input').focus();
}
async function createWorktreeDialog(cwd=currentCwd,context=workspaceCapture()) {
  workspaceRequire(context);
  closeFloatingMenu();
  openCustomModal('New worktree','<p>Start an isolated workspace from a Git branch or commit.</p><div id="worktree-create-fields" class="workspace-form"></div>');
  const box=$('worktree-create-fields');
  const name=workspaceField('Name','worktree-create-name','','Feature work');
  const base=workspaceField('Branch','worktree-create-base','HEAD','Branch, tag or commit');
  const branch=workspaceField('New branch (optional)','worktree-create-branch','','codex/…');
  box.append(name.label,name.input,base.label,base.input,branch.label,branch.input);
  const localHeading=document.createElement('span');localHeading.textContent='Local file state';box.append(localHeading);
  const localLabel=document.createElement('label');localLabel.className='workspace-checkbox';
  const local=document.createElement('input');local.type='checkbox';local.id='worktree-include-local-changes';
  localLabel.append(local,document.createTextNode('With local code changes'));box.append(localLabel);
  const note=document.createElement('small');note.id='worktree-local-state-note';note.className='workspace-note';
  const updateLocalState=()=>{note.textContent=local.checked?'Copies staged, unstaged and untracked files. The branch must point to the current HEAD. Ignored files stay in the source workspace.':'Starts with clean files from the selected branch. Local changes stay in the current workspace.';};
  local.onchange=updateLocalState;updateLocalState();box.append(note);
  const environment=typeof codexEnvironmentCreateFields==='function'?await codexEnvironmentCreateFields(box,cwd,context).catch(error=>{showError(error);return null;}):null;
  const create=workspaceButton('Create worktree',async()=>{
    workspaceRequire(context);const ticket=desktop.navigation;
    const params={cwd,baseRef:base.input.value.trim()||'HEAD'};
    if(environment)params.localEnvironmentConfigPath=environment.value||null;
    if(local.checked)params.includeLocalChanges=true;
    if(name.input.value.trim())params.name=name.input.value.trim();if(branch.input.value.trim())params.branch=branch.input.value.trim();
    const result=await context.call('workspace/worktree/create',params);
    if(result.setupRun&&typeof codexEnvironmentRemember==='function')codexEnvironmentRemember(result.setupRun,environment?._host||'local');
    if(ticket===desktop.navigation&&workspaceCurrent(context)&&$('worktree-create-fields')===box){closeModal();await useWorkspace(result.worktree,context);}toast('Worktree created');
  },'btn-pill primary');box.append(create);name.input.focus();
}
function archiveWorktreeDialog(worktree,cwd=currentCwd,context=workspaceCapture()) {
  workspaceRequire(context);
  openCustomModal('Archive worktree?','<p>Save a recoverable snapshot, then remove this checkout. The snapshot keeps commits, staged edits, unstaged edits and untracked files.</p><div id="worktree-archive-options" class="workspace-form"></div>');
  const box=$('worktree-archive-options');
  if(typeof codexEnvironmentArchiveControls==='function')codexEnvironmentArchiveControls(box,worktree,context);
  const label=document.createElement('label');label.className='workspace-checkbox';const include=document.createElement('input');include.type='checkbox';include.id='worktree-include-ignored';label.append(include,document.createTextNode('Also save ignored files'));box.append(label);
  const note=document.createElement('small');note.className='workspace-note';note.textContent='If this workspace has ignored files, choose this option to preserve them. No files are discarded by default.';box.append(note);
  box.append(workspaceButton('Cancel',async()=>closeModal()),workspaceButton('Archive',async()=>{
    workspaceRequire(context);await context.call('workspace/worktree/archive',{cwd,id:worktree.id,includeIgnored:include.checked});if(!workspaceCurrent(context))return;closeModal();
    const activePath=currentCwd;
    if(activePath===worktree.path||activePath===worktree.cwd||activePath.startsWith(worktree.path+'/'))await useWorkspace({cwd:worktree.sourceCwd||worktree.repoRoot,name:baseName(worktree.repoRoot)});
    await openSettingsView('git');toast('Worktree archived. You can restore it from Archived.');
  },'btn-pill primary'));
}
async function restoreWorktree(worktree,cwd=currentCwd,context=workspaceCapture()) {
  workspaceRequire(context);const ticket=desktop.navigation;const result=await context.call('workspace/worktree/restore',{cwd,id:worktree.id});if(ticket===desktop.navigation&&workspaceCurrent(context))await useWorkspace(result.worktree,context);toast('Worktree restored');
}
function createBranchDialog(cwd=currentCwd,context=workspaceCapture()) {
  workspaceRequire(context);
  closeFloatingMenu();openCustomModal('Create branch','<div id="branch-create-fields" class="workspace-form"></div>');const box=$('branch-create-fields');
  const branch=workspaceField('Branch name','branch-create-name','','codex/feature');const base=workspaceField('Start from','branch-create-base','HEAD');box.append(branch.label,branch.input,base.label,base.input);
  box.append(workspaceButton('Create and switch',async()=>{workspaceRequire(context);if(!branch.input.value.trim())return;await context.call('workspace/branch/create',{cwd,branch:branch.input.value.trim(),baseRef:base.input.value.trim()||'HEAD',switch:true});if(!workspaceCurrent(context))return;closeModal();await refreshGitStatus();toast('Branch created');},'btn-pill primary'));branch.input.focus();
}
async function showBranches(anchor) {
  const cwd=currentCwd,context=workspaceCapture();closeFloatingMenu();
  const result=await context.call('workspace/branch/list',{cwd});if(cwd!==currentCwd||!workspaceCurrent(context))return;
  openFloatingMenuAt('workspace-branches',anchor,'',false);const menu=$('floating-menu');menu.classList.add('workspace-branch-menu');
  const heading=document.createElement('div');heading.className='menu-section-title';heading.textContent='Switch branch';menu.append(heading);
  const search=document.createElement('input');search.className='menu-search';search.placeholder='Search branches';search.setAttribute('aria-label','Search branches');menu.append(search);
  const list=document.createElement('div');menu.append(list);
  const render=()=>{list.replaceChildren();for(const branch of result.data||[]){if(branch.isRemote||!branch.name.toLowerCase().includes(search.value.toLowerCase()))continue;
    const used=Boolean(branch.worktreePath&&!branch.current);const button=workspaceButton(branch.name+(branch.current?' ✓':used?' · In another worktree':''),async()=>{
      workspaceRequire(context);await context.call('workspace/branch/switch',{cwd,branch:branch.name});if(!workspaceCurrent(context))return;closeFloatingMenu();await refreshGitStatus();toast('Switched to '+branch.name);
    },'menu-item');button.disabled=branch.current||used;list.append(button);
  }};search.oninput=render;render();
  menu.append(menuButton('Create new branch…',()=>createBranchDialog(cwd,context)),menuButton('New worktree…',()=>createWorktreeDialog(cwd,context)));search.focus();
}
async function renderWorkspaceSettings() {
  const ticket=++workspaceUi.request,cwd=currentCwd,context=workspaceCapture();const box=$('settings-body');
  box.innerHTML='<h1 class="settings-h1">Git &amp; Worktrees</h1><p class="settings-sub">Loading repository…</p>';
  let result;
  try{result=await context.call('workspace/worktree/list',{cwd});}catch(error){if(ticket!==workspaceUi.request||!workspaceCurrent(context)||currentSettingsSection!=='git')return;box.replaceChildren();const heading=document.createElement('h1');heading.className='settings-h1';heading.textContent='Git & Worktrees';const note=document.createElement('p');note.className='settings-sub';note.textContent=error.message;box.append(heading,note,workspaceButton('Choose folder…',()=>openCreateProjectDialog('existing')));return;}
  if(ticket!==workspaceUi.request||!workspaceCurrent(context)||cwd!==currentCwd||currentSettingsSection!=='git')return;
  workspaceUi.repository=result.repository;workspaceUi.worktrees=result.data||[];
  box.innerHTML='<div class="workspace-settings-header"><h1 class="settings-h1">Git &amp; Worktrees</h1><div id="workspace-create-actions" class="workspace-actions"></div></div><p id="workspace-repository" class="settings-sub"></p><div id="workspace-filters" class="workspace-actions"></div><div id="workspace-list"></div>';
  $('workspace-repository').textContent=result.repository.repoRoot;
  $('workspace-create-actions').append(workspaceButton('New branch',()=>createBranchDialog(cwd,context)),workspaceButton('New worktree',()=>createWorktreeDialog(cwd,context),'btn-pill primary'));
  for(const [value,label] of [['active','Active'],['archived','Archived']]){const button=workspaceButton(label,async()=>{workspaceUi.filter=value;await renderWorkspaceSettings();});button.setAttribute('aria-pressed',String(value===workspaceUi.filter));$('workspace-filters').append(button);}
  const worktrees=workspaceUi.worktrees.filter(worktree=>workspaceUi.filter==='archived'?worktree.state==='archived':worktree.state!=='archived');
  for(const worktree of worktrees){
    const row=document.createElement('div');row.className='workspace-card';const info=document.createElement('div');info.className='workspace-card-info';
    const name=document.createElement('strong');name.textContent=worktree.name||baseName(worktree.path);
    const path=document.createElement('small');path.textContent=worktree.path;const branch=document.createElement('small');branch.textContent=(worktree.branch||'Detached HEAD')+(worktree.managed?' · Managed worktree':' · Existing checkout');info.append(name,path,branch);
    if(worktree.snapshot){const note=document.createElement('small');note.textContent=`Snapshot · ${worktree.snapshot.entryCount??0} files${worktree.snapshot.includeIgnored?' · Includes ignored files':''}`;info.append(note);}
    if(worktree.error){const error=document.createElement('p');error.className='workspace-error';error.textContent=worktree.error;info.append(error);}
    const actions=document.createElement('div');actions.className='workspace-actions';
    if(worktree.state==='archived')actions.append(workspaceButton('Restore',()=>restoreWorktree(worktree,cwd,context)));
    else {actions.append(workspaceButton(worktree.cwd===cwd||worktree.path===cwd?'Current':'Open',()=>useWorkspace(worktree,context)));if(worktree.managed)actions.append(workspaceButton('Archive',async()=>archiveWorktreeDialog(worktree,cwd,context)));}
    row.append(info,actions);$('workspace-list').append(row);
  }
  if(!worktrees.length){const empty=document.createElement('p');empty.className='empty-note';empty.textContent=workspaceUi.filter==='archived'?'No archived worktrees':'No worktrees';$('workspace-list').append(empty);}
}
const workspaceBaseSettings=renderSettingsSection;
renderSettingsSection=async function(section){
  workspaceUi.request++;
  if(section!=='git')return workspaceBaseSettings(section);
  currentSettingsSection=section;document.querySelectorAll('.settings-nav-btn').forEach(button=>button.classList.toggle('active',button.dataset.sec===section));await renderWorkspaceSettings();
};
let workspaceGitRequest=0;
refreshGitStatus=async function(){
  const ticket=++workspaceGitRequest,cwd=currentCwd,context=workspaceCapture();if(!cwd)return;
    try{const status=await context.call('workspace/gitStatus',{cwd});if(ticket!==workspaceGitRequest||!workspaceCurrent(context)||cwd!==currentCwd)return;latestGitStatus=status;latestGitStatusContext=context.key+'\0'+cwd;
    $('branch-badge').style.display=status.isGit===false?'none':'inline-flex';$('branch-name').textContent=status.branch||(status.isGit===false?'':'Detached HEAD');
    $('top-diff-add').textContent='+'+(status.totalAdded||0);$('top-diff-del').textContent='-'+(status.totalDeleted||0);
    if(drawerOpen&&currentDrawerTab==='diff')renderDrawerContent();
    if(typeof codexSyncChrome==='function')codexSyncChrome();
  }catch(error){if(ticket!==workspaceGitRequest||!workspaceCurrent(context)||cwd!==currentCwd)return;latestGitStatus=null;$('branch-badge').style.display='none';$('top-diff-add').textContent='+0';$('top-diff-del').textContent='-0';}
};
const workspaceReady=desktopReady.then(()=>{
  const badge=$('branch-badge');badge.onclick=()=>showBranches(badge).catch(showError);badge.setAttribute('role','button');badge.tabIndex=0;badge.title='Switch branch';badge.onkeydown=event=>{if(event.key==='Enter'||event.key===' '){event.preventDefault();badge.click();}};
  const button=document.createElement('button');button.id='workspace-environment';button.className='composer-chip-btn codex-only';button.textContent='Local ▾';button.setAttribute('aria-label','Execution environment');
  button.onclick=event=>{event.stopPropagation();openFloatingMenuAt('execution-environment',button,'',true);const menu=$('floating-menu');menu.append(menuButton('New worktree…',()=>createWorktreeDialog()),menuButton('Manage worktrees',()=>openSettingsView('git')));};
  $('project-pill-btn').after(button);refreshGitStatus();
}).catch(showError);
