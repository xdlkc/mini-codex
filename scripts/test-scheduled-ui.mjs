#!/usr/bin/env node
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source=readFileSync(new URL('../src/gui_scheduled.js',import.meta.url),'utf8');
const plain=value=>JSON.parse(JSON.stringify(value));
function deferred(){let resolve;const promise=new Promise(yes=>resolve=yes);return {promise,resolve};}
const decode=text=>text.replace(/&(amp|lt|gt|quot|#39);/g,(_,key)=>({amp:'&',lt:'<',gt:'>',quot:'"','#39':"'"}[key]));
function attrs(text){return Object.fromEntries(Array.from(text.matchAll(/([\w-]+)(?:="([^"]*)")?/g),m=>[m[1],decode(m[2]||'') ]));}

function harness(respond=()=>({})){
  const nodes=new Map(),elements=[],calls=[],opened=[],errors=[],ready=deferred();
  function options(node,markup){const choices=Array.from(markup.matchAll(/<option\b([^>]*)>([^<]*)<\/option>/g),m=>({attributes:attrs(m[1]),label:m[2]}));node.value=(choices.find(o=>Object.hasOwn(o.attributes,'selected'))||choices[0])?.attributes.value||'';}
  function parse(markup){
    for(const match of markup.matchAll(/<(input|select|textarea|form|button|p|label|div|h1|aside)\b([^>]*)>/g)){
      const attributes=attrs(match[2]),node=make(match[1]);node.attributes=attributes;
      if(attributes.id)nodes.set(attributes.id,node);node.value=attributes.value||'';node.checked=Object.hasOwn(attributes,'checked');
      const rest=markup.slice(match.index+match[0].length);
      if(match[1]==='select')options(node,rest.slice(0,rest.indexOf('</select>')));
      if(match[1]==='textarea')node.value=decode(rest.slice(0,rest.indexOf('</textarea>')));
      node.dataset=Object.fromEntries(Object.entries(attributes).filter(([k])=>k.startsWith('data-')).map(([k,v])=>[k.slice(5).replace(/-([a-z])/g,(_,c)=>c.toUpperCase()),v]));
    }
  }
  function select(selector){const name=selector.match(/\[([^\]:]+)\]/)?.[1];return name?elements.filter(n=>Object.hasOwn(n.attributes,name)&&(!selector.includes(':checked')||n.checked)):[];}
  function make(tag='div'){
    let markup='';const classes=new Set();
    const node={tag,attributes:{},dataset:{},value:'',hidden:false,children:[],style:{},textContent:'',
      classList:{add:x=>classes.add(x),remove:x=>classes.delete(x)},
      setAttribute(k,v){this.attributes[k]=v;},append(...items){this.children.push(...items);},prepend(...items){this.children.unshift(...items);},replaceChildren(...items){this.children=items;},
      querySelectorAll:select,focus(){},addEventListener(){},
    };
    Object.defineProperty(node,'innerHTML',{get:()=>markup,set:value=>{markup=value;if(tag==='select')options(node,value);else parse(value);}});
    elements.push(node);return node;
  }
  const node=id=>{if(!nodes.has(id))nodes.set(id,make());return nodes.get(id);};
  const desktop={mode:'codex',navigation:0,projectId:null,threads:[{id:'chat-A',name:'Existing chat'}],state:{projects:[{id:'project-A',name:'Alpha',path:'/alpha'}]},drafts:new Map()};
  const context=vm.createContext({
    console,Date,Intl,setTimeout,clearTimeout,setInterval(){},desktop,desktopReady:ready.promise,$:node,
    isTurnActive:false,desktopAttachments:[],mentionedFiles:[],renderComposerChips(){},handleSubmitOrStop(){},retryLastTurn:async()=>{},
    currentThread:null,currentCwd:'/workspace',currentModel:'model-A',currentEffort:'high',EFFORT_LABELS:{low:'Low',high:'High',ultra:'Ultra'},
    modelCatalog:[{model:'model-A',displayName:'Model A',supportedReasoningEfforts:[{reasoningEffort:'high',label:'High'}]}],
    escapeHtml:value=>String(value||'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])),
    baseName:path=>path.split('/').at(-1),titleCase:value=>value[0].toUpperCase()+value.slice(1),
    document:{body:make(),createElement:make,querySelector:()=>node('sidebar-nav'),querySelectorAll:select},
    rpcCall(method,params={}){calls.push({method,params});return Promise.resolve(respond(method,params));},
    openCustomModal(_title,markup){parse(markup);},closeModal(){},flushDocument:async()=>{},saveDraft(){},autoResizeInput(){},updateSubmitButton(){},
    closeSettingsView(){},hideLibrary(){},closeFloatingMenu(){},closeDrawer(){},toast(){},showError:error=>errors.push(error),
    openThread:async id=>{opened.push(id);context.currentThread=id;desktop.navigation++;},
    newThread:async()=>{context.currentThread=null;desktop.navigation++;},switchMode:async()=>{},showLibrary:async()=>{},openSettingsView:async()=>{},openProject:async()=>{},openDocument:async()=>{},handleServerNotification(){},sendTurn:async()=>calls.push({method:'ordinary-send'}),
  });
  context.window=context;
  vm.runInContext(source,context,{filename:'gui_scheduled.js'});const installed=vm.runInContext('scheduledUiReady',context),state=vm.runInContext('scheduledUi',context);
  return {context,state,desktop,node,calls,opened,errors,start:async()=>{ready.resolve();await installed;}};
}

test('production recurrence builder supports intervals/weekdays and rejects unsupported end dates',()=>{
  const h=harness(),values={timezone:'Asia/Shanghai',date:'2026-10-19',time:'09:30',interval:'3',weekday:'FR',monthday:'15'};
  for(const [frequency,rule] of [['minutes','FREQ=MINUTELY;INTERVAL=3'],['hours','FREQ=HOURLY;INTERVAL=3'],['daily','FREQ=DAILY'],['weekdays','FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR'],['weekly','FREQ=WEEKLY;BYDAY=FR'],['monthly','FREQ=MONTHLY;BYMONTHDAY=15']]){
    assert.deepEqual(plain(h.context.buildScheduledSchedule({...values,frequency})),{timezone:'Asia/Shanghai',dtstart:'DTSTART;TZID=Asia/Shanghai:20261019T093000',rrule:rule});
  }
  assert.throws(()=>h.context.buildScheduledSchedule({...values,frequency:'custom',rrule:'FREQ=DAILY;UNTIL=20261101T000000Z'}),/end date/);
  assert.throws(()=>h.context.buildScheduledSchedule({...values,frequency:'minutes',interval:'0'}),/positive/);
  assert.equal(h.context.goalBudget(''),null);assert.equal(h.context.goalBudget('25000'),25000);assert.throws(()=>h.context.goalBudget('-1'),/positive/);
});

test('actual Scheduled form submits cron/heartbeat contracts with correct archive and notification rules',async()=>{
  for(const existing of [false,true]){
    const h=harness();await h.context.openScheduledEditor();
    h.node('schedule-name').value='Review changes';h.node('schedule-prompt').value='Inspect changes and report issues';
    h.node('schedule-date').value='2026-10-19';h.node('schedule-time').value='09:30';h.node('schedule-timezone').value='Asia/Shanghai';
    h.node('schedule-environment').value='worktree';h.node('schedule-archive').checked=true;
    if(existing){h.node('schedule-destination').value='existing';h.node('schedule-thread').value='chat-A';h.node('schedule-destination').onchange();}
    else h.node('schedule-notification').value='failed_runs_only';
    // Changing recurrence must preserve an explicit notification preference.
    h.node('schedule-frequency').value='weekdays';h.node('schedule-frequency').onchange();
    await h.node('scheduled-editor').onsubmit({preventDefault(){}});
    assert.equal(h.node('schedule-form-error').textContent,'');
    const call=h.calls.find(c=>c.method==='scheduler/create');assert.ok(call,'form must call the real scheduler RPC');
    assert.equal(call.params.kind,existing?'heartbeat':'cron');assert.equal(call.params.targetThreadId,existing?'chat-A':null);
    assert.equal(call.params.autoArchive,!existing);assert.equal(call.params.notificationPolicy,existing?null:'failed_runs_only');
    assert.equal(call.params.schedule.rrule,'FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR');
    assert.equal(call.params.executionEnvironment,existing?'local':'worktree');if(existing){assert.equal(h.node('schedule-environment').disabled,true);assert.equal(h.node('schedule-cwd').disabled,true);}assert.equal(call.params.model,'model-A');assert.equal(call.params.reasoningEffort,'high');
  }
});

test('choosing Goal in the shared form starts an independent goal and creates no schedule',async()=>{
  const h=harness(method=>method==='thread/goal/start'?{goal:{objective:'Ship the feature'},thread:{id:'goal-chat'}}:{});
  await h.context.openScheduledEditor(null,'goal');h.node('schedule-prompt').value='Ship the feature';h.node('schedule-goal-budget').value='9000';
  await h.node('scheduled-editor').onsubmit({preventDefault(){}});
  assert.deepEqual(plain(h.calls.find(c=>c.method==='thread/goal/start').params),{objective:'Ship the feature',mode:'codex',cwd:'/workspace',model:'model-A',tokenBudget:9000});
  assert.equal(h.calls.some(c=>c.method.startsWith('scheduler/')),false);assert.deepEqual(h.opened,['goal-chat']);
});

test('an unfinished goal requires replacement confirmation before its start RPC',async()=>{
  const h=harness(method=>method==='thread/goal/get'?{goal:{objective:'Existing objective',status:'paused'}}:{thread:{id:'chat-A'}});
  h.context.currentThread='chat-A';let confirms=0;h.context.scheduledConfirm=async()=>{confirms++;return false;};
  assert.equal(await h.context.startDesktopGoal('Replacement'),null);assert.equal(confirms,1);assert.equal(h.calls.some(c=>c.method==='thread/goal/start'),false);
  h.context.scheduledConfirm=async()=>true;await h.context.startDesktopGoal('Replacement');
  assert.equal(h.calls.find(c=>c.method==='thread/goal/start').params.threadId,'chat-A');assert.equal(h.calls.find(c=>c.method==='thread/goal/start').params.replace,true);
});

test('Goal editor Revert restores full text and Save edits without resetting goal accounting',async()=>{
  const h=harness();
  const goal={threadId:'chat-A',objective:'Original objective with full details',status:'paused',tokensUsed:1250,timeUsedSeconds:90,tokenBudget:5000};
  await h.context.openGoalEditor(goal);
  h.node('goal-objective').value='Unsaved change';h.node('goal-budget').value='2000';
  await h.node('goal-editor-actions').children[0].onclick();
  assert.equal(h.node('goal-objective').value,goal.objective);assert.equal(h.node('goal-budget').value,5000);
  h.node('goal-objective').value='Revised objective';h.node('goal-budget').value='';
  await h.node('goal-editor-actions').children[1].onclick();
  assert.deepEqual(plain(h.calls.find(c=>c.method==='thread/goal/set').params),{threadId:'chat-A',objective:'Revised objective',tokenBudget:null,edit:true});
  assert.equal(h.node('goal-editor').hidden,true);
});

test('a goal started before navigation does not replace the new chat or its draft',async()=>{
  const started=deferred(),h=harness(method=>method==='thread/goal/start'?started.promise:{});
  h.node('composer-input').value='/goal Ship the feature';
  const pending=h.context.startDesktopGoal('Ship the feature',{clearInput:'/goal Ship the feature'});
  h.desktop.navigation++;h.context.currentThread='chat-B';h.node('composer-input').value='Draft in B';
  started.resolve({thread:{id:'goal-chat'},goal:{objective:'Ship the feature'}});await pending;
  assert.equal(h.context.currentThread,'chat-B');assert.equal(h.node('composer-input').value,'Draft in B');assert.deepEqual(h.opened,[]);
});

test('out-of-order goal reads and scheduler filters keep the newest requested state',async()=>{
  const a=deferred(),b=deferred();const h=harness((method,params)=>method==='thread/goal/get'?(params.threadId==='A'?a.promise:b.promise):{});
  h.context.renderGoalProgress=()=>{};h.context.currentThread='A';const first=h.context.refreshCurrentGoal();h.context.currentThread='B';const second=h.context.refreshCurrentGoal();
  b.resolve({goal:{threadId:'B',objective:'B'}});await second;a.resolve({goal:{threadId:'A',objective:'A'}});await first;assert.equal(h.state.goal.threadId,'B');
  const active=deferred(),paused=deferred();h.context.rpcCall=(method,params={})=>method==='scheduler/runs'?Promise.resolve({data:[]}):params.state==='active'?active.promise:paused.promise;
  h.context.renderScheduledRows=()=>{};h.state.open=true;h.state.filter='active';const old=h.context.refreshScheduledView();h.state.filter='paused';const latest=h.context.refreshScheduledView();
  paused.resolve({data:[{id:'paused-task'}]});await latest;active.resolve({data:[{id:'active-task'}]});await old;
  assert.equal(h.state.tasks[0].id,'paused-task');
});

test('installed /goal wrapper routes commands to Goal RPCs and closes its surfaces on navigation',async()=>{
  const h=harness((method,params)=>method==='thread/goal/get'?{goal:null}:method==='thread/goal/start'?{thread:{id:'goal-chat'},goal:{objective:params.objective}}:{});
  await h.start();h.node('composer-input').value='/goal Complete the release';await h.context.sendTurn();
  assert.ok(h.calls.some(c=>c.method==='thread/goal/start'));assert.equal(h.calls.some(c=>c.method==='ordinary-send'||c.method.startsWith('scheduler/')),false);
  h.node('composer-input').value='Ordinary message';await h.context.sendTurn();assert.ok(h.calls.some(c=>c.method==='ordinary-send'));
  h.state.open=true;h.node('scheduled-view').hidden=false;h.node('goal-editor').hidden=false;
  await h.context.openThread('another-chat');assert.equal(h.state.open,false);assert.equal(h.node('scheduled-view').hidden,true);assert.equal(h.node('goal-editor').hidden,true);
  assert.deepEqual(h.errors,[]);
});

test('a follow-up while pursuing a goal queues to the same chat without interrupting it',async()=>{
  const h=harness();h.context.currentThread='chat-A';h.context.isTurnActive=true;h.state.goal={threadId:'chat-A',status:'active',objective:'Finish the project'};
  h.node('composer-input').value='Also preserve the public API';await h.context.handleSubmitOrStop();
  const request=h.calls.find(call=>call.method==='turn/queue');assert.ok(request);assert.equal(request.params.threadId,'chat-A');assert.equal(request.params.input[0].text,'Also preserve the public API');assert.equal(h.calls.some(call=>call.method==='turn/interrupt'),false);assert.equal(h.node('composer-input').value,'');
});

test('editing an exhausted schedule reactivates its new calendar instead of sending invalid completed state',async()=>{
  const h=harness();await h.context.openScheduledEditor({id:'finished',name:'Old task',prompt:'Review changes',state:'completed',schedule:{dtstart:'DTSTART;TZID=Asia/Shanghai:20261019T093000',timezone:'Asia/Shanghai',rrule:'FREQ=DAILY;COUNT=1'}});
  h.node('schedule-rule').value='FREQ=DAILY';await h.node('scheduled-editor').onsubmit({preventDefault(){}});
  const request=h.calls.find(call=>call.method==='scheduler/update');assert.ok(request);assert.equal(request.params.state,'active');assert.equal(request.params.schedule.rrule,'FREQ=DAILY');
});
