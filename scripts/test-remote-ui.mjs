import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFileSync} from 'node:fs';
const source=readFileSync(new URL('../src/gui_remote.js',import.meta.url),'utf8');
const plain=value=>JSON.parse(JSON.stringify(value));
function deferred(){let resolve,reject;const promise=new Promise((a,b)=>{resolve=a;reject=b;});return {promise,resolve,reject};}
function harness(respond){
  const calls=[],sent=[],notifications=[],modals=[],errors=[],nodes=new Map();
  function element(){const classes=new Set();return {children:[],dataset:{},value:'',hidden:false,disabled:false,textContent:'',classList:{add:x=>classes.add(x),remove:x=>classes.delete(x),contains:x=>classes.has(x),toggle(x,on){on?classes.add(x):classes.delete(x);}},append(...children){this.children.push(...children);},replaceChildren(...children){this.children=[...children];},focus(){},querySelector(){return null;},querySelectorAll(){return [];}};}
  const node=id=>{if(!nodes.has(id))nodes.set(id,element());return nodes.get(id);};
  const c=vm.createContext({console,Map,Set,Date,JSON,Promise,Error,Array,Object,Number,setTimeout(){return 1;},clearTimeout(){},
    desktop:{mode:'codex',navigation:0,projectId:null,approvals:new Map(),threads:[],tabs:[],drafts:new Map(),state:{settings:{preferences:{}}}},currentThread:null,currentTurn:null,currentCwd:'/local',currentModel:'local-model',currentEffort:'medium',currentSandbox:'workspace-write',fastMode:false,isTurnActive:false,currentSettingsSection:'remote',
    desktopReady:new Promise(()=>{}),$:node,document:{createElement:element,createTextNode:text=>({textContent:text}),querySelector(){return null;},querySelectorAll(){return [];}},
    filesUi:{drafts:new Map(),mentions:new Map(),importing:0,waiters:[]},desktopAttachments:[],mentionedFiles:[],fileDraftMaps(){},renderComposerChips(){},fileDraftKey(){return c.draftKey();},fileBase64(file){return Promise.resolve(file.data);},
    attachmentInput(file){return {type:'attachment',attachmentId:file.id,id:file.id,name:file.name,parseStatus:file.parseStatus};},async addAttachments(){},
    saveDraft(){c.desktop.drafts.set(c.draftKey(),node('composer-input').value);},restoreDraft(){node('composer-input').value=c.desktop.drafts.get(c.draftKey())||'';},async flushDocument(){},async savePreferences(patch){return c.rpcLocalCall('desktop/settings/update',{preferences:patch});},
    async rpcLocalCall(method,params={},timeout){calls.push({method,params:plain(params),timeout});if(respond){const value=respond(method,params,c);if(value!==undefined)return value;}if(method==='remote/connect'){const id='conn-'+params.id;return {connection:{id,profileId:params.id,state:'connected',createdAt:1,initialize:{cwd:'/same'}}};}
      if(method==='remote/profile/list')return {data:[]};if(method==='remote/status')return {data:[]};
      if(method==='remote/rpc'){const cid=params.connectionId,m=params.method,p=params.params;if(m==='thread/start')return {thread:{id:'thread-'+cid,mode:p.mode||'codex',cwd:'/same',model:'remote',status:{type:'idle'},turns:[],connectionId:cid}};if(m==='thread/resume')return {thread:{id:p.threadId,mode:p.threadId==='remote-chat'?'chat':p.threadId==='remote-work'?'work':'codex',cwd:'/remote-thread',model:'remote',status:{type:'idle'},turns:[],connectionId:cid},cwd:'/remote-thread'};if(m==='files/import')return {attachment:{id:'att_imported_remote',name:p.name,parseStatus:'ready',connectionId:cid}};if(m==='approval/list')return {data:[]};if(m==='model/list')return {data:[],defaultModel:'remote-model',defaultReasoningEffort:'auto'};if(m==='desktop/state/read')return {projects:[{id:'project_remote',name:'Remote',mode:'codex'}],threadMetadata:[{threadId:'remote-row',mode:'codex'}],settings:{preferences:{theme:'wrong-remote'}},documents:[{id:'wrong'}]};return {};}
      if(method==='thread/resume')return {thread:{id:params.threadId,mode:params.threadId==='local-codex'?'codex':'chat',cwd:'/local-thread',model:'local',status:{type:'idle'},turns:[]},cwd:'/local-thread'};
      if(method==='files/import')return {attachment:{id:'att_imported_local',name:params.name,parseStatus:'ready'}};if(method==='thread/list')return {data:[]};if(method==='model/list')return {data:[],defaultModel:'local-model',defaultReasoningEffort:'medium'};
      if(method==='desktop/state/read')return {projects:[{id:'project_local',name:'Local',mode:'codex'}],threadMetadata:[{threadId:'local-row',mode:'chat'}],settings:{preferences:{theme:'local'}},documents:[{id:'local-doc'}]};return {};
    },rpcCall(){},rpcSend(message){sent.push(plain(message));},handleServerNotification(method,params){notifications.push({method,params:plain(params)});},
    updateSubmitButton(){node('submit-btn').disabled=false;},setTurnStatus(value){c.isTurnActive=value;},renderPendingApprovals(){},showError(error){errors.push(error.message);},toast(){},
    openCustomModal(title,html){modals.push({title,html});node('modal-backdrop').classList.add('open');for(const id of html.matchAll(/id="([^"]+)"/g))nodes.set(id[1],element());},closeModal(){node('modal-backdrop').classList.remove('open');},
    renderSettingsSection:async()=>{},openSettingsView:async()=>{},closeSettingsView(){},hideLibrary(){},closeFloatingMenu(){},openFloatingMenuAt(){},menuButton(label,run){const e=element();e.textContent=label;e.onclick=run;return e;},writeClipboardText:async()=>{},
    async newThread(keep=true,skip=false){if(!skip){c.desktop.navigation++;c.saveDraft();}c.currentThread=null;c.restoreDraft();},
    async switchMode(mode){const ticket=++c.desktop.navigation;c.saveDraft();await c.flushDocument();if(ticket!==c.desktop.navigation)return;c.desktop.mode=mode;await c.rpcCall('desktop/settings/update',{mode});await c.newThread(false,true);await c.refreshThreads();},
    async openThread(id){const ticket=++c.desktop.navigation;c.saveDraft();await c.flushDocument();if(ticket!==c.desktop.navigation)return;const result=await c.rpcCall('thread/resume',{threadId:id});if(ticket!==c.desktop.navigation)return;c.desktop.mode=result.thread.mode;c.currentThread=id;c.updateCwdUI(result.cwd);if(typeof c.codexPermissionsRestore==='function')c.codexPermissionsRestore(result.thread);},
    async openProject(){},async refreshThreads(){return c.rpcCall('thread/list',{});},async refreshProjects(){return c.rpcCall('projects/list',{});},
    async refreshModelCatalog(){const result=await c.rpcCall('model/list');c.currentModel=result.defaultModel||c.currentModel;return result;},
    updateCwdUI(cwd){c.currentCwd=cwd;c.rpcCall('workspace/gitStatus',{cwd}).catch(()=>{});},pickNativeFolderForInput:async()=>{},toggleOpenInMenu(){},openScheduledEditor:async()=>{},
  });c.window=c;c.__onServerMessage=message=>{if(message.id!==undefined&&message.method)c.desktop.approvals.set(message.id,message);else c.handleServerNotification(message.method,message.params||{});};
  vm.runInContext(source,c);vm.runInContext(`remoteUi.local=remoteSnapshot();remoteUi.profiles=[{id:'a',name:'Host A',host:'a.invalid',port:22,remoteCwd:'/same'},{id:'b',name:'Host B',host:'b.invalid',port:22,remoteCwd:'/same'}];for(const p of remoteUi.profiles){const connection={id:'conn-'+p.id,profileId:p.id,state:'connected',createdAt:1,initialize:{cwd:'/same'}};remoteUi.connections.set(p.id,connection);remoteUi.byConnection.set(connection.id,connection);}remoteUi.active='a';`,c);
  return {c,calls,sent,notifications,modals,errors,nodes,eval:code=>vm.runInContext(code,c)};
}
test('source-bound remote threads route in the background while local accounts/preferences/documents/images stay local',async()=>{
  const h=harness();h.eval(`remoteUi.threads.set('r','a');remoteUi.threads.set('l',null);desktop.mode='chat';remoteUi.active='b';`);
  await h.c.rpcCall('turn/interrupt',{threadId:'r'});assert.equal(h.calls.at(-1).method,'remote/rpc');assert.equal(h.calls.at(-1).params.connectionId,'conn-a');
  await h.c.rpcCall('turn/start',{threadId:'l'});assert.equal(h.calls.at(-1).method,'turn/start');
  for(const method of ['account/read','images/list','desktop/settings/update','desktop/document/read']){await h.c.rpcCall(method,{threadId:'r'});assert.equal(h.calls.at(-1).method,method);}
});
test('numeric local approval ids are valid; remote composite approval responses and resolutions keep their source',async()=>{
  const h=harness((method)=>method==='approval/list'?{data:[{id:7,method:'item/commandExecution/requestApproval',params:{threadId:'local'}}]}:undefined);h.c.desktop.mode='chat';await h.c.rpcCall('approval/list');
  h.c.__onServerMessage({method:'remote/request',params:{connectionId:'conn-a',id:'remote:conn-a:7',method:'item/commandExecution/requestApproval',params:{threadId:'r'}}});
  assert.equal(h.c.desktop.approvals.get('remote:conn-a:7').params.connectionId,'conn-a');h.c.rpcSend({id:'remote:conn-a:7',result:{decision:'accept'}});await Promise.resolve();
  assert.deepEqual(h.calls.at(-1).params,{connectionId:'conn-a',requestId:'remote:conn-a:7',decision:'accept'});assert.equal(h.sent.length,0);
  h.c.__onServerMessage({method:'remote/notification',params:{connectionId:'conn-a',method:'approval/resolved',params:{requestId:'remote:conn-a:7',threadId:'r'}}});assert.equal(h.notifications.at(-1).params.requestId,'remote:conn-a:7');
});
test('closed connections fail hard without falling back to local or rerouting a source-bound file',async()=>{
  const h=harness();h.eval(`remoteUi.threads.set('r','a');remoteUi.files.set('att_remote','a');remoteUi.connections.get('a').state='failed';remoteUi.active='b';`);
  const count=h.calls.length;await assert.rejects(h.c.rpcCall('thread/read',{threadId:'r'}),/closed/);await assert.rejects(h.c.rpcCall('files/read',{id:'att_remote'}),/closed/);assert.equal(h.calls.length,count);
  await h.c.rpcCall('files/read',{id:'att_generated_local'});assert.equal(h.calls.at(-1).method,'files/read');
});
test('reconnecting a profile replaces its connection while old thread/file bindings remain on that profile',async()=>{
  const h=harness((method,p)=>method==='remote/connect'?{connection:{id:'conn-new',profileId:p.id,state:'connected',createdAt:2,initialize:{cwd:'/same'}}}:undefined);h.eval(`remoteUi.threads.set('r','a');remoteUi.files.set('att_remote','a');`);
  const old=h.c.modelContextKey();await h.c.remoteConnect('a');assert.notEqual(h.c.modelContextKey(),old);await h.c.rpcCall('thread/read',{threadId:'r'});assert.equal(h.calls.at(-1).params.connectionId,'conn-new');await h.c.rpcCall('files/read',{id:'att_remote'});assert.equal(h.calls.at(-1).params.connectionId,'conn-new');
});
test('desktop state merges remote project metadata but retains local preferences and documents',async()=>{
  const h=harness(),result=await h.c.rpcCall('desktop/state/read');assert.equal(result.settings.preferences.theme,'local');assert.deepEqual(plain(result.documents),[{id:'local-doc'}]);assert.deepEqual(plain(result.projects.map(p=>p.id)),['project_local','project_remote']);assert.equal(h.eval(`remoteUi.threads.get('remote-row')`),'a');assert.equal(h.eval(`remoteUi.threads.get('local-row')`),null);
});
test('a slow connect cannot reclaim the view after an ordinary mode switch or New chat',async()=>{
  for(const action of ['mode','new']){const pending=deferred(),h=harness(method=>method==='remote/connect'?pending.promise:undefined);h.eval(`remoteUi.active=null;`);const work=h.c.remoteSelect('b');await Promise.resolve();await Promise.resolve();
    if(action==='mode')await h.c.switchMode('chat');else await h.c.newThread();
    const expected=h.c.desktop.mode;pending.resolve({connection:{id:'late-b',profileId:'b',state:'connected',createdAt:2,initialize:{cwd:'/remote'}}});await work;assert.equal(h.c.desktop.mode,expected);assert.equal(h.eval('remoteUi.active'),null);
  }
});
test('late host probes and account reads cannot cover a newer modal or a new view',async()=>{
  for(const kind of ['probe','account']){const pending=deferred(),h=harness((method,p)=>kind==='probe'&&method==='remote/hostKey/probe'||kind==='account'&&method==='remote/rpc'&&p.method==='account/read'?pending.promise:undefined);const work=kind==='probe'?h.c.remoteInspect('a'):h.c.remoteAccount('a');h.c.openCustomModal('Newer dialog','<div id="newer"></div>');pending.resolve(kind==='probe'?{keys:['a key'],fingerprints:['SHA256:a'],host:'a.invalid',port:22}:{account:{type:'apiKey'}});await work;assert.deepEqual(h.modals.map(m=>m.title),['Newer dialog']);}
});
test('opening local Codex from Chat cannot inherit the remembered remote host',async()=>{
  const h=harness();h.c.desktop.mode='chat';h.c.currentThread='local-chat';h.eval(`remoteUi.threads.set('local-chat',null);remoteUi.threads.set('local-codex',null);`);await h.c.openThread('local-codex');assert.equal(h.c.desktop.mode,'codex');assert.equal(h.eval('remoteContext()'),null);assert.equal(h.eval('remoteUi.active'),null);
  await h.c.rpcCall('model/list');assert.equal(h.calls.at(-1).method,'model/list');await h.c.rpcCall('workspace/gitStatus',{cwd:h.c.currentCwd});assert.equal(h.calls.at(-1).method,'workspace/gitStatus');
});
test('opening a local chat keeps its saved permissions after the host defaults are restored',async()=>{
  const h=harness((method,p)=>method==='thread/resume'?{cwd:'/local-thread',thread:{id:p.threadId,mode:'codex',sandboxMode:'read-only',approvalPolicy:'on-request',approvalsReviewer:'user',turns:[]}}:undefined);
  let selected={approvalsReviewer:'guardian_subagent',approvalPolicy:'never'};
  h.c.codexPermissionRequest=()=>selected;h.c.codexPermissionsRestore=settings=>{selected={approvalsReviewer:settings.approvalsReviewer||'user',approvalPolicy:settings.approvalPolicy||'on-request'};if(settings.sandboxMode)h.c.currentSandbox=settings.sandboxMode;};
  h.eval(`remoteUi.threads.set('local-codex',null);remoteUi.local={...remoteUi.local,sandbox:'danger-full-access',approvalsReviewer:'guardian_subagent',approvalPolicy:'never'};`);
  await h.c.openThread('local-codex');assert.equal(h.c.currentSandbox,'read-only');assert.equal(selected.approvalsReviewer,'user');assert.equal(selected.approvalPolicy,'on-request');assert.equal(h.c.currentCwd,'/local-thread');
});
test('selecting a host loads new-chat permissions through that captured connection',async()=>{
  const h=harness(),contexts=[];h.c.codexPermissionsDefaultsForDraft=async context=>{contexts.push(context.key);await context.call('settings/read');};await h.c.remoteSelect('b');
  assert.deepEqual(contexts,['b@conn-b']);assert.ok(h.calls.some(call=>call.method==='remote/rpc'&&call.params.connectionId==='conn-b'&&call.params.method==='settings/read'));
});
test('remote scheduled Chat/Work threads retain remote model/settings/files routes and new local chats restore local context',async()=>{
  for(const mode of ['chat','work']){const h=harness();h.eval(`remoteUi.threads.set('remote-${mode}','a');`);await h.c.openThread('remote-'+mode);assert.equal(h.c.desktop.mode,mode);assert.equal(h.eval('remoteContext()'),'a');await h.c.rpcCall('model/list');assert.equal(h.calls.at(-1).params.connectionId,'conn-a');await h.c.rpcCall('settings/write',{model:'remote'});assert.equal(h.calls.at(-1).params.connectionId,'conn-a');await h.c.newThread();assert.equal(h.eval('remoteContext()'),null);assert.equal(h.c.currentCwd,'/local');assert.equal(h.eval('remoteUi.active'),'a');}
});
test('empty drafts and models distinguish same-path hosts, while switching Chat preserves a remote Codex choice',async()=>{
  const h=harness();h.c.currentCwd='/same';assert.equal(h.c.draftKey(),'codex@a');const first=h.c.modelContextKey();h.eval(`remoteUi.active='b';`);assert.equal(h.c.draftKey(),'codex@b');assert.notEqual(h.c.modelContextKey(),first);await h.c.switchMode('chat');assert.equal(h.eval('remoteUi.active'),'b');assert.equal(h.c.draftKey(),'chat');assert.equal(h.c.currentCwd,'/local');
});
test('FileReader imports retain the original host and draft when a user changes context mid-read',async()=>{
  const read=deferred(),h=harness();h.c.fileBase64=()=>read.promise;const work=h.c.addAttachments([{name:'original.txt',size:3,type:'text/plain'}]);h.c.desktop.navigation++;h.eval(`remoteUi.active='b';`);
  h.calls.length=0;
  read.resolve('YWJj');
  await work;const request=h.calls.find(c=>c.method==='remote/rpc'&&c.params.method==='files/import');assert.equal(request.params.connectionId,'conn-a');assert.equal(h.c.desktopAttachments.length,0);assert.equal(h.c.filesUi.drafts.get('codex@a')[0].attachmentId,'att_imported_remote');assert.equal(h.c.filesUi.drafts.get('codex@a')[0].profileId,'a');assert.deepEqual(h.errors,[]);
});
test('local imports cannot become remote while FileReader is pending',async()=>{
  const read=deferred(),h=harness();h.c.desktop.mode='chat';h.c.fileBase64=()=>read.promise;const work=h.c.addAttachments([{name:'local.txt',size:3,type:'text/plain'}]);h.c.desktop.navigation++;h.c.desktop.mode='codex';h.eval(`remoteUi.active='b';`);read.resolve('YWJj');await work;assert.equal(h.calls.find(call=>call.method==='files/import').params.name,'local.txt');assert.equal(h.calls.some(call=>call.method==='remote/rpc'&&call.params.method==='files/import'),false);assert.equal(h.c.filesUi.drafts.get('chat')[0].attachmentId,'att_imported_local');assert.deepEqual(h.errors,[]);
});
test('preferences with persisted remote file DTOs cannot overwrite ownership after restart',async()=>{
  const h=harness();h.eval(`remoteUi.files.set('att_saved','a');`);h.c.remoteBind({settings:{preferences:{attachmentDrafts:{'codex@a':[{id:'att_saved',attachmentId:'att_saved',profileId:'a',connectionId:'old-connection'}]}}}},null);assert.equal(h.eval(`remoteUi.files.get('att_saved')`),'a');await h.c.rpcCall('files/read',{id:'att_saved'});assert.equal(h.calls.at(-1).params.connectionId,'conn-a');
  h.c.remoteBind({id:'att_restored',attachmentId:'att_restored',profileId:'a',connectionId:'old-connection'},null);assert.equal(h.eval(`remoteUi.files.get('att_restored')`),'a');
});
test('cloned store IDs are marked ambiguous and cannot silently replace a local or another host owner',async()=>{
  const h=harness();h.eval(`remoteUi.threads.set('copied-session',null);remoteUi.files.set('att_copy','a');`);assert.throws(()=>h.c.remoteBind({thread:{id:'copied-session',cwd:'/remote',model:'m',turns:[]}},'a'),/collision/);assert.equal(h.eval(`remoteUi.threads.get('copied-session')`),null);const before=h.calls.length;await assert.rejects(h.c.rpcCall('turn/start',{threadId:'copied-session'}),/Ambiguous/);assert.equal(h.calls.length,before);
  assert.throws(()=>h.c.remoteBind({attachment:{id:'att_copy'}},'b'),/collision/);await assert.rejects(h.c.rpcCall('files/delete',{id:'att_copy'}),/Ambiguous/);assert.equal(h.calls.length,before);
});
test('captured RPC contexts keep their exact host and reject replaced/closed connections',async()=>{
  const h=harness(),a=h.c.captureRpcContext();h.eval(`remoteUi.active='b';`);assert.notEqual(a.key,h.c.captureRpcContext().key);await a.call('workspace/branch/switch',{cwd:'/same',branch:'from-A'});assert.equal(h.calls.at(-1).params.connectionId,'conn-a');h.eval(`remoteUi.byConnection.set('conn-a',{id:'conn-a',profileId:'a',state:'disconnected'});`);const before=h.calls.length;await assert.rejects(a.call('command/exec',{command:'must-not-run'}),/closed/);assert.equal(h.calls.length,before);
  h.c.desktop.mode='chat';const local=h.c.captureRpcContext();h.c.desktop.mode='codex';await local.call('workspace/gitStatus',{cwd:'/local'});assert.equal(h.calls.at(-1).method,'workspace/gitStatus');
});
test('MCP mutations and skill inventory follow the remote settings context, and local contexts remain local',async()=>{
  const h=harness();for(const method of ['mcp/addServer','mcp/removeServer','skills/list']){await h.c.rpcCall(method,{name:'fixture'});assert.equal(h.calls.at(-1).method,'remote/rpc');assert.equal(h.calls.at(-1).params.connectionId,'conn-a');assert.equal(h.calls.at(-1).params.method,method);}
  h.c.desktop.mode='chat';await h.c.rpcCall('mcp/addServer',{name:'local'});assert.equal(h.calls.at(-1).method,'mcp/addServer');
});
test('trust requires an explicit checkbox and a late accepted trust does not navigate or connect',async()=>{
  const trusted=deferred(),h=harness(method=>method==='remote/hostKey/probe'?{host:'a.invalid',port:22,keys:['a.invalid ssh-ed25519 KEY'],fingerprints:['SHA256:fixture']}:method==='remote/hostKey/trust'?trusted.promise:undefined);await h.c.remoteInspect('a');const box=h.nodes.get('remote-trust'),accept=box.children.at(-1),check=box.children.find(node=>node.children?.some(child=>child.type==='checkbox')).children[0];assert.equal(accept.disabled,true);check.checked=true;check.onchange();assert.equal(accept.disabled,false);const work=accept.onclick();h.c.closeModal();h.c.desktop.navigation++;trusted.resolve({trusted:true});await work;assert.equal(h.calls.some(call=>call.method==='remote/connect'),false);assert.equal(h.eval('remoteUi.active'),'a');
});
test('voice capability denies remote media before a local microphone or signaling request can start',async()=>{
  const h=harness(),count=h.calls.length;const capability=await h.c.rpcCall('voice/capabilities');assert.equal(capability.available,false);assert.match(capability.reason,/local Chat or Work/);assert.equal(h.calls.length,count);h.eval(`remoteUi.threads.set('r','a');`);await assert.rejects(h.c.rpcCall('voice/start',{threadId:'r'}),/not connected/);assert.equal(h.calls.length,count);await h.c.rpcCall('voice/end',{id:'old-local-call'});assert.equal(h.calls.at(-1).method,'voice/end');
});

test('location menus retain the clicked DOM anchor after event.currentTarget is cleared by async dispatch',async()=>{
  const waiting=deferred(),h=harness(method=>method==='remote/profile/list'?waiting.promise:undefined),anchor={isConnected:true},event={currentTarget:anchor,stopPropagation(){}};
  const opened=[];h.c.openFloatingMenuAt=(key,target)=>opened.push({key,target});
  const opening=h.c.remoteLocationMenu(event);event.currentTarget=null;waiting.resolve({data:[]});await opening;
  assert.equal(opened.length,1);assert.equal(opened[0].target,anchor);assert.equal(opened[0].key,'remote-location');
});
