// Reference: threadHeader.fork* and newChatInWorktree in 26.1013.40934.
const codexForkUi={pending:new Map(),workspaces:new Map(),submenu:null};
function codexForkText(en,zh){return typeof codexText==='function'?codexText(en,zh):en;}
function codexForkKey(host,id){return JSON.stringify([host,id]);}
function codexForkIntent(){return typeof codexProjectsUi!=='undefined'?codexProjectsUi.intent:desktop.navigation;}
function codexForkCurrent(intent){return desktop.mode==='codex'&&intent===codexForkIntent();}
function codexForkNotice(request,message){
  if(!request.notice){const note=document.createElement('div');note.className='toast';note.setAttribute('role','status');$('toast-host').append(note);request.notice=note;}
  request.notice.textContent=message;
}
async function codexForkChat(thread,destination='current'){
  const context=codexThreadContext(thread.id),key=codexForkKey(context.host,thread.id);
  if(codexForkUi.pending.has(key))return codexForkUi.pending.get(key).promise;
  const intent=codexForkIntent(),request={context,threadId:thread.id,notice:null,promise:null};
  codexForkUi.pending.set(key,request);closeFloatingMenu();
  codexForkNotice(request,destination==='newWorktree'?codexForkText('Preparing a new worktree…','正在准备新工作树…'):codexForkText('Forking chat…','正在创建聊天分支…'));
  const params={threadId:thread.id,destination,titleMode:'fork'};
  if(currentThread===thread.id){params.model=currentModel;params.reasoningEffort=currentEffort;}
  request.promise=(async()=>{
    try{
      const result=await context.call('thread/fork',params,180000),target=result.thread;
      if(!target?.id)throw new Error(codexForkText('The fork did not return a chat.','分支操作没有返回聊天。'));
      if(result.worktree)codexForkUi.workspaces.set(codexForkKey(context.host,target.cwd),{time:Date.now(),isGit:true,isWorktree:true});
      if(codexForkCurrent(intent)){
        await openThread(target.id);
        if(currentThread===target.id&&desktop.mode==='codex'){
          currentModel=target.model||currentModel;
          if(target.reasoningEffort)currentEffort=target.reasoningEffort;
          updateModelAndEffortUI();
          if(drawerOpen&&currentDrawerTab!=='document')await renderDrawerContent();
          if(typeof remoteRemember==='function')remoteRemember();
        }
      }else{
        toast(codexForkText('Chat fork created','聊天分支已创建'),{label:codexForkText('Open','打开'),run:()=>openThread(target.id)});
      }
      return result;
    }finally{codexForkUi.pending.delete(key);request.notice?.remove();}
  })();return request.promise;
}
async function codexNewChatInWorktree(thread){
  const context=codexThreadContext(thread.id),intent=codexForkIntent();
  const result=await context.call('thread/read',{threadId:thread.id});
  if(!codexForkCurrent(intent))return;
  const source=result.thread;if(!source?.cwd)throw new Error(codexForkText('This chat has no working folder.','此聊天没有工作目录。'));
  // Resume the original host first. New chat then saves its draft and keeps its
  // files and branch, without forking or copying the conversation transcript.
  if(currentThread!==thread.id){await openThread(thread.id);if(currentThread!==thread.id)return;}
  const project=source.projectId||null;
  const starting=newThread(true),owned=codexForkIntent();await starting;
  if(!codexForkCurrent(owned)||currentThread)return;
  desktop.projectId=project;updateCwdUI(source.cwd);
  if(typeof remoteRemember==='function')remoteRemember();
  if(typeof codexRecordView==='function')codexRecordView();
  $('composer-input').focus();
}
async function codexForkWorkspace(thread,context){
  const key=codexForkKey(context.host,thread.cwd),cached=codexForkUi.workspaces.get(key);
  if(cached&&Date.now()-cached.time<5000)return cached;
  const status=await context.call('workspace/gitStatus',{cwd:thread.cwd},12000);
  if(status.isGit===false){const value={time:Date.now(),isGit:false,isWorktree:false};codexForkUi.workspaces.set(key,value);return value;}
  const list=await context.call('workspace/worktree/list',{cwd:thread.cwd},12000);
  const current=(list.data||[]).find(tree=>tree.path===list.repository?.repoRoot);
  const value={time:Date.now(),isGit:true,isWorktree:Boolean(current&&!current.primary)};codexForkUi.workspaces.set(key,value);return value;
}
function codexCloseForkSubmenu(restore=false){
  const current=codexForkUi.submenu;if(!current)return;codexForkUi.submenu=null;current.panel.remove();current.trigger.setAttribute('aria-expanded','false');if(restore)current.trigger.focus();
}
function codexForkSubmenu(trigger,thread){
  if(codexForkUi.submenu?.trigger===trigger){codexCloseForkSubmenu(true);return;}
  codexCloseForkSubmenu();const context=codexThreadContext(thread.id),panel=document.createElement('div');panel.className='codex-fork-submenu';panel.setAttribute('role','menu');panel.setAttribute('aria-label',codexForkText('Fork','分叉'));
  trigger.setAttribute('aria-expanded','true');$('floating-menu').append(panel);
  const same=menuButton(codexForkText('Fork chat','创建聊天分支'),()=>codexForkChat(thread,'current'));
  const isolated=menuButton(codexForkText('Fork chat in new worktree','在新工作树中创建聊天分支'),()=>codexForkChat(thread,'newWorktree'));isolated.disabled=true;
  const reason=document.createElement('p');reason.className='codex-fork-reason';reason.setAttribute('role','status');reason.textContent=codexForkText('Checking workspace…','正在检查工作区…');
  panel.append(same,isolated,reason);for(const button of [same,isolated])button.setAttribute('role','menuitem');
  codexForkUi.submenu={panel,trigger};
  const position=()=>{
    if(!panel.isConnected||!trigger.isConnected){codexCloseForkSubmenu();return;}const r=trigger.getBoundingClientRect(),bounds=panel.getBoundingClientRect(),right=r.right+7;
    panel.style.left=Math.max(10,right+bounds.width<=innerWidth-10?right:r.left-bounds.width-7)+'px';
    panel.style.top=Math.max(10,Math.min(r.top,innerHeight-bounds.height-10))+'px';
  };codexForkUi.submenu.position=position;position();same.focus();
  panel.onkeydown=event=>{
    const options=Array.from(panel.querySelectorAll('button:not(:disabled)'));
    if(['ArrowDown','ArrowUp','Home','End'].includes(event.key)){event.preventDefault();event.stopPropagation();const index=options.indexOf(document.activeElement);options[event.key==='Home'?0:event.key==='End'?options.length-1:(index+(event.key==='ArrowDown'?1:options.length-1))%options.length]?.focus();}
  };
  codexForkWorkspace(thread,context).then(info=>{
    if(codexForkUi.submenu?.panel!==panel)return;
    same.textContent=info.isWorktree?codexForkText('Fork chat in same worktree','在同一工作树中创建聊天分支'):codexForkText('Fork chat','创建聊天分支');
    isolated.disabled=!info.isGit;reason.textContent=info.isGit?'':codexForkText('A Git repository is required to fork in a new worktree.','需要 Git 代码仓库才能在新工作树中创建分支');
    if(info.isWorktree){const fresh=menuButton(codexForkText('New chat in this worktree','在此工作树中新建聊天'),()=>codexNewChatInWorktree(thread));fresh.title=codexForkText('Starts a fresh chat using the same files and branch','使用相同的文件和分支开始全新聊天');fresh.setAttribute('role','menuitem');panel.append(fresh);}position();
  }).catch(error=>{if(codexForkUi.submenu?.panel===panel)reason.textContent=error.message;});
}
const codexForkBaseThreadMenu=threadMenu;
threadMenu=function(event,thread){
  codexForkBaseThreadMenu(event,thread);if(desktop.mode!=='codex')return;
  const button=Array.from($('floating-menu').querySelectorAll('button')).find(button=>button.dataset?.threadAction==='fork'||button.textContent==='Open a copy');if(!button)return;
  button.textContent=codexForkText('Fork','分叉');if(typeof CHEV_SVG==='string')button.insertAdjacentHTML('beforeend',CHEV_SVG);button.setAttribute('role','menuitem');button.setAttribute('aria-haspopup','menu');button.setAttribute('aria-expanded','false');
  button.onclick=event=>{event.stopPropagation();codexForkSubmenu(button,thread);};button.onkeydown=event=>{if(event.key==='ArrowRight'){event.preventDefault();event.stopPropagation();codexForkSubmenu(button,thread);}};
};
const codexForkBaseClose=closeFloatingMenu;
closeFloatingMenu=function(...args){const anchor=codexForkUi.submenu?.trigger;codexCloseForkSubmenu();const result=codexForkBaseClose(...args);if(anchor&&document.activeElement===document.body)$('composer-input')?.focus();return result;};
window.addEventListener('keydown',event=>{
  const current=codexForkUi.submenu;if(current&&current.panel.contains(document.activeElement)&&['ArrowLeft','Escape'].includes(event.key)){event.preventDefault();event.stopImmediatePropagation();codexCloseForkSubmenu(true);}
},true);
window.addEventListener('resize',()=>codexForkUi.submenu?.position?.());
$('floating-menu').addEventListener('scroll',()=>codexForkUi.submenu?.position?.(),{passive:true});
const codexForkBaseNotification=handleServerNotification;
handleServerNotification=function(method,params){
  codexForkBaseNotification(method,params);if(method!=='thread/fork/progress')return;
  const host=params.connectionId?remoteUi.byConnection.get(params.connectionId)?.profileId:'local';
  const request=codexForkUi.pending.get(codexForkKey(host,params.threadId));if(!request)return;
  const labels={creatingWorktree:codexForkText('Preparing a new worktree…','正在准备新工作树…'),copyingHistory:codexForkText('Copying chat history…','正在复制聊天历史…')};if(labels[params.stage])codexForkNotice(request,labels[params.stage]);
};
