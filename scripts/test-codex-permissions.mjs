import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
const source=readFileSync(new URL('../src/gui_codex_permissions.js',import.meta.url),'utf8');
const plain=value=>JSON.parse(JSON.stringify(value));
function harness({confirm=true,wait,readWait}={}){
  const calls=[],painted=[],items=new Map(),nodes=new Map();let context;
  const element=()=>({children:[],dataset:{},open:false,textContent:'',className:'',append(...nodes){this.children.push(...nodes);},replaceChildren(){this.children=[];},querySelector(){return this.children.find(child=>child.className==='codex-approval-review')||null;}});
  context=vm.createContext({Promise,Map,JSON,Error,desktop:{mode:'codex',navigation:1},currentThread:'source',currentSandbox:'workspace-write',host:'alpha',remoteContext:()=>context.host,
    captureRpcContext:()=>{const host=context.host;return {call:async(method,params={})=>{calls.push({host,method,params:plain(params)});if(method==='settings/read'){if(readWait)await readWait;return {sandboxMode:'workspace-write',config:{approvals_reviewer:'guardian_subagent',approval_policy:'on-request'}};}}};},
    codexThreadContext:()=>{const host=context.host;return {call:async(method,params)=>{calls.push({host,method,params:plain(params)});if(method==='thread/settings/update'&&wait)await wait;}};},
    codexText:en=>en,closeFloatingMenu(){},scheduledConfirm:async()=>confirm,updateSandboxUI(){},toggleSandboxPicker(){},upsertTimelineItem(){},handleServerNotification(){},toast:(message,action)=>painted.push({message,action}),openThread:async id=>calls.push({method:'openThread',id}),
    newThread:async()=>{context.desktop.navigation++;context.currentThread=null;},
    itemEls:items,document:{createElement:element},$:id=>{if(!nodes.has(id))nodes.set(id,{textContent:'',setAttribute(){},focus(){context.document.activeElement=this;}});return nodes.get(id);},remoteUi:{byConnection:new Map([['conn-a',{profileId:'alpha'}],['conn-b',{profileId:'beta'}]])},
  });vm.runInContext(source,context);return {context,calls,painted,items,element,read:expr=>vm.runInContext(expr,context)};
}
test('Approve for me keeps the sandbox and writes the exact reviewer on its issuing host',async()=>{
  const h=harness();await h.context.codexPermissionSelect('guardian-approvals');
  assert.equal(h.context.currentSandbox,'workspace-write');assert.equal(h.context.codexPermissionLabel(),'Approve for me');
  assert.deepEqual(h.calls.map(call=>call.method),['thread/settings/update','settings/write']);
  assert.equal(h.calls[0].params.approvalsReviewer,'guardian_subagent');assert.equal(h.calls[0].params.approvalPolicy,'on-request');assert.equal(h.calls[0].params.sandboxPolicy.type,'workspaceWrite');
  assert.ok(h.calls.every(call=>call.host==='alpha'));
});
test('read-only remains read-only in reviewer mode, and aliases restore the same selection',async()=>{
  const h=harness();h.context.currentSandbox='read-only';await h.context.codexPermissionSelect('guardian-approvals');assert.equal(h.calls[0].params.sandboxPolicy.type,'readOnly');
  h.context.codexPermissionsRestore({approvalsReviewer:'auto_review',approvalPolicy:'on-request',sandboxMode:'read-only'});assert.equal(h.context.codexPermissionLabel(),'Approve for me');assert.equal(h.context.currentSandbox,'read-only');
});
test('full access needs its product confirmation; cancel makes no permission changes',async()=>{
  const h=harness({confirm:false});await h.context.codexPermissionSelect('full-access');assert.equal(h.calls.length,0);assert.equal(h.context.currentSandbox,'workspace-write');
});
test('custom policy combinations do not pretend that commands are being reviewed',()=>{
  const h=harness();h.context.codexPermissionsRestore({sandboxMode:'workspace-write',approvalPolicy:'never',approvalsReviewer:'guardian_subagent'});assert.equal(h.context.codexPermissionLabel(),'Custom');
  h.context.codexPermissionsRestore({sandboxMode:'danger-full-access',approvalPolicy:'on-request',approvalsReviewer:'user'});assert.equal(h.context.codexPermissionLabel(),'Custom');
});
test('removing approvals from an already unrestricted sandbox still requires the full-access confirmation',async()=>{
  const h=harness({confirm:false});h.context.codexPermissionsRestore({sandboxMode:'danger-full-access',approvalPolicy:'on-request',approvalsReviewer:'user'});await h.context.codexPermissionSelect('full-access');assert.equal(h.calls.length,0);assert.equal(h.context.codexPermissionLabel(),'Custom');
});
test('full access and returning to Ask for approval set both reviewer and policy',async()=>{
  const h=harness();await h.context.codexPermissionSelect('full-access');assert.equal(h.context.currentSandbox,'danger-full-access');assert.equal(h.calls[0].params.approvalPolicy,'never');
  await h.context.codexPermissionSelect('auto');assert.equal(h.context.currentSandbox,'workspace-write');assert.equal(h.context.codexPermissionLabel(),'Ask for approval');assert.equal(h.calls[2].params.approvalsReviewer,'user');
});
test('navigation during a permission write cannot redirect the saved preference or rewrite the new composer',async()=>{
  let release;const waiting=new Promise(resolve=>release=resolve);const h=harness({wait:waiting});const pending=h.context.codexPermissionSelect('guardian-approvals');await Promise.resolve();
  h.context.host='beta';h.context.currentThread='newer';h.context.desktop.navigation++;release();await pending;
  assert.ok(h.calls.every(call=>call.host==='alpha'));assert.equal(h.context.codexPermissionLabel(),'Ask for approval');
});
test('new-chat preferences use a captured transport while writes are queued',async()=>{
  const h=harness();h.context.currentThread=null;const pending=h.context.codexPermissionSelect('guardian-approvals');h.context.host='beta';await pending;assert.equal(h.calls[0].host,'alpha');
});
test('review events from another host never paint the currently selected tool',()=>{
  const h=harness(),item=h.element();h.items.set('tool',item);
  h.context.handleServerNotification('item/autoApprovalReview/completed',{threadId:'source',targetItemId:'tool',connectionId:'conn-b',review:{status:'denied',rationale:'Other host'}});assert.equal(item.children.length,0);
  h.context.handleServerNotification('item/autoApprovalReview/completed',{threadId:'source',targetItemId:'tool',connectionId:'conn-a',review:{status:'denied',rationale:'<img src=x onerror=alert(1)>'}});
  const details=item.children[0];assert.equal(details.dataset.status,'denied');assert.equal(details.children[1].textContent,'<img src=x onerror=alert(1)>');assert.equal(details.children[1].innerHTML,undefined);
});
test('a new chat restores the host default instead of inheriting a visited chat permission',async()=>{
  const h=harness();h.context.codexPermissionsRestore({approvalsReviewer:'user',sandboxMode:'read-only',approvalPolicy:'on-request'});await h.context.newThread();assert.equal(h.context.currentSandbox,'workspace-write');assert.equal(h.context.codexPermissionLabel(),'Approve for me');assert.equal(h.calls[0].host,'alpha');assert.equal(h.context.document.activeElement,h.context.$('composer-input'));
});
test('new-chat permission loading blocks the composer and cannot overwrite a newer chat selection',async()=>{
  let release;const waiting=new Promise(resolve=>release=resolve);const h=harness({readWait:waiting});const started=h.context.newThread();await Promise.resolve();await Promise.resolve();assert.equal(h.context.$('composer-host').inert,true);
  h.context.currentThread='newer';h.context.desktop.navigation++;h.context.codexPermissionsRestore({approvalsReviewer:'user',sandboxMode:'read-only'});release();await started;
  assert.equal(h.context.currentSandbox,'read-only');assert.equal(h.context.codexPermissionLabel(),'Ask for approval');assert.equal(h.context.$('composer-host').inert,false);
});
test('a failed default-permission read never enables an inherited full-access composer',async()=>{
  const h=harness({readWait:Promise.reject(new Error('configuration unavailable'))});h.context.currentSandbox='danger-full-access';await assert.rejects(h.context.newThread(),/configuration unavailable/);assert.equal(h.context.$('composer-host').inert,true);assert.ok(h.read('codexPermissionUi.loading.failed'));
});
test('a reconnect during default-permission loading cannot enable an unverified composer',async()=>{
  const h=harness();h.context.currentThread=null;h.context.currentSandbox='danger-full-access';h.context.captureRpcContext=()=>({key:'alpha@new'});
  await assert.rejects(h.context.codexPermissionsDefaultsForDraft({key:'alpha@old',call:async()=>({sandboxMode:'read-only',config:{approvals_reviewer:'user'}})}),/execution context changed/);
  assert.equal(h.context.$('composer-host').inert,true);
});
test('configuration warnings stay separate from assistant text and identify a background chat',async()=>{
  const h=harness();h.context.handleServerNotification('configWarning',{threadId:'source',connectionId:'conn-a',summary:'Invalid command rules'});
  assert.equal(h.painted[0].message,'Invalid command rules');assert.equal(h.calls.length,0);
  h.context.handleServerNotification('configWarning',{threadId:'other-chat',connectionId:'conn-b',summary:'Other host detail'});
  assert.equal(h.painted[1].message,'A background chat has a configuration warning.');await h.painted[1].action.run();assert.equal(h.calls[0].id,'other-chat');
});
