// Reference: thread-handoff-modal and app-primary, 26.1013.40934.
const codexHandoffUi={jobs:new Map(),locations:new Map(),dialog:null,readonly:false};
const codexHandoffText=(en,zh)=>codexText(en,zh);
const codexHandoffKey=(host,id)=>JSON.stringify([host,id]);
function codexHandoffHost(){return remoteContext()||'local';}
function codexHandoffSelected(job){return desktop.mode==='codex'&&currentThread===job.thread.id&&codexHandoffHost()===job.context.host;}
function codexHandoffBusy(job){return job&&['running','completing','unknown'].includes(job.status);}
function codexHandoffCurrentJob(){return codexHandoffUi.jobs.get(codexHandoffKey(codexHandoffHost(),currentThread));}
function codexHandoffError(error){
  const message=error?.message||String(error);
  const labels=[
    [/queued messages/,['Send or remove queued messages before handing off this chat.','移交此聊天前，请先发送或移除队列中的消息。']],[/voice call/,['End the voice call before handing off this chat.','请先结束语音通话，再移交此聊天。']],
    [/local checkout has tracked or untracked/,['Stash or commit your local changes to hand off','移交前，请贮藏或提交本地更改']],
    [/local branch points to a different commit/,['The local branch points to a different commit than the worktree.','本地同名分支与工作树指向不同的提交。']],
    [/Wait for the current response/,['Wait for the current response to finish before moving this chat','请等待当前回复完成后再移动此聊天']]];
  for(const [pattern,pair] of labels)if(pattern.test(message))return codexHandoffText(...pair);
  return message;
}
function codexHandoffBranchValid(branch){return Boolean(branch)&&!branch.startsWith('-')&&!branch.endsWith('/')&&!branch.endsWith('.')&&branch!=='@'&&!/[\s\x00-\x1f\x7f~^:?*\[\\]/.test(branch)&&!branch.includes('..')&&!branch.includes('@{')&&!branch.includes('//')&&!branch.split('/').some(part=>part.startsWith('.')||part.endsWith('.lock'));}
function codexHandoffDirection(workspace){return workspace.repository?.repoRoot===workspace.localCheckout?.path?'toWorktree':'toLocal';}
function codexHandoffPlan(dialog){
  const {data,state}=dialog,workspace=data.workspace,source=data.thread,direction=codexHandoffDirection(workspace);
  const params={threadId:source.id,direction},blocked={readOnly:['Fork this imported chat first.','请先为导入的聊天创建分支。'],unavailable:['This chat is not available for handoff.','此聊天暂时无法移交。'],queuedFollowUps:['Send or remove queued messages before handing off this chat.','移交此聊天前，请先发送或移除队列中的消息。'],voiceCall:['End the voice call before handing off this chat.','请先结束语音通话，再移交此聊天。']};
  if(blocked[data.blockedReason])return {params,error:codexHandoffText(...blocked[data.blockedReason])};
  if(workspace.pending?.length)return {params,error:codexHandoffText('Finish the current handoff before starting another.','请先完成当前交接，再开始另一项交接。')};
  const running=source.status?.type==='active',branches=(workspace.branches||[]).filter(branch=>!branch.isRemote&&!branch.symbolic);
  if(direction==='toWorktree'&&state.targetPath){
    const target=(workspace.worktrees||[]).find(tree=>tree.path===state.targetPath&&!tree.primary&&!tree.locked&&!tree.prunable&&tree.state==='active');
    if(!target)return {params,error:codexHandoffText('Unable to resolve the selected worktree','无法解析所选工作树')};
    params.targetPath=target.path;params.continueExisting=true;
    return {params,error:running?codexHandoffText('Wait for the current response to finish before moving this chat','请等待当前回复完成后再移动此聊天'):null};
  }
  const needsName=direction==='toLocal'||!workspace.sourceBranch||workspace.sourceBranch===workspace.defaultBranch;
  const branch=(needsName?state.branch:workspace.sourceBranch||'').trim();
  if(!branch)return {params,error:codexHandoffText(direction==='toLocal'?'Enter a branch name':'Enter a worktree branch name.',direction==='toLocal'?'输入分支名称':'输入工作树分支名称。')};
  if(!codexHandoffBranchValid(branch))return {params,error:codexHandoffText(branch.endsWith('/')?'Branch name cannot end with “/”.':'Enter a valid Git branch name.',branch.endsWith('/')?'分支名不能以“/”结尾。':'请输入有效的 Git 分支名称。')};
  const existing=branches.find(item=>item.name===branch),sourceRoot=workspace.repository?.repoRoot;
  if(direction==='toWorktree'){
    if(branch===workspace.defaultBranch)return {params,error:codexHandoffText('Worktree branch must be different from the default branch.','工作树分支必须不同于默认分支。')};
    if(needsName&&existing)return {params,error:codexHandoffText('Branch already exists.','分支已存在。')};
    params.worktreeBranch=branch;
    if(workspace.sourceBranch&&branch===workspace.sourceBranch){
      const local=branches.find(item=>item.name===state.localBranch&&item.name!==branch&&(!item.worktreePath||item.worktreePath===sourceRoot));
      if(!local)return {params,error:codexHandoffText('Select local checkout branch','选择本地检出分支')};
      params.localBranch=local.name;
    }
  }else{
    const local=workspace.localCheckout;
    if(!local||local.state!=='active')return {params,error:codexHandoffText('No local workspace found for this worktree','未找到此工作树的本地工作空间')};
    if(local.dirtyStatus?.clean!==true)return {params,error:codexHandoffText('Stash or commit your local changes to hand off','移交前，请贮藏或提交本地更改')};
    params.localBranch=branch;
    if(existing&&existing.commit!==workspace.head)return {params,error:codexHandoffText('The local branch points to a different commit than the worktree.','本地同名分支与工作树指向不同的提交。')};
  }
  if(existing?.worktreePath&&![sourceRoot,workspace.localCheckout?.path].includes(existing.worktreePath))return {params,error:codexHandoffText('Branch is already checked out in another worktree','分支已在其他工作树检出')};
  if(running)params.interrupt=true;
  return {params,error:null};
}
function codexHandoffMoveMap(map,source,target){
  if(!map?.has(source))return false;
  const destination=map.get(target),hasDestination=Array.isArray(destination)?destination.length>0:typeof destination==='string'?destination.length>0:destination!==undefined;
  if(hasDestination)return true;
  const value=map.get(source);map.set(target,Array.isArray(value)?value.map(item=>item&&typeof item==='object'?{...item}:item):value);map.delete(source);return false;
}
async function codexHandoffMoveState(job,target){
  if(job.stateMoved)return;job.stateMoved=true;
  if(typeof codexSidebarUi!=='undefined')await codexSidebarUi.write;
  if(typeof webSearchFlushWrites==='function')await webSearchFlushWrites();
  if(codexHandoffSelected(job))saveDraft();
  const source=job.thread.id;
  const draftMaps=[desktop.drafts];if(typeof filesUi!=='undefined'){fileDraftMaps();draftMaps.push(filesUi.drafts,filesUi.mentions);}
  const populated=(map,id)=>{const value=map.get(id);return Array.isArray(value)?value.length>0:typeof value==='string'&&value.length>0;};
  const conflict=draftMaps.some(map=>populated(map,target.id))&&draftMaps.some(map=>populated(map,source));
  job.movedDrafts=[];
  if(!conflict)for(const map of draftMaps){if(map.has(source))job.movedDrafts.push({map,source,value:JSON.stringify(map.get(source))});codexHandoffMoveMap(map,source,target.id);}
  if(typeof webSearchDrafts==='function')codexHandoffMoveMap(webSearchDrafts(),webSearchThreadKey(job.context.host,source),webSearchThreadKey(job.context.host,target.id));
  const seen=new Set();desktop.tabs=desktop.tabs.map(tab=>tab.id===source?{...tab,id:target.id,title:target.name||target.preview||tab.title,running:target.status?.type==='active'}:tab).filter(tab=>{if(seen.has(tab.id))return false;seen.add(tab.id);return true;});
  const patch={drafts:Object.fromEntries(desktop.drafts)};
  if(typeof codexSidebarPreferences==='function'){
    const preferences=codexSidebarPreferences(),from=codexSidebarKey(job.context.host,source),to=codexSidebarKey(job.context.host,target.id);
    const replace=list=>[...new Set(list.map(key=>key===from?to:key))];
    const sections={...preferences.threadSections};if(Object.hasOwn(sections,from)&&!Object.hasOwn(sections,to)){sections[to]=sections[from];delete sections[from];}
    codexSidebarUi.preferences={...preferences,order:replace(preferences.order),pinnedOrder:replace(preferences.pinnedOrder),threadSections:sections};patch.codexSidebar=codexSidebarUi.preferences;
  }
  if(typeof webSearchDrafts==='function')patch.webSearchDrafts=Object.fromEntries(webSearchDrafts());
  await savePreferences(patch);
  if(conflict)toast(codexHandoffText('Your original chat draft was retained.','原聊天的草稿已保留。'),{label:codexHandoffText('Open','打开'),run:()=>openThread(source)});
}
async function codexHandoffFinish(job,result){
  if(job.finished)return;const target=result.thread;if(!target?.id)throw new Error(codexHandoffText('The handoff did not return a destination chat.','移交没有返回目标聊天。'));
  job.chatCopied=target.id!==job.thread.id;
  if(!job.chatCopied&&!result.handoff?.warnings?.length)throw new Error(codexHandoffText('The handoff did not return a destination chat.','移交没有返回目标聊天。'));
  if(typeof remoteMark==='function')remoteMark(remoteUi.threads,target.id,job.context.host==='local'?null:job.context.host);
  job.finished=true;job.result=result;job.status='completing';job.stage='switching-thread';
  const show=job.autoOpen&&codexHandoffSelected(job)&&job.intent===codexForkIntent();
  const view=show?{open:drawerOpen,tab:currentDrawerTab,scroll:$('conversation-wrap').scrollTop,auto:desktop.autoScroll,command:$('quick-cmd-input')?.value}:null;
  try{if(job.chatCopied)await codexHandoffMoveState(job,target);}catch(error){job.status='warning';job.uiError=codexHandoffError(error);}
  try{if(show&&job.chatCopied&&codexHandoffSelected(job)&&job.intent===codexForkIntent()){
    await openThread(target.id);
    if(currentThread===target.id&&codexHandoffHost()===job.context.host){
      currentModel=target.model||currentModel;if(target.reasoningEffort)currentEffort=target.reasoningEffort;updateModelAndEffortUI();
      if(view?.open&&['diff','files','terminal'].includes(view.tab)){drawerOpen=true;currentDrawerTab=view.tab;$('inspector-drawer').classList.remove('closed');await renderDrawerContent();if(view.command!==undefined&&$('quick-cmd-input'))$('quick-cmd-input').value=view.command;}
      if(view&&!view.auto){desktop.autoScroll=false;$('conversation-wrap').scrollTop=view.scroll;}
      remoteRemember();restoreDraft();$('composer-input').focus();
    }
  }else if(job.chatCopied)toast(codexHandoffText('Chat handed off','聊天已移交'),{label:codexHandoffText('Open','打开'),run:()=>openThread(target.id)});
  // openThread saves the outgoing composer before navigation. Remove only the
  // identical source snapshot it recreated; preserve any genuinely newer draft.
  for(const entry of job.movedDrafts||[])if(entry.map.has(entry.source)&&JSON.stringify(entry.map.get(entry.source))===entry.value)entry.map.delete(entry.source);
  await savePreferences({drafts:Object.fromEntries(desktop.drafts)});
  await refreshThreads();
  }catch(error){job.status='warning';job.uiError=codexHandoffError(error);}
  finally{job.status=job.uiError||result.handoff?.warnings?.length?'warning':'success';job.stage='ready';codexHandoffPaint();if(codexHandoffUi.dialog?.job===job)codexHandoffProgress(job);}
}
async function codexHandoffCheck(job){
  const context=codexThreadContext(job.thread.id);if(context.host!==job.context.host)throw new Error(codexHandoffText('This chat’s execution host changed. Reopen it on the original host to check the handoff.','此聊天的执行主机已更改。请在原主机上检查移交状态。'));job.context=context;
  const result=await job.context.call('thread/handoff/status',{threadId:job.thread.id,operationId:job.operationId},15000);
  if(result.active)return false;
  const operation=result.operation;
  if(operation?.status==='committed'&&operation.ownerThreadId){const response=await job.context.call('thread/read',{threadId:operation.ownerThreadId});await codexHandoffFinish(job,{thread:response.thread,handoff:operation});return true;}
  if(operation){job.status='error';job.error=codexHandoffError(operation.rollbackError||operation.error||codexHandoffText('The handoff needs attention.','此移交需要关注。'));job.recovery=operation;codexHandoffPaint();if(codexHandoffUi.dialog?.job===job)codexHandoffProgress(job);return true;}
  return false;
}
async function codexHandoffStart(dialog){
  const plan=codexHandoffPlan(dialog);if(dialog.starting||plan.error||codexHandoffUi.dialog!==dialog)return;
  if(!codexForkCurrent(dialog.intent)||!codexHandoffSelected({thread:dialog.data.thread,context:dialog.context}))return;
  const key=codexHandoffKey(dialog.context.host,dialog.data.thread.id),existing=codexHandoffUi.jobs.get(key);if(codexHandoffBusy(existing)){codexHandoffProgress(existing);return existing.promise;}
  dialog.starting=true;dialog.validate?.();
  try{saveDraft();await flushDocument();if(typeof filesUi!=='undefined'&&filesUi.importing)await new Promise(resolve=>filesUi.waiters.push(resolve));await webSearchFlushWrites();saveDraft();await savePreferences({drafts:Object.fromEntries(desktop.drafts)});}
  catch(error){dialog.starting=false;dialog.validate?.();throw error;}
  if(codexHandoffUi.dialog!==dialog||!codexForkCurrent(dialog.intent)){dialog.starting=false;dialog.validate?.();if(codexHandoffUi.dialog===dialog)closeModal();return;}
  const params={...plan.params,model:currentModel,reasoningEffort:currentEffort,sandbox:currentSandbox,approvalPolicy:currentSandbox==='danger-full-access'?'never':'on-request',operationId:'handoff_'+(globalThis.crypto?.randomUUID?.()||Date.now().toString(36)+'_'+Math.random().toString(36).slice(2))};
  const targetBranch=params.continueExisting?dialog.data.workspace.worktrees?.find(tree=>tree.path===params.targetPath)?.branch:params.direction==='toLocal'?params.localBranch:params.worktreeBranch;
  const job={thread:dialog.data.thread,context:dialog.context,params,targetBranch,operationId:params.operationId,intent:dialog.intent,autoOpen:true,status:'running',stage:params.direction==='toLocal'?'stash-source-changes':params.targetPath?'reuse-existing-worktree':'create-new-worktree'};
  codexHandoffUi.jobs.set(key,job);codexHandoffPaint();codexHandoffProgress(job);
  job.promise=(async()=>{
    try{const result=await job.context.call('thread/handoff',params,180000);await codexHandoffFinish(job,result);return result;}
    catch(error){
      if(job.finished)return job.result;
      if(job.committedTarget){try{const response=await job.context.call('thread/read',{threadId:job.committedTarget});await codexHandoffFinish(job,{thread:response.thread,handoff:{warnings:[]}});return job.result;}catch{}}
      job.error=codexHandoffError(error);job.status=/timed out|connection|SSH|disconnect/i.test(error.message||'')?'unknown':'error';
      if(job.status==='unknown')try{await codexHandoffCheck(job);}catch{}
      codexHandoffPaint();if(codexHandoffUi.dialog?.job===job)codexHandoffProgress(job);
      else toast(job.error,{label:codexHandoffText('View details','查看详情'),run:async()=>codexHandoffProgress(job)});
    }
  })();return job.promise;
}
// UI composition is separate from the transfer above so races are tested on the
// real production functions without reproducing them inside a test controller.
function codexHandoffElement(tag,className='',text=''){const node=document.createElement(tag);node.className=className;node.textContent=text;return node;}
function codexHandoffModal(title){
  openCustomModal(title,'');$('modal-backdrop').classList.add('codex-handoff-modal');
  const card=$('modal-body').closest('.modal-card');card.setAttribute('role','dialog');card.setAttribute('aria-modal','true');card.setAttribute('aria-label',title);
  card.dataset.handoffAria='true';
  const body=$('modal-body'),icon=codexHandoffElement('span','codex-handoff-icon');icon.innerHTML=codexIcon('handoff');icon.setAttribute('aria-hidden','true');body.append(icon);
  const heading=codexHandoffElement('h2','codex-handoff-heading',title);body.append(heading);if(!card.contains(document.activeElement))card.querySelector('button')?.focus();return body;
}
function codexHandoffSelect(label,choices,value,changed){
  const row=codexHandoffElement('label','codex-handoff-row',label),select=codexHandoffElement('select','codex-handoff-select');select.setAttribute('aria-label',label);
  for(const item of choices){const option=codexHandoffElement('option','',item.label);option.value=item.value;select.append(option);}select.value=value;select.onchange=()=>changed(select.value);row.append(select);return row;
}
function codexHandoffForm(dialog){
  if(codexHandoffUi.dialog!==dialog)return;const {data,state}=dialog,workspace=data.workspace,direction=codexHandoffDirection(workspace),root=dialog.body;root.replaceChildren();
  const icon=codexHandoffElement('span','codex-handoff-icon');icon.innerHTML=codexIcon('handoff');icon.setAttribute('aria-hidden','true');root.append(icon);
  const title=codexHandoffText(direction==='toLocal'?'Bring changes back to local checkout':'Hand off chat to worktree',direction==='toLocal'?'将更改带回本地检出':'将对话移交至工作树');$('modal-title').textContent=title;root.closest('.modal-card').setAttribute('aria-label',title);root.append(codexHandoffElement('h2','codex-handoff-heading',title));
  const subtitle=codexHandoffElement('p','codex-handoff-description');
  const needsName=!workspace.sourceBranch||workspace.sourceBranch===workspace.defaultBranch;
  if(direction==='toLocal'){
    subtitle.append(document.createTextNode(codexHandoffText('Check out branch ','在本地工作空间检出分支 ')));
    const input=codexHandoffElement('input','codex-handoff-branch');input.value=state.branch;input.autocomplete='off';input.setAttribute('aria-label',codexHandoffText('Local branch name','本地分支名称'));input.oninput=()=>{state.branch=input.value;validate();};subtitle.append(input,document.createTextNode(codexHandoffText(' in a local workspace and detach it from worktree.','，并从工作树分离。')));
    root.append(subtitle,codexHandoffElement('p','codex-handoff-description',codexHandoffText('Bringing changes back to local checkout','正在将更改带回本地检出')));
    const target=codexHandoffElement('div','codex-handoff-workspace',baseName(workspace.localCheckout?.path||''));target.title=workspace.localCheckout?.path||'';root.append(target);
  }else{
    if(state.targetPath)subtitle.textContent=codexHandoffText('Continue this chat in the selected worktree without changing its branch','在所选工作树中继续此聊天，不更改其分支');
    else if(needsName)subtitle.textContent=codexHandoffText('Create and check out a branch in a new worktree to continue working in parallel.','在新的工作树中创建并检出一个分支，以继续并行工作。');
    else subtitle.append(document.createTextNode(codexHandoffText('Check out branch ','在新工作树中检出分支 ')),codexHandoffElement('span','codex-handoff-branch-chip',workspace.sourceBranch),document.createTextNode(codexHandoffText(' in a new worktree to continue working in parallel.','，以继续并行工作。')));root.append(subtitle);
    const trees=(workspace.worktrees||[]).filter(tree=>!tree.primary&&tree.path!==workspace.repository.repoRoot&&tree.branch&&!tree.locked&&!tree.prunable&&tree.state==='active');
    if(trees.length)root.append(codexHandoffSelect(codexHandoffText('Worktree','工作树'),[{value:'',label:codexHandoffText('Create a new worktree','创建新工作树')},...trees.map(tree=>({value:tree.path,label:tree.branch||tree.path}))],state.targetPath,value=>{state.targetPath=value;codexHandoffForm(dialog);root.querySelector('select')?.focus();}));
    if(!state.targetPath){
      if(needsName){const label=codexHandoffElement('label','codex-handoff-field',codexHandoffText('Branch name','分支名称')),input=codexHandoffElement('input','codex-handoff-branch');input.value=state.branch;input.placeholder=codexHandoffText('new-branch','新分支');input.autocomplete='off';input.setAttribute('aria-label',codexHandoffText('Worktree branch name','工作树分支名称'));input.oninput=()=>{state.branch=input.value;validate();};label.append(input);root.append(label);}
      else{
        const branches=(workspace.branches||[]).filter(branch=>!branch.isRemote&&!branch.symbolic&&branch.name!==workspace.sourceBranch&&(!branch.worktreePath||branch.worktreePath===workspace.repository.repoRoot));
        branches.sort((a,b)=>Number(b.name===workspace.defaultBranch)-Number(a.name===workspace.defaultBranch));
        root.append(codexHandoffSelect(codexHandoffText('Local workspace will switch to','本地工作空间将切换至'),branches.length?branches.map(branch=>({value:branch.name,label:branch.name})):[{value:'',label:codexHandoffText('No other local branches are available','没有其他本地分支可用')}],state.localBranch,value=>{state.localBranch=value;validate();}));
      }
    }
  }
  const warning=codexHandoffElement('p','codex-handoff-warning',codexHandoffText('This chat is running, so handing it off will interrupt the current response','此聊天正在进行，移交会中断当前回复'));
  const action=codexHandoffElement('button','codex-handoff-primary',codexHandoffText(direction==='toLocal'?'Bring changes back':'Hand off',direction==='toLocal'?'带回更改':'移交'));action.type='button';action.onclick=()=>codexHandoffStart(dialog).catch(error=>{errorNode.textContent=codexHandoffError(error);});root.append(action);
  const errorNode=codexHandoffElement('p','codex-handoff-error');errorNode.setAttribute('role','alert');root.append(errorNode);
  function validate(){const plan=codexHandoffPlan(dialog);action.disabled=Boolean(plan.error)||Boolean(dialog.starting);errorNode.textContent=plan.error||'';if(data.thread.status?.type==='active'&&!state.targetPath)root.insertBefore(warning,action);else warning.remove();for(const input of root.querySelectorAll('input,select'))input.disabled=Boolean(dialog.starting);}
  dialog.validate=validate;validate();root.onkeydown=event=>{if(event.key==='Enter'&&event.target.tagName==='INPUT'){event.preventDefault();if(!action.disabled)action.click();}};
  root.querySelector('input')?.focus();
}
async function codexHandoffDialog(thread){
  const context=codexThreadContext(thread.id),key=codexHandoffKey(context.host,thread.id),job=codexHandoffUi.jobs.get(key);
  if(codexHandoffBusy(job)){codexHandoffProgress(job);return;}
  closeFloatingMenu();const intent=codexForkIntent(),body=codexHandoffModal(codexHandoffText('Checking for ability to hand off…','正在检查能否移交…'));
  const dialog={context,intent,body,thread};codexHandoffUi.dialog=dialog;body.append(codexHandoffElement('p','codex-handoff-description',codexHandoffText('Checking available local workspaces…','正在检查可用的本地工作空间…')));
  try{
    const data=await context.call('thread/handoff/inspect',{threadId:thread.id},30000);
    if(codexHandoffUi.dialog!==dialog||!codexForkCurrent(intent)||!codexHandoffSelected({thread,context}))return;
    dialog.data=data;const branches=(data.workspace.branches||[]).filter(branch=>!branch.isRemote&&!branch.symbolic&&branch.name!==data.workspace.sourceBranch&&(!branch.worktreePath||branch.worktreePath===data.workspace.repository.repoRoot));
    dialog.state={targetPath:'',branch:codexHandoffDirection(data.workspace)==='toLocal'?data.workspace.sourceBranch||'':'',localBranch:branches.find(branch=>branch.name===data.workspace.defaultBranch)?.name||branches[0]?.name||''};codexHandoffForm(dialog);
  }catch(error){if(codexHandoffUi.dialog===dialog){body.append(codexHandoffElement('p','codex-handoff-error',codexHandoffError(error)));const retry=codexHandoffElement('button','codex-handoff-primary',codexHandoffText('Retry','重试'));retry.onclick=()=>codexHandoffDialog(thread);body.append(retry);}}
}
const codexHandoffStageLabels={
  'create-new-worktree':['Creating new worktree','正在创建新工作树'],'reuse-existing-worktree':['Reusing existing worktree','正在复用现有工作树'],
  'stash-source-changes':['Stashing uncommitted changes','正在暂存未提交的更改'],'checkout-local-branch':['Checking out the local branch','正在本地检出分支'],
  'stash-target-worktree-changes':['Stashing worktree changes','正在暂存工作树更改'],'checkout-worktree-branch':['Checking out the worktree branch','正在工作树中检出分支'],
  'detach-worktree-branch':['Detaching branch from worktree','正在从工作树中分离分支'],'apply-changes-to-worktree':['Applying uncommitted changes to worktree','正在将未提交的更改应用到工作树'],
  'apply-changes-to-local':['Applying uncommitted changes locally','正在本地应用未提交的更改'],'switching-thread':['Moving chat to the destination workspace','正在将聊天移至目标工作空间']};
function codexHandoffProgress(job){
  const local=job.params.direction==='toLocal',titles={running:local?['Handing off to local','正在移交给本地']:['Handing off to worktree','正在移交给工作树'],unknown:['Handoff needs attention','移交需要关注'],success:local?['Handed off to local','已移交给本地']:['Handed off to worktree','已移交给工作树'],warning:['Handoff needs attention','移交需要关注'],error:local?['Failed to hand off to local','移交给本地失败']:['Failed to hand off to worktree','移交给工作树失败']};
  const body=codexHandoffModal(codexHandoffText(...titles[job.status==='completing'?'running':job.status]));codexHandoffUi.dialog={job,body};
  if(job.status==='running'||job.status==='completing'){
    body.append(codexHandoffElement('p','codex-handoff-description',codexHandoffText('Hang tight, this may take a few moments. You can close this modal, we’ll let you know when the hand-off is finished.','请稍等，这可能需要一些时间。你可以关闭此窗口，移交完成后我们会通知你。')));
    const stages=job.params.continueExisting?['reuse-existing-worktree','switching-thread']:local?['stash-source-changes','detach-worktree-branch','checkout-local-branch','apply-changes-to-local','switching-thread']:[job.params.targetPath?'reuse-existing-worktree':'create-new-worktree','stash-source-changes','checkout-local-branch','stash-target-worktree-changes','checkout-worktree-branch','apply-changes-to-worktree','switching-thread'];
    const position=stages.indexOf(job.stage),steps=codexHandoffElement('ol','codex-handoff-steps');
    for(const [index,stage] of stages.entries()){const item=codexHandoffElement('li',index<position?'done':index===position?'running':'',codexHandoffText(...codexHandoffStageLabels[stage]));if(index===position)item.setAttribute('aria-current','step');steps.append(item);}body.append(steps);
  }else if(job.status==='success'||job.status==='warning'){
    const branch=job.targetBranch||job.params.localBranch||job.params.worktreeBranch||'',subtitle=codexHandoffElement('p','codex-handoff-description');
    if(job.chatCopied===false)subtitle.textContent=codexHandoffText('Git changes were handed off, but this chat could not be switched to the destination.','Git 更改已移交，但此聊天未能切换到目标位置。');
    else if(branch)subtitle.append(document.createTextNode(codexHandoffText(local?'You’re now working locally on ':job.params.continueExisting?'You’re now working in the worktree on ':'You’re now working in a new worktree on ',local?'你当前正在本地处理 ':job.params.continueExisting?'你现在正在工作树中处理 ':'你现在正在新的工作树中处理 ')),codexHandoffElement('span','codex-handoff-branch-chip',branch),document.createTextNode(codexHandoffText('.','。')));
    else subtitle.textContent=codexHandoffText(local?'Your chat is now in the local workspace.':'Your chat is now in the worktree.',local?'聊天已移至本地工作空间。':'聊天已移至工作树。');body.append(subtitle);
    const statusIcon=body.querySelector('.codex-handoff-icon');if(job.status==='success'){statusIcon.innerHTML=codexIcon('handoffSuccess');statusIcon.classList.add('success');}
    for(const warning of [...(job.result?.handoff?.warnings||[]),...(job.uiError?[job.uiError]:[])]){
      if(job.chatCopied===false){const details=codexHandoffElement('details','codex-handoff-details');details.append(codexHandoffElement('summary','',codexHandoffText('Details','详情')),codexHandoffElement('pre','',warning));body.append(details);}
      else body.append(codexHandoffElement('p','codex-handoff-warning',warning));
    }
  }else{
    body.append(codexHandoffElement('p','codex-handoff-error',job.status==='unknown'?codexHandoffText('The handoff result is not confirmed yet. Check its status before retrying.','尚未确认移交结果。重试前请先检查状态。'):job.error||codexHandoffText('The handoff failed.','移交未成功。')));
    if(job.error){const details=codexHandoffElement('details','codex-handoff-details'),summary=codexHandoffElement('summary','',codexHandoffText('Details','详情'));details.append(summary,codexHandoffElement('pre','',job.error));body.append(details);}
    const check=codexHandoffElement('button','codex-handoff-primary',codexHandoffText(job.status==='unknown'?'Check status':'Retry',job.status==='unknown'?'检查状态':'重试'));
    check.onclick=async()=>{check.disabled=true;try{if(job.status==='unknown'){if(!await codexHandoffCheck(job))toast(codexHandoffText('The handoff has not finished.','移交尚未完成。'));}else await codexHandoffDialog(job.thread);}catch(error){toast(codexHandoffError(error));}finally{check.disabled=false;}};body.append(check);
  }
}
function codexHandoffPaint(){
  const job=codexHandoffCurrentJob(),busy=codexHandoffBusy(job),input=$('composer-input');
  $('composer-host').inert=Boolean(busy)||(typeof codexPermissionUi!=='undefined'&&Boolean(codexPermissionUi.loading));
  if(busy){input.readOnly=true;codexHandoffUi.readonly=true;}else if(codexHandoffUi.readonly){input.readOnly=false;codexHandoffUi.readonly=false;}
  updateSubmitButton();let strip=$('codex-handoff-strip');if(!strip){strip=codexHandoffElement('button','');strip.id='codex-handoff-strip';strip.type='button';$('workspace-tabs').after(strip);}
  strip.hidden=!job||desktop.mode!=='codex';if(job){strip.textContent=codexHandoffText(...(busy?job.params.direction==='toLocal'?['Handing off to local','正在移交给本地']:['Handing off to worktree','正在移交给工作树']:job.status==='error'?['Handoff failed','移交失败']:['View handoff','查看移交']));strip.onclick=()=>codexHandoffProgress(job);}
}
const codexHandoffBaseModal=openCustomModal;
openCustomModal=function(...args){codexHandoffUi.dialog=null;$('modal-backdrop').classList.remove('codex-handoff-modal');const card=$('modal-body').closest('.modal-card');if(card.dataset.handoffAria){for(const key of ['role','aria-modal','aria-label'])card.removeAttribute(key);delete card.dataset.handoffAria;}return codexHandoffBaseModal(...args);};
const codexHandoffBaseClose=closeModal;
closeModal=function(...args){codexHandoffUi.dialog=null;$('modal-backdrop').classList.remove('codex-handoff-modal');return codexHandoffBaseClose(...args);};
window.addEventListener('keydown',event=>{
  if(!codexHandoffUi.dialog||!$('modal-backdrop').classList.contains('open'))return;
  if(event.key==='Escape'){event.preventDefault();event.stopImmediatePropagation();closeModal();$('remote-location')?.focus();}
  if(event.key==='Tab'){
    const controls=Array.from($('modal-backdrop').querySelectorAll('button:not(:disabled),input:not(:disabled),select:not(:disabled),summary')).filter(node=>node.getClientRects().length);
    const index=controls.indexOf(document.activeElement);
    if(index<0||event.shiftKey&&index===0||!event.shiftKey&&index===controls.length-1){event.preventDefault();controls[event.shiftKey?controls.length-1:0]?.focus();}
  }
},true);
function codexHandoffLocation(){
  if(desktop.mode!=='codex'||!codexChrome.mounted||remoteContext()||!currentCwd)return;
  const cwd=currentCwd,context={host:'local',call:rpcLocalCall};let entry=codexHandoffUi.locations.get(cwd);
  const paint=()=>{if(desktop.mode!=='codex'||remoteContext()||currentCwd!==cwd||!entry.info?.isWorktree)return;const location=$('remote-location');location.innerHTML=codexIcon('computer');location.append(document.createTextNode(codexHandoffText('Worktree','工作树')));location.title=cwd;};
  if(entry?.info)paint();
  if(entry?.promise||entry&&Date.now()-entry.time<5000)return;
  if(!entry){entry={};codexHandoffUi.locations.set(cwd,entry);}
  entry.promise=codexForkWorkspace({cwd},context).then(info=>{entry.info=info;entry.time=Date.now();paint();}).catch(()=>{}).finally(()=>{entry.promise=null;});
}
const codexHandoffBaseSync=codexSyncChrome;
codexSyncChrome=function(...args){const result=codexHandoffBaseSync(...args);codexHandoffLocation();return result;};
const codexHandoffBaseSubmit=updateSubmitButton;
updateSubmitButton=function(...args){const result=codexHandoffBaseSubmit(...args);if(codexHandoffBusy(codexHandoffCurrentJob())){$('submit-btn').disabled=true;$('submit-btn').title=codexHandoffText('Cannot send messages while this chat is being handed off','聊天交接期间无法发送消息');}return result;};
const codexHandoffBaseSend=sendTurn;
sendTurn=async function(...args){if(codexHandoffBusy(codexHandoffCurrentJob()))throw new Error(codexHandoffText('Cannot send messages while this chat is being handed off','聊天交接期间无法发送消息'));return codexHandoffBaseSend(...args);};
const codexHandoffBaseSubmitAction=handleSubmitOrStop;
handleSubmitOrStop=function(...args){if(codexHandoffBusy(codexHandoffCurrentJob())){toast(codexHandoffText('Cannot send messages while this chat is being handed off','聊天交接期间无法发送消息'));return;}return codexHandoffBaseSubmitAction(...args);};
for(const name of ['openThread','newThread','switchMode']){
  const original=({openThread,newThread,switchMode})[name],wrapped=async function(...args){const result=await original(...args);codexHandoffPaint();return result;};
  if(name==='openThread')openThread=wrapped;else if(name==='newThread')newThread=wrapped;else switchMode=wrapped;
}
const codexHandoffBaseLocation=remoteLocationMenu;
remoteLocationMenu=async function(event){
  const id=currentThread,cwd=currentCwd,context=id?codexThreadContext(id):null;await codexHandoffBaseLocation(event);
  if(!id||currentThread!==id||codexHandoffHost()!==context.host||desktop.mode!=='codex'||activeFloatingMenuKey!=='remote-location'||!$('floating-menu').classList.contains('open'))return;
  const menu=$('floating-menu'),button=menuButton(codexHandoffText('Checking for ability to hand off…','正在检查能否移交…'),()=>{if(currentThread===id&&codexHandoffHost()===context.host)return codexHandoffDialog({id,cwd});});button.disabled=true;menu.append(button);
  try{const inspection=await context.call('workspace/handoff/inspect',{cwd},15000);if(currentThread!==id||codexHandoffHost()!==context.host||!menu.contains(button)||activeFloatingMenuKey!=='remote-location')return;
    const local=codexHandoffDirection(inspection)==='toLocal';button.textContent=codexHandoffText(local?'Move to primary checkout':'Move to worktree',local?'移动到主检出':'移动到工作树');button.disabled=false;
  }catch(error){if(menu.contains(button))button.title=codexHandoffError(error);}
};
const codexHandoffBaseNotification=handleServerNotification;
handleServerNotification=function(method,params){
  codexHandoffBaseNotification(method,params);
  if(method==='thread/handoff/progress'){
    const host=params.connectionId?remoteUi.byConnection.get(params.connectionId)?.profileId:'local';if(!host)return;
    const key=codexHandoffKey(host,params.threadId);let job=codexHandoffUi.jobs.get(key);
    if(job&&job.operationId!==params.operationId)return;
    if(!job){if(params.stage==='error')return;job={thread:{id:params.threadId,cwd:currentThread===params.threadId?currentCwd:''},context:codexThreadContext(params.threadId),params:{direction:params.direction||'toWorktree'},operationId:params.operationId,intent:codexForkIntent(),autoOpen:params.stage!=='ready',status:'running',passive:true};codexHandoffUi.jobs.set(key,job);}
    job.stage=params.stage;
    if(params.stage==='ready'){job.committedTarget=params.targetThreadId;if(job.passive||job.status==='unknown')codexHandoffCheck(job).then(done=>{if(!done&&job.committedTarget)return job.context.call('thread/read',{threadId:job.committedTarget}).then(result=>codexHandoffFinish(job,{thread:result.thread,handoff:{warnings:[]}}));}).catch(showError);}
    else if(params.stage==='error'&&!job.finished){job.status='error';job.error=codexHandoffError(params.message||codexHandoffText('The handoff failed.','移交未成功。'));}
    codexHandoffPaint();if(codexHandoffUi.dialog?.job===job)codexHandoffProgress(job);
  }
  const dialog=codexHandoffUi.dialog;
  if(dialog?.data?.thread.id===params.threadId&&['thread/status/changed','turn/started','turn/completed','thread/queue/updated'].includes(method)){
    if(method==='thread/queue/updated'){dialog.data.queuedCount=params.count;dialog.data.blockedReason=params.count?'queuedFollowUps':null;}
    else dialog.data.thread.status={type:method==='turn/started'||params.status?.type==='active'?'active':'idle'};
    dialog.validate?.();
  }
};
