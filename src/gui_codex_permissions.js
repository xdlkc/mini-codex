// Codex permissions are distinct from the filesystem sandbox. Review decisions
// remain attached to their issuing chat and tool item, including background work.
let currentApprovalsReviewer='user',currentApprovalPolicy='on-request';
const codexPermissionUi={nonFullSandbox:'workspace-write',writing:Promise.resolve(),reviews:new Map(),loading:null,revision:0};
function codexPermissionsInert(){if($('composer-host'))$('composer-host').inert=Boolean(codexPermissionUi.loading)||(typeof codexHandoffCurrentJob==='function'&&codexHandoffBusy(codexHandoffCurrentJob()))||(typeof codexEnvironmentBusy==='function'&&codexEnvironmentBusy());}
function codexPermissionRequest(){return desktop.mode==='codex'?{approvalsReviewer:currentApprovalsReviewer,approvalPolicy:currentApprovalPolicy}:{};}
function codexPermissionMode(){if(currentSandbox==='danger-full-access'&&currentApprovalPolicy==='never')return 'full-access';if(currentSandbox==='danger-full-access'||currentApprovalPolicy!=='on-request')return 'custom';return currentApprovalsReviewer==='guardian_subagent'?'guardian-approvals':'auto';}
function codexPermissionLabel(){const mode=codexPermissionMode();return mode==='full-access'?codexText('Full access','完全访问'):mode==='guardian-approvals'?codexText('Approve for me','帮我批准'):mode==='custom'?codexText('Custom','自定义'):codexText('Ask for approval','请求批准');}
function codexPermissionsRestore(settings,cancelLoading=true){
  if(desktop.mode!=='codex')return;
  if(cancelLoading){codexPermissionUi.loading=null;codexPermissionsInert();}
  currentApprovalsReviewer=['guardian_subagent','auto_review'].includes(settings.approvalsReviewer)?'guardian_subagent':'user';
  currentApprovalPolicy=settings.approvalPolicy==='never'?'never':'on-request';
  if(settings.sandboxMode)currentSandbox=settings.sandboxMode;
  if(currentSandbox!=='danger-full-access')codexPermissionUi.nonFullSandbox=currentSandbox;
  updateSandboxUI();
}
async function codexPermissionsDefaultsForDraft(context=captureRpcContext(),loading={}){
  if(desktop.mode!=='codex'||currentThread)return;
  const host=remoteContext()||'local',navigation=desktop.navigation,revision=codexPermissionUi.revision;
  codexPermissionUi.loading=loading;codexPermissionsInert();const focus=document.activeElement;
  try{
    await codexPermissionUi.writing.catch(()=>{});const settings=await context.call('settings/read');
    if(codexPermissionUi.loading===loading&&revision===codexPermissionUi.revision&&navigation===desktop.navigation&&!currentThread&&host===(remoteContext()||'local')&&context.key===captureRpcContext().key){codexPermissionsRestore({sandboxMode:settings.sandboxMode,approvalPolicy:settings.config?.approval_policy||'on-request',approvalsReviewer:settings.config?.approvals_reviewer||'user'});if(document.activeElement===focus)$('composer-input').focus();}
    else if(codexPermissionUi.loading===loading&&desktop.mode==='codex'&&!currentThread)throw new Error(codexText('The execution context changed while loading permissions. Reopen the chat to reload them.','加载权限时执行环境已变化，请重新打开聊天以加载权限。'));
  }catch(error){if(codexPermissionUi.loading===loading)loading.failed=true;throw error;}
  finally{if(codexPermissionUi.loading===loading&&!loading.failed){codexPermissionUi.loading=null;codexPermissionsInert();}}
}
if(typeof newThread==='function'){
  const baseNewThread=newThread;
  newThread=async function(...args){
    if(desktop.mode!=='codex')return baseNewThread(...args);
    const loading={};codexPermissionUi.loading=loading;codexPermissionsInert();
    try{
      await baseNewThread(...args);if(codexPermissionUi.loading===loading)await codexPermissionsDefaultsForDraft(captureRpcContext(),loading);
    }catch(error){if(codexPermissionUi.loading===loading)loading.failed=true;throw error;}
    finally{if(codexPermissionUi.loading===loading&&!loading.failed){codexPermissionUi.loading=null;codexPermissionsInert();}}
  };
}
async function codexPermissionSelect(mode){
  codexPermissionUi.revision++;
  const context=currentThread?codexThreadContext(currentThread):null,transport=captureRpcContext(),id=currentThread,host=remoteContext()||'local',navigation=desktop.navigation;
  if(mode==='full-access'&&codexPermissionMode()!=='full-access'){
    closeFloatingMenu();if(!await scheduledConfirm(codexText('Allow full access?','允许完全访问？'),codexText('Codex can edit any file and access the internet without asking for approval.','Codex 可以编辑任何文件并访问互联网，无需请求批准。'),codexText('Allow full access','允许完全访问')))return;
    if(navigation!==desktop.navigation||id!==currentThread||host!==(remoteContext()||'local'))return;
  }
  const sandbox=mode==='full-access'?'danger-full-access':currentSandbox==='danger-full-access'?codexPermissionUi.nonFullSandbox:currentSandbox;
  const params={approvalsReviewer:mode==='guardian-approvals'?'guardian_subagent':'user',approvalPolicy:mode==='full-access'?'never':'on-request',sandboxMode:sandbox};
  closeFloatingMenu();
  const save=async()=>{
    if(context)await context.call('thread/settings/update',{threadId:id,...params,sandboxPolicy:{type:sandbox==='read-only'?'readOnly':sandbox==='danger-full-access'?'dangerFullAccess':'workspaceWrite'}});
    // Store the new-chat preference on the same captured host.
    await (context?.call||transport.call)('settings/write',params);
    if(navigation===desktop.navigation&&id===currentThread&&host===(remoteContext()||'local'))codexPermissionsRestore(params);
  };
  codexPermissionUi.writing=codexPermissionUi.writing.catch(()=>{}).then(save);await codexPermissionUi.writing;
}
const codexPermissionsBasePicker=toggleSandboxPicker;
toggleSandboxPicker=function(event){
  if(desktop.mode!=='codex')return codexPermissionsBasePicker(event);
  event.stopPropagation();openFloatingMenuAt('codex-permissions',event.currentTarget,'',true);
  const menu=$('floating-menu'),mode=codexPermissionMode();
  for(const [value,en,zh,description,zhDescription] of [
    ['auto','Ask for approval','请求批准','Always ask to edit external files and use the internet','编辑外部文件和使用互联网时始终询问'],
    ['guardian-approvals','Approve for me','帮我批准','Only ask for actions detected as potentially unsafe','仅对检测到的风险操作请求批准'],
    ['full-access','Full access','完全访问权限','Access the internet and any file on your computer without restrictions','可不受限制地访问互联网和你电脑上的任何文件'],
  ]){
    const button=menuButton('',()=>codexPermissionSelect(value)),text=document.createElement('span'),title=document.createElement('span'),detail=document.createElement('span');
    button.classList.add('codex-permission-option');button.setAttribute('role','menuitemradio');button.setAttribute('aria-checked',String(value===mode));
    const icon=document.createElement('span');icon.className='codex-permission-icon';icon.innerHTML=codexIcon(value==='full-access'?'permissionFull':value==='guardian-approvals'?'permissionGuardian':'permissionAsk');button.append(icon);
    title.className='menu-item-title';title.textContent=codexText(en,zh);detail.className='menu-item-desc';detail.textContent=codexText(description,zhDescription);text.className='menu-item-text';text.append(title,detail);button.append(text);
    if(value===mode){const check=document.createElement('span');check.innerHTML=CHECK_SVG;button.append(check);}menu.append(button);
  }
  if(mode==='custom'){const current=menuButton(codexText('Custom (config.toml)','自定义 (config.toml)'),()=>{});current.setAttribute('role','menuitemradio');current.setAttribute('aria-checked','true');menu.append(current);}
  codexMenuKeyboard(menu,event.currentTarget);
};
const codexPermissionsBaseSandbox=updateSandboxUI;
updateSandboxUI=function(...args){const result=codexPermissionsBaseSandbox(...args);if(desktop.mode==='codex'){$('sandbox-pill-text').textContent=codexPermissionLabel();if(typeof codexIcon==='function')$('sandbox-pill-icon').innerHTML=codexIcon(currentSandbox==='danger-full-access'?'permissionFull':currentApprovalsReviewer==='guardian_subagent'?'permissionGuardian':'permissionAsk');const label=codexText('Change permissions','更改权限');$('sandbox-pill-btn').setAttribute('aria-label',label);$('sandbox-pill-btn').title=label;}return result;};
function codexPaintReview(itemId,review){
  const item=itemEls.get(itemId);if(!item)return;
  let status=item.querySelector('.codex-approval-review');if(!status){status=document.createElement('details');status.className='codex-approval-review';item.append(status);}
  const open=status.open;status.replaceChildren();const summary=document.createElement('summary');
  const labels={inProgress:['Reviewing request','正在审查请求'],approved:['Auto-review approved','自动审查已批准'],denied:['Auto-review denied','自动审查已拒绝'],timedOut:['Auto-review timed out','自动审查超时'],aborted:['Auto-review stopped','自动审查已停止']};
  summary.textContent=codexText(...(labels[review.status]||labels.aborted));status.append(summary);if(review.rationale){const reason=document.createElement('p');reason.textContent=review.rationale;status.append(reason);}if(review.status==='denied'){const hint=document.createElement('p');hint.textContent=review.riskLevel==='high'?codexText('Requires explicit authorization because this action is considered high risk','此操作被视为高风险，需要明确授权'):codexText('Requires explicit authorization','需要明确授权');status.append(hint);}status.open=open;status.dataset.status=review.status;
}
const codexReviewBaseUpsert=upsertTimelineItem;
upsertTimelineItem=function(item){const result=codexReviewBaseUpsert(item);if(item.approvalReview)codexPaintReview(item.id,item.approvalReview);else{const review=codexPermissionUi.reviews.get(JSON.stringify([remoteContext()||'local',currentThread,item.id]));if(review)codexPaintReview(item.id,review);}return result;};
const codexReviewBaseNotification=handleServerNotification;
handleServerNotification=function(method,params){
  if(method==='configWarning'||method==='guardianWarning'){
    params={...params,summary:params.summary||params.message};
    const host=params.connectionId?remoteUi.byConnection.get(params.connectionId)?.profileId:'local';if(!host)return;
    if(params.threadId===currentThread&&host===(remoteContext()||'local'))toast(params.summary);
    else toast(codexText('A background chat has a configuration warning.','后台聊天有配置警告。'),{label:codexText('Open chat','打开聊天'),run:()=>openThread(params.threadId)});
    return;
  }
  if(method.startsWith('item/autoApprovalReview/')){const host=params.connectionId?remoteUi.byConnection.get(params.connectionId)?.profileId:'local';if(!host)return;const key=JSON.stringify([host,params.threadId,params.targetItemId]);codexPermissionUi.reviews.set(key,params.review);if(params.threadId===currentThread&&host===(remoteContext()||'local'))codexPaintReview(params.targetItemId,params.review);return;}
  return codexReviewBaseNotification(method,params);
};
function codexRenderApprovalCard(reqId,method,params){
  setViewMode(true);
  const patch=method.includes('fileChange'),files=Array.isArray(params.files)?params.files:[];
  const card=document.createElement('div');card.className='approval-card codex-approval-card';
  const intro=document.createElement('div');intro.className='codex-approval-intro';intro.setAttribute('role','alert');intro.setAttribute('aria-atomic','true');
  const identity=document.createElement('div');identity.className='codex-approval-identity';
  const icon=document.createElement('span');icon.setAttribute('aria-hidden','true');icon.innerHTML=patch?ICONS.diff:ICONS.term;
  const label=document.createElement('span');label.textContent=patch?codexText('Edit files','编辑文件'):codexText('Terminal','终端');identity.append(icon,label);
  const title=document.createElement('div');title.className='codex-approval-title';
  title.textContent=patch?codexText(`Allow ChatGPT to edit the following ${files.length===1?'file':'files'}?`,'是否允许 ChatGPT 编辑以下文件？'):
    String(params.reason||'').trim()||codexText('Allow ChatGPT to run this command?','允许 ChatGPT 运行此命令？');
  intro.append(identity,title);card.append(intro);
  const detail=document.createElement('div');detail.className='approval-cmd';detail.textContent=patch?files.join('\n'):String(params.command||'');card.append(detail);
  const actions=document.createElement('div');actions.className='approval-actions codex-approval-actions';
  const button=(text,decision,primary=false)=>{const element=document.createElement('button');element.type='button';element.className='btn-pill'+(primary?' primary':'');element.textContent=text;element.dataset.dec=decision;return element;};
  // The reference's Deny action sends decline even when the CLI lists cancel.
  // cancel is a separate protocol action that interrupts the entire turn.
  const deny=button(codexText('Deny','拒绝'),'decline'),once=button(codexText('Allow once','允许一次'),'accept',true);
  actions.append(deny);
  const choices=params.availableDecisions,allowSession=patch&&(!Array.isArray(choices)||choices.includes('acceptForSession'));
  const offered=params.proposedExecpolicyAmendment;
  const rule=!patch&&Array.isArray(offered)&&offered.length&&offered.every(token=>typeof token==='string')&&!offered.join(' ').includes('\n')&&!offered.join(' ').includes('\r')?offered.slice():null;
  if(allowSession||rule){
    const group=document.createElement('div');group.className='codex-approval-split';group.append(once);
    const options=document.createElement('details');options.className='codex-approval-options';
    const summary=document.createElement('summary');summary.setAttribute('aria-label',codexText('Approval options','审批选项'));summary.title=summary.getAttribute('aria-label');summary.textContent='⌄';
    const menu=document.createElement('div');menu.className='codex-approval-scope-menu';menu.setAttribute('role','menu');
    const single=button(codexText('Allow once','允许一次'),'accept'),session=button(rule?codexText('Allow similar commands','允许类似命令'):codexText('Allow all edits','允许所有修改'),rule?'acceptWithExecpolicyAmendment':'acceptForSession');
    single.setAttribute('role','menuitem');session.setAttribute('role','menuitem');
    const prefixText=rule?.map(token=>token&&Array.from(token).every(c=>/[a-zA-Z0-9]/.test(c)||'_+-./:=,@%'.includes(c))?token:"'"+token.replaceAll("'","'\\''")+"'").join(' ');
    session.title=rule?codexText(`Allow commands that start with ${prefixText} for this conversation`,`允许在此对话中使用以 ${prefixText} 开头的命令`):codexText('Allow this and future file edits in this conversation without asking again','允许在此对话中进行本次及后续文件编辑，无需再次询问');menu.append(single,session);options.append(summary,menu);group.append(options);actions.append(group);
    const items=[single,session];
    summary.onkeydown=event=>{if(event.key==='ArrowDown'||event.key==='ArrowUp'){event.preventDefault();event.stopPropagation();options.open=true;items[event.key==='ArrowDown'?0:1].focus();}};
    options.onkeydown=event=>{
      if(event.key==='Escape'){event.preventDefault();event.stopPropagation();options.open=false;summary.focus();return;}
      if(!['ArrowDown','ArrowUp','Home','End'].includes(event.key))return;
      event.preventDefault();const index=items.indexOf(document.activeElement);
      items[event.key==='Home'?0:event.key==='End'?1:(index+(event.key==='ArrowDown'?1:items.length-1))%items.length].focus();
    };
  }else actions.append(once);
  card.append(actions);
  let answered=false;
  card.querySelectorAll('button[data-dec]').forEach(element=>{element.onclick=()=>{if(answered)return;answered=true;const decision=element.dataset.dec==='acceptWithExecpolicyAmendment'&&rule?{acceptWithExecpolicyAmendment:{execpolicy_amendment:rule}}:element.dataset.dec;rpcSend({id:reqId,result:{decision}});card.remove();};});
  document.getElementById('messages').append(card);scrollBottom();
  if(!document.activeElement?.closest('input,textarea,[contenteditable]:not([contenteditable="false"])'))once.focus();
  return card;
}
if(typeof renderApprovalCard==='function'){
  const codexBaseApprovalCard=renderApprovalCard;
  renderApprovalCard=function(reqId,method,params){return desktop.mode==='codex'?codexRenderApprovalCard(reqId,method,params):codexBaseApprovalCard(reqId,method,params);};
}
