// Scheduled tasks and Goals are independent products backed by separate Rust RPCs.
const scheduledUi = { open:false, tab:'scheduled', filter:'all', search:'', tasks:[], runs:[], goals:[], selected:null, request:0, detail:0, goal:null, goalRequest:0, confirm:null };
const goalLabels = {active:'Pursuing goal',paused:'Paused goal',blocked:'Goal stalled',usageLimited:'Goal usage limited',budgetLimited:'Goal limited',complete:'Goal achieved'};
const scheduledEscape = value => escapeHtml(String(value ?? ''));

function scheduledButton(text, run, className='btn-pill') {
  const button=document.createElement('button');button.type='button';button.className=className;button.textContent=text;
  button.onclick=async()=>{button.disabled=true;try{await run();}catch(error){showError(error);}finally{button.disabled=false;}};return button;
}
function scheduledDate(value) {
  if(!value)return 'Not scheduled';
  const time=typeof value==='number'&&value<1e12?value*1000:value;
  const date=new Date(time);return Number.isNaN(date.getTime())?'Not scheduled':date.toLocaleString([], {month:'short',day:'numeric',hour:'2-digit',minute:'2-digit'});
}
function goalElapsed(goal) {
  let seconds=goal.timeUsedSeconds||0;
  if(goal.status==='active'&&goal.updatedAt)seconds+=Math.max(0,Date.now()/1000-goal.updatedAt);
  const minutes=Math.floor(seconds/60);return seconds<60?`${Math.floor(seconds)}s`:minutes<60?`${minutes}m`:`${Math.floor(minutes/60)}h ${minutes%60}m`;
}
function goalUsage(goal) { return `${Number(goal.tokensUsed||0).toLocaleString()}${goal.tokenBudget?` / ${Number(goal.tokenBudget).toLocaleString()}`:''} tokens · ${goalElapsed(goal)}`; }
function scheduledSummary(task) {
  const rule=(task.schedule?.rrule||'').replace(/^RRULE:/i,'');
  const frequency=rule.match(/(?:^|;)FREQ=([^;]+)/i)?.[1];
  const interval=rule.match(/(?:^|;)INTERVAL=(\d+)/i)?.[1]||'1';
  const names={MINUTELY:'minute',HOURLY:'hour',DAILY:'day',WEEKLY:'week',MONTHLY:'month',YEARLY:'year'};
  const summary=names[frequency]?`Every ${interval==='1'?'':interval+' '}${names[frequency]}${interval==='1'?'':'s'}`:rule||'Custom schedule';
  return `${summary}${/BYDAY=MO,TU,WE,TH,FR(?:;|$)/i.test(rule)?' · Weekdays':''} · ${task.schedule?.timezone||'Local time'}`;
}
function closeScheduledView() {
  scheduledUi.open=false;scheduledUi.request++;scheduledUi.detail++;
  if($('scheduled-view'))$('scheduled-view').hidden=true;
  document.body.classList.remove('in-scheduled');$('nav-scheduled')?.classList.remove('active');
  closeGoalEditor();
}
async function openScheduledView(tab='scheduled') {
  const ticket=++desktop.navigation;saveDraft();await flushDocument();if(ticket!==desktop.navigation)return;
  closeSettingsView();hideLibrary();closeFloatingMenu();closeDrawer();
  scheduledUi.open=true;scheduledUi.tab=tab;scheduledUi.filter='all';scheduledUi.selected=null;
  $('scheduled-view').hidden=false;document.body.classList.add('in-scheduled');
  $('nav-home').classList.remove('active');$('nav-library')?.classList.remove('active');$('nav-scheduled').classList.add('active');
  $('topbar-title').textContent='Scheduled';$('scheduled-detail').hidden=true;
  renderScheduledControls();await refreshScheduledView();
}
function renderScheduledControls() {
  $('scheduled-heading').textContent=scheduledUi.tab==='goals'?'Goals':'Scheduled';
  $('scheduled-create').textContent=scheduledUi.tab==='goals'?'+ New goal':'+ New task';
  $('scheduled-search').placeholder=scheduledUi.tab==='goals'?'Search goals':'Search scheduled tasks';
  document.querySelectorAll('[data-scheduled-tab]').forEach(b=>b.setAttribute('aria-selected',String(b.dataset.scheduledTab===scheduledUi.tab)));
  document.querySelectorAll('[data-scheduled-filter]').forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.scheduledFilter===scheduledUi.filter)));
}
async function refreshScheduledView() {
  if(!scheduledUi.open)return;
  const ticket=++scheduledUi.request,tab=scheduledUi.tab;
  const params={};if(scheduledUi.filter!=='all')params[tab==='goals'?'status':'state']=scheduledUi.filter;
  if(tab==='scheduled'&&scheduledUi.search.trim())params.search=scheduledUi.search.trim();
  const [result,runs]=await Promise.all([rpcCall(tab==='goals'?'thread/goal/list':'scheduler/list',params),tab==='scheduled'?rpcCall('scheduler/runs'):Promise.resolve(null)]);
  if(!scheduledUi.open||ticket!==scheduledUi.request||tab!==scheduledUi.tab)return;
  if(tab==='goals')scheduledUi.goals=result.data||[];else {scheduledUi.tasks=result.data||[];scheduledUi.runs=runs.data||[];}
  renderScheduledRows();
}
function renderScheduledRows() {
  const list=$('scheduled-list');list.replaceChildren();
  const goals=scheduledUi.tab==='goals';
  const query=scheduledUi.search.toLowerCase();
  const items=goals?scheduledUi.goals.filter(g=>g.objective.toLowerCase().includes(query)):scheduledUi.tasks.filter(t=>(t.name+' '+t.prompt).toLowerCase().includes(query));
  if(!items.length){const empty=document.createElement('div');empty.className='scheduled-empty';empty.textContent=goals?'No goals yet. Start a goal to work toward an outcome.':'No scheduled tasks. Create a task to run on a recurring schedule.';list.append(empty);return;}
  for(const item of items){
    const running=!goals&&(item.running||item.status==='running'||scheduledUi.runs.some(run=>run.taskId===item.id&&run.status==='running'));
    const row=document.createElement('div');row.className='scheduled-row';
    const main=document.createElement('button');main.className='scheduled-row-main';
    main.innerHTML=goals?`<strong>${scheduledEscape(item.objective)}</strong><span>${scheduledEscape(goalLabels[item.status]||item.status)} · ${scheduledEscape(goalUsage(item))}</span>`:
      `<strong>${scheduledEscape(item.name)}</strong><span>${scheduledEscape(scheduledSummary(item))}</span><small>${running?'Running':item.state==='paused'?'Paused':item.state==='completed'?'Schedule completed':'Next run · '+scheduledEscape(scheduledDate(item.nextRunAt??item.nextRun))}</small>`;
    main.onclick=()=>{(goals?openGoalEditor(item):openScheduledTask(item.id)).catch(showError);};row.append(main);
    const actions=document.createElement('div');actions.className='scheduled-row-actions';
    if(goals)actions.append(scheduledButton('Open chat',()=>openThread(item.threadId)));
    else {
      const run=scheduledButton('Run now',()=>runScheduledTask(item)),remove=scheduledButton('Delete',()=>deleteScheduledTask(item));run.disabled=!!running;remove.disabled=!!running;
      actions.append(run);if(item.state!=='completed')actions.append(scheduledButton(item.state==='paused'?'Resume':'Pause',()=>updateScheduledTask(item,{state:item.state==='paused'?'active':'paused'})));
      actions.append(scheduledButton('Edit',()=>openScheduledEditor(item)),remove);
    }
    row.append(actions);list.append(row);
  }
}
async function updateScheduledTask(task,patch) { await rpcCall('scheduler/update',{id:task.id,...patch});await refreshScheduledView();if(scheduledUi.selected===task.id)await openScheduledTask(task.id); }
async function runScheduledTask(task) { await rpcCall('scheduler/run',{id:task.id});await refreshScheduledView();if(scheduledUi.open)await openScheduledTask(task.id); }
async function deleteScheduledTask(task) {
  if(!await scheduledConfirm('Delete scheduled task?',`Delete “${task.name}”? Previous runs will be kept.`,'Delete'))return;
  await rpcCall('scheduler/delete',{id:task.id});if(scheduledUi.selected===task.id){scheduledUi.selected=null;$('scheduled-detail').hidden=true;}await refreshScheduledView();
}
function scheduledConfirm(title,message,label='Continue') {
  scheduledUi.confirm?.(false);
  return new Promise(resolve=>{
    scheduledUi.confirm=resolve;
    openCustomModal(title,`<p class="scheduled-help">${scheduledEscape(message)}</p><div class="scheduled-form-actions" id="scheduled-confirm-actions"></div>`);
    const finish=value=>{scheduledUi.confirm=null;closeModal();resolve(value);};
    $('scheduled-confirm-actions').append(scheduledButton('Cancel',()=>finish(false)),scheduledButton(label,()=>finish(true),label==='Delete'?'btn-pill danger':'btn-pill primary'));
  });
}
async function openScheduledTask(id) {
  const ticket=++scheduledUi.detail;scheduledUi.selected=id;
  const [result,runs]=await Promise.all([rpcCall('scheduler/read',{id}),rpcCall('scheduler/runs',{taskId:id})]);
  if(!scheduledUi.open||scheduledUi.tab!=='scheduled'||scheduledUi.selected!==id||ticket!==scheduledUi.detail)return;
  const task=result.task,detail=$('scheduled-detail');detail.hidden=false;
  detail.innerHTML=`<div class="scheduled-detail-heading"><h2>${scheduledEscape(task.name)}</h2><button class="icon-btn" id="scheduled-detail-close" aria-label="Close task details">✕</button></div><p class="scheduled-help">${scheduledEscape(scheduledSummary(task))}</p><p class="scheduled-instructions">${scheduledEscape(task.prompt)}</p><div class="scheduled-row-actions" id="scheduled-detail-actions"></div><h3>Previous runs</h3><div id="scheduled-runs"></div>`;
  $('scheduled-detail-close').onclick=()=>{scheduledUi.selected=null;scheduledUi.detail++;detail.hidden=true;};
  $('scheduled-detail-actions').append(scheduledButton('Run now',()=>runScheduledTask(task)),scheduledButton('Edit',()=>openScheduledEditor(task)));
  const history=$('scheduled-runs');
  if(!runs.data?.length){history.innerHTML='<p class="scheduled-help">No runs yet</p>';return;}
  for(const run of runs.data){
    const row=document.createElement('article');row.className='scheduled-run';
    row.innerHTML=`<div><strong>${scheduledEscape(titleCase(run.status))}${run.unread?' · Unread':''}</strong><time>${scheduledEscape(scheduledDate(run.startedAt))}</time></div><p>${scheduledEscape(run.error||run.summary||'')}</p>`;
    const actions=document.createElement('div');actions.className='scheduled-row-actions';
    const threadIds=Array.from(new Set([run.threadId,...(run.threadIds||[])].filter(Boolean)));
    threadIds.forEach((threadId,index)=>actions.append(scheduledButton(threadIds.length>1?`Open chat ${index+1}`:'Open chat',async()=>{await rpcCall('scheduler/run/update',{id:run.id,unread:false});await openThread(threadId);})));
    if(run.unread)actions.append(scheduledButton('Mark read',async()=>{await rpcCall('scheduler/run/update',{id:run.id,unread:false});await openScheduledTask(id);}));
    if(run.reviewState==='PENDING_REVIEW')actions.append(scheduledButton('Accept',async()=>{await rpcCall('scheduler/run/update',{id:run.id,reviewState:'ACCEPTED',unread:false});await openScheduledTask(id);}));
    if(run.status!=='running'&&run.reviewState!=='ARCHIVED')actions.append(scheduledButton('Archive',async()=>{await rpcCall('scheduler/run/update',{id:run.id,reviewState:'ARCHIVED',unread:false});await openScheduledTask(id);}));
    row.append(actions);history.append(row);
  }
}

function scheduledLocalParts(timezone) {
  const parts=new Intl.DateTimeFormat('en-CA',{timeZone:timezone,year:'numeric',month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit',hourCycle:'h23'}).formatToParts(new Date(Date.now()+60000));
  const p=Object.fromEntries(parts.map(part=>[part.type,part.value]));return {date:`${p.year}-${p.month}-${p.day}`,time:`${p.hour}:${p.minute}`};
}
function buildScheduledSchedule(values) {
  new Intl.DateTimeFormat('en',{timeZone:values.timezone});
  if(!/^\d{4}-\d{2}-\d{2}$/.test(values.date)||!/^\d{2}:\d{2}$/.test(values.time))throw new Error('Choose a start date and time.');
  const interval=Number(values.interval||1);if(!Number.isInteger(interval)||interval<1)throw new Error('The interval must be a positive whole number.');
  let rule;
  switch(values.frequency){
    case 'minutes':rule=`FREQ=MINUTELY;INTERVAL=${interval}`;break;
    case 'hours':rule=`FREQ=HOURLY;INTERVAL=${interval}`;break;
    case 'daily':rule='FREQ=DAILY';break;
    case 'weekdays':rule='FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR';break;
    case 'weekly':if(!['MO','TU','WE','TH','FR','SA','SU'].includes(values.weekday))throw new Error('Choose a weekday.');rule=`FREQ=WEEKLY;BYDAY=${values.weekday}`;break;
    case 'monthly':{const day=Number(values.monthday);if(!Number.isInteger(day)||day<1||day>31)throw new Error('Choose a day from 1 to 31.');rule=`FREQ=MONTHLY;BYMONTHDAY=${day}`;break;}
    case 'custom':rule=values.rrule.trim().replace(/^RRULE:/i,'').toUpperCase();if(!/(?:^|;)FREQ=(MINUTELY|HOURLY|DAILY|WEEKLY|MONTHLY|YEARLY)(?:;|$)/.test(rule)||/(?:^|;)(UNTIL|COUNT)=/.test(rule)||/[\r\n]/.test(rule))throw new Error('Use a recurring RRULE without an end date or occurrence limit.');break;
    default:throw new Error('Choose a frequency.');
  }
  return {dtstart:`DTSTART;TZID=${values.timezone}:${values.date.replaceAll('-','')}T${values.time.replace(':','')}00`,rrule:rule,timezone:values.timezone};
}
function scheduledSelectOptions(items,value) { return items.map(([id,label])=>`<option value="${scheduledEscape(id)}"${id===value?' selected':''}>${scheduledEscape(label)}</option>`).join(''); }
async function openScheduledEditor(task=null,initialType='scheduled') {
  if(typeof refreshProjects==='function')await refreshProjects();
  const timezone=task?.schedule?.timezone||Intl.DateTimeFormat().resolvedOptions().timeZone||'Asia/Shanghai';
  const local=scheduledLocalParts(timezone),start=task?.schedule?.dtstart?.match(/:(\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})/);
  if(start){local.date=`${start[1]}-${start[2]}-${start[3]}`;local.time=`${start[4]}:${start[5]}`;}
  const threads=desktop.threads.map(t=>[t.id,t.name||t.preview||t.id]);
  if(currentThread&&!threads.some(t=>t[0]===currentThread))threads.unshift([currentThread,'Current chat']);
  const models=(modelCatalog||[]).filter(m=>!m.hidden||m.model===currentModel||m.model===task?.model).map(m=>[m.model,m.displayName||m.model]);if(!models.some(m=>m[0]===currentModel))models.unshift([currentModel,currentModel]);
  const projects=(desktop.state?.projects||[]).map(p=>({cwd:p.cwd||p.path||currentCwd,projectId:p.id,name:p.name||p.label||baseName(p.path||p.cwd)}));
  for(const folder of (typeof projectsList==='undefined'?[]:projectsList)){if(folder.path&&!projects.some(p=>p.cwd===folder.path))projects.push({cwd:folder.path,projectId:null,name:folder.label||folder.name||baseName(folder.path)});}
  const checked=new Set((task?.projects||[]).map(p=>p.projectId||p.cwd));
  openCustomModal(task?'Edit scheduled task':'Create task',`<form id="scheduled-editor" class="scheduled-form">
    <label>Type<select id="schedule-type"${task?' disabled':''}>${scheduledSelectOptions([['scheduled','Scheduled task'],['goal','Goal']],initialType)}</select></label>
    <label data-schedule-only>Name<input id="schedule-name" maxlength="200" value="${scheduledEscape(task?.name||'')}" placeholder="Morning project review"></label>
    <label>Instructions<textarea id="schedule-prompt" rows="5" placeholder="Describe what should be done">${scheduledEscape(task?.prompt||'')}</textarea></label>
    <div class="scheduled-form-grid"><label>Mode<select id="schedule-mode">${scheduledSelectOptions([['chat','Chat'],['work','Work'],['codex','Codex']],task?.mode||desktop.mode)}</select></label><label>Runs in<select id="schedule-destination">${scheduledSelectOptions([['new','New chat'],['existing','Existing chat']],task?.kind==='heartbeat'?'existing':'new')}</select></label></div>
    <label id="schedule-thread-row">Chat<select id="schedule-thread">${scheduledSelectOptions(threads,task?.targetThreadId||currentThread)}</select></label>
    <div data-schedule-only class="scheduled-form-grid"><label>Frequency<select id="schedule-frequency">${scheduledSelectOptions([['minutes','Every few minutes'],['hours','Every few hours'],['daily','Daily'],['weekdays','Weekdays'],['weekly','Weekly'],['monthly','Monthly'],['custom','Custom RRULE']],task?'custom':'daily')}</select></label><label>Timezone<input id="schedule-timezone" value="${scheduledEscape(timezone)}" list="schedule-timezones"><datalist id="schedule-timezones"><option>Asia/Shanghai</option><option>UTC</option><option>America/Los_Angeles</option><option>Europe/London</option></datalist></label></div>
    <div data-schedule-only class="scheduled-form-grid"><label>Start date<input id="schedule-date" type="date" value="${local.date}"></label><label>Time<input id="schedule-time" type="time" value="${local.time}"></label></div>
    <label id="schedule-interval-row" data-schedule-only>Interval<input id="schedule-interval" type="number" min="1" step="1" value="1"></label>
    <label id="schedule-weekday-row" data-schedule-only>Day<select id="schedule-weekday">${scheduledSelectOptions([['MO','Monday'],['TU','Tuesday'],['WE','Wednesday'],['TH','Thursday'],['FR','Friday'],['SA','Saturday'],['SU','Sunday']],'MO')}</select></label>
    <label id="schedule-monthday-row" data-schedule-only>Day of month<input id="schedule-monthday" type="number" min="1" max="31" value="1"></label>
    <label id="schedule-rule-row" data-schedule-only>RRULE<input id="schedule-rule" value="${scheduledEscape(task?.schedule?.rrule||'FREQ=DAILY')}"></label>
    <label>Runs on<select disabled aria-label="Execution location"><option>This device</option><option disabled>Cloud · Not connected</option></select></label>
    <p class="scheduled-help" data-schedule-only>Tasks run locally while the app and this computer are running.</p>
    <label data-schedule-only>Execution environment<select id="schedule-environment">${scheduledSelectOptions([['local','Local project'],['worktree','Isolated worktree']],task?.executionEnvironment||'local')}</select></label>
    <label>Workspace<input id="schedule-cwd" value="${scheduledEscape(task?.cwd||currentCwd)}"></label>
    ${projects.length?`<fieldset data-schedule-only><legend>Projects</legend>${projects.map((p,i)=>`<label class="scheduled-check"><input type="checkbox" data-schedule-project="${i}"${checked.has(p.projectId||p.cwd)?' checked':''}>${scheduledEscape(p.name)}</label>`).join('')}</fieldset>`:''}
    <div class="scheduled-form-grid"><label>Model<select id="schedule-model">${scheduledSelectOptions(models,task?.model||currentModel)}</select></label><label data-schedule-only>Thinking effort<select id="schedule-effort"></select></label></div>
    <label data-goal-only hidden>Token budget · optional<input id="schedule-goal-budget" type="number" min="1" step="1" placeholder="No limit"></label>
    <label data-schedule-only>Notifications<select id="schedule-notification"></select></label>
    <label data-schedule-only id="schedule-archive-row" class="scheduled-check"><input id="schedule-archive" type="checkbox"${task?.autoArchive?' checked':''}>Automatically archive successful runs</label>
    <p id="schedule-form-error" class="scheduled-error" role="alert"></p><div class="scheduled-form-actions"><button type="button" class="btn-pill" id="schedule-cancel">Cancel</button><button class="btn-pill primary" type="submit" id="schedule-submit">${task?'Save changes':'Create'}</button></div>
  </form>`);
  const updateEfforts=()=>{const m=modelCatalog.find(m=>m.model===$('schedule-model').value),requested=$('schedule-effort').value||task?.reasoningEffort||currentEffort,reported=(m?.supportedReasoningEfforts||[]).filter(e=>e.reasoningEffort!=='persistent').map(e=>[e.reasoningEffort,EFFORT_LABELS[e.reasoningEffort]||e.label||e.reasoningEffort]),efforts=m?.reasoningMetadataKnown&&reported.length?reported:[['auto','Auto'],...reported];if(!m?.reasoningMetadataKnown&&requested&&!efforts.some(e=>e[0]===requested))efforts.push([requested,(EFFORT_LABELS[requested]||requested)+' (manual)']);const selected=efforts.some(e=>e[0]===requested)?requested:m?.defaultReasoningEffort||efforts[0]?.[0]||'auto';$('schedule-effort').innerHTML=scheduledSelectOptions(efforts,selected);};
  let notificationKind;
  const updateForm=()=>{
    const goal=$('schedule-type').value==='goal',existing=$('schedule-destination').value==='existing',frequency=$('schedule-frequency').value;
    $('scheduled-editor').querySelectorAll('[data-schedule-only]').forEach(n=>n.hidden=goal);$('scheduled-editor').querySelectorAll('[data-goal-only]').forEach(n=>n.hidden=!goal);
    $('schedule-thread-row').hidden=!existing;$('schedule-interval-row').hidden=goal||!['minutes','hours'].includes(frequency);$('schedule-weekday-row').hidden=goal||frequency!=='weekly';$('schedule-monthday-row').hidden=goal||frequency!=='monthly';$('schedule-rule-row').hidden=goal||frequency!=='custom';$('schedule-archive-row').hidden=goal||existing;
    $('schedule-prompt').maxLength=goal?4000:100000;
    $('schedule-environment').disabled=existing;if(existing)$('schedule-environment').value='local';
    $('scheduled-editor').querySelectorAll('[data-schedule-project]').forEach(box=>{box.disabled=existing;if(existing)box.checked=false;});
    $('schedule-cwd').disabled=existing;$('schedule-mode').disabled=existing;
    if(existing){const target=desktop.threads.find(t=>t.id===$('schedule-thread').value);$('schedule-cwd').value=target?.cwd||task?.cwd||currentCwd;$('schedule-mode').value=target?.mode||task?.mode||desktop.mode;}
    if(notificationKind!==existing){$('schedule-notification').innerHTML=scheduledSelectOptions([['default',existing?'Important updates':'All runs'],['failed_runs_only','Failed runs only']],task?.notificationPolicy||'default');notificationKind=existing;}
  };
  $('schedule-type').onchange=updateForm;$('schedule-destination').onchange=updateForm;$('schedule-frequency').onchange=updateForm;$('schedule-thread').onchange=updateForm;$('schedule-model').onchange=updateEfforts;$('schedule-cancel').onclick=closeModal;updateForm();updateEfforts();
  $('scheduled-editor').onsubmit=async event=>{
    event.preventDefault();const button=$('schedule-submit');button.disabled=true;$('schedule-form-error').textContent='';
    try{
      const prompt=$('schedule-prompt').value.trim();if(!prompt)throw new Error('Add instructions.');
      const target=$('schedule-destination').value==='existing'?$('schedule-thread').value:null;if($('schedule-destination').value==='existing'&&!target)throw new Error('Choose an existing chat.');
      if($('schedule-type').value==='goal'){
        const options={threadId:target,mode:$('schedule-mode').value,cwd:$('schedule-cwd').value,model:$('schedule-model').value,tokenBudget:goalBudget($('schedule-goal-budget').value)};
        closeModal();await startDesktopGoal(prompt,options);return;
      }
      const name=$('schedule-name').value.trim();if(!name)throw new Error('Add a task name.');
      const schedule=buildScheduledSchedule({frequency:$('schedule-frequency').value,timezone:$('schedule-timezone').value.trim(),date:$('schedule-date').value,time:$('schedule-time').value,interval:$('schedule-interval').value,weekday:$('schedule-weekday').value,monthday:$('schedule-monthday').value,rrule:$('schedule-rule').value});
      const selectedProjects=Array.from($('scheduled-editor').querySelectorAll('[data-schedule-project]:checked'),node=>projects[Number(node.dataset.scheduleProject)]);
      await rpcCall(task?'scheduler/update':'scheduler/create',{
        ...(task?{id:task.id}:{}),name,prompt,mode:$('schedule-mode').value,cwd:$('schedule-cwd').value,projectId:selectedProjects[0]?.projectId||task?.projectId||desktop.projectId||null,
        model:$('schedule-model').value,reasoningEffort:$('schedule-effort').value,kind:target?'heartbeat':'cron',targetThreadId:target,projects:selectedProjects,
        executionEnvironment:$('schedule-environment').value,schedule,notificationPolicy:$('schedule-notification').value==='failed_runs_only'?'failed_runs_only':null,
        autoArchive:!target&&$('schedule-archive').checked,state:task?.state==='paused'?'paused':'active'
      });
      closeModal();await refreshScheduledView();if(task&&scheduledUi.selected===task.id)await openScheduledTask(task.id);
    }catch(error){if($('schedule-form-error'))$('schedule-form-error').textContent=error.message||String(error);else showError(error);}finally{button.disabled=false;}
  };
}

function goalBudget(value) { if(value===''||value===null||value===undefined)return null;const budget=Number(value);if(!Number.isSafeInteger(budget)||budget<=0)throw new Error('Token budget must be a positive whole number.');return budget; }
async function refreshCurrentGoal() {
  const threadId=currentThread,ticket=++scheduledUi.goalRequest;
  if(!threadId){scheduledUi.goal=null;renderGoalProgress();return;}
  const result=await rpcCall('thread/goal/get',{threadId});
  if(ticket!==scheduledUi.goalRequest||currentThread!==threadId)return;
  scheduledUi.goal=result.goal;renderGoalProgress();
}
function renderGoalProgress() {
  const row=$('goal-progress');if(!row)return;row.replaceChildren();const goal=scheduledUi.goal;
  row.hidden=!goal||goal.threadId!==currentThread;if(row.hidden)return;
  const text=document.createElement('button');text.className='goal-progress-title';text.title=goal.objective;
  text.innerHTML=`<strong>${scheduledEscape(goalLabels[goal.status]||goal.status)}</strong><span>${scheduledEscape(goal.objective)}</span><small>${scheduledEscape(goalUsage(goal))}${goal.suppressed?' · Needs attention':''}</small>`;
  text.onclick=()=>openGoalEditor(goal).catch(showError);row.append(text);
  const actions=document.createElement('div');actions.className='goal-progress-actions';
  if(goal.status==='active')actions.append(scheduledButton('Pause',()=>setDesktopGoalStatus('paused',goal.threadId)));
  else if(goal.status!=='complete')actions.append(scheduledButton('Resume',()=>setDesktopGoalStatus('active',goal.threadId)));
  actions.append(scheduledButton('Edit',()=>openGoalEditor(goal)),scheduledButton('Clear',()=>clearDesktopGoal(goal.threadId)));row.append(actions);
}
async function setDesktopGoalStatus(status,threadId=currentThread) {
  if(!threadId)throw new Error('Open a chat with a goal first.');
  await rpcCall('thread/goal/set',{threadId,status});if(currentThread===threadId)await refreshCurrentGoal();if(scheduledUi.open&&scheduledUi.tab==='goals')await refreshScheduledView();
}
async function clearDesktopGoal(threadId=currentThread) {
  if(!threadId)throw new Error('Open a chat with a goal first.');
  if(!await scheduledConfirm('Clear goal?','The goal will be removed. Your chat will be kept.','Clear'))return false;
  await rpcCall('thread/goal/clear',{threadId});if(currentThread===threadId){scheduledUi.goalRequest++;scheduledUi.goal=null;renderGoalProgress();}closeGoalEditor();if(scheduledUi.open)await refreshScheduledView();
}
async function startDesktopGoal(objective,options={}) {
  objective=objective.trim();if(!objective||objective.length>4000)throw new Error('A goal needs an objective of 1 to 4,000 characters.');
  const threadId=Object.hasOwn(options,'threadId')?options.threadId:currentThread,ticket=desktop.navigation;
  const params={objective,mode:options.mode||desktop.mode,cwd:options.cwd??currentCwd,model:options.model||currentModel,tokenBudget:goalBudget(options.tokenBudget)};
  if(threadId){
    const {goal}=await rpcCall('thread/goal/get',{threadId});
    if(ticket!==desktop.navigation)return null;
    if(goal&&goal.status!=='complete'&&!await scheduledConfirm('Replace current goal?',`This chat already has an unfinished goal: “${goal.objective}”. Replace it?`,'Replace'))return null;
    params.threadId=threadId;if(goal)params.replace=true;
  }
  const result=await rpcCall('thread/goal/start',params);
  if(ticket===desktop.navigation){
    if(options.clearInput&&$('composer-input').value===options.clearInput){$('composer-input').value='';saveDraft();autoResizeInput();updateSubmitButton();}
    await openThread(result.thread.id);
  }else toast('Goal started',{label:'Open chat',run:()=>openThread(result.thread.id)});
  return result;
}
function closeGoalEditor() { if($('goal-editor'))$('goal-editor').hidden=true; }
async function openGoalEditor(goal=scheduledUi.goal) {
  if(!goal){await openScheduledEditor(null,'goal');return;}
  const ticket=desktop.navigation;await flushDocument();if(ticket!==desktop.navigation)return;closeDrawer();const editor=$('goal-editor');editor.hidden=false;
  editor.innerHTML=`<div class="scheduled-detail-heading"><h2>Goal</h2><button class="icon-btn" id="goal-editor-close" aria-label="Close goal editor">✕</button></div><p class="scheduled-help">${scheduledEscape(goalLabels[goal.status]||goal.status)} · ${scheduledEscape(goalUsage(goal))}</p>${goal.reason?`<p class="scheduled-help">${scheduledEscape(goal.reason)}</p>`:''}<label>Objective<textarea id="goal-objective" maxlength="4000" rows="12">${scheduledEscape(goal.objective)}</textarea></label><label>Token budget · optional<input type="number" min="1" step="1" id="goal-budget" value="${scheduledEscape(goal.tokenBudget)}" placeholder="No limit"></label><p id="goal-editor-error" class="scheduled-error" role="alert"></p><div class="scheduled-form-actions" id="goal-editor-actions"></div>`;
  $('goal-editor-close').onclick=closeGoalEditor;
  $('goal-editor-actions').append(scheduledButton('Revert',()=>{$('goal-objective').value=goal.objective;$('goal-budget').value=goal.tokenBudget??'';}),scheduledButton('Save',async()=>{
    try{const objective=$('goal-objective').value.trim();if(!objective||objective.length>4000)throw new Error('Add an objective of 1 to 4,000 characters.');await rpcCall('thread/goal/set',{threadId:goal.threadId,objective,tokenBudget:goalBudget($('goal-budget').value),edit:true});closeGoalEditor();if(currentThread===goal.threadId)await refreshCurrentGoal();if(scheduledUi.open)await refreshScheduledView();}catch(error){if($('goal-editor-error'))$('goal-editor-error').textContent=error.message;else throw error;}
  },'btn-pill primary'));
}
async function handleGoalCommand(raw) {
  const command=raw.slice(5).trim(),ticket=desktop.navigation;
  switch(command.toLowerCase()){
    case '':await refreshCurrentGoal();await openGoalEditor();break;
    case 'pause':await setDesktopGoalStatus('paused');break;
    case 'resume':await setDesktopGoalStatus('active');break;
    case 'edit':await refreshCurrentGoal();await openGoalEditor();break;
    case 'clear':if(await clearDesktopGoal()===false)return;break;
    default:return startDesktopGoal(command,{clearInput:raw});
  }
  if(ticket===desktop.navigation&&$('composer-input').value===raw){$('composer-input').value='';saveDraft();autoResizeInput();updateSubmitButton();}
}

async function installScheduledUi() {
  await desktopReady;
  const nav=scheduledButton('Scheduled',()=>openScheduledView(),'nav-row');nav.id='nav-scheduled';nav.innerHTML='<span class="nav-symbol">◷</span><span>Scheduled</span>';document.querySelector('.sidebar-nav').append(nav);
  const view=document.createElement('section');view.id='scheduled-view';view.hidden=true;view.setAttribute('aria-label','Scheduled tasks and goals');
  view.innerHTML=`<div class="scheduled-page-heading"><h1 id="scheduled-heading">Scheduled</h1><button class="btn-pill primary" id="scheduled-create">+ New task</button></div><div class="scheduled-tabs" role="tablist" aria-label="Task type"><button role="tab" data-scheduled-tab="scheduled">Scheduled</button><button role="tab" data-scheduled-tab="goals">Goals</button></div><div class="scheduled-toolbar"><div class="scheduled-filters" aria-label="Task status"><button data-scheduled-filter="all">All</button><button data-scheduled-filter="active">Active</button><button data-scheduled-filter="paused">Paused</button></div><input id="scheduled-search" type="search" aria-label="Search tasks" placeholder="Search scheduled tasks"></div><div class="scheduled-columns"><div id="scheduled-list" aria-live="polite"></div><aside id="scheduled-detail" hidden aria-label="Task details"></aside></div>`;
  $('main-col').append(view);
  const editor=document.createElement('aside');editor.id='goal-editor';editor.hidden=true;editor.setAttribute('aria-label','Goal editor');$('main-col').append(editor);
  const progress=document.createElement('div');progress.id='goal-progress';progress.hidden=true;progress.setAttribute('aria-label','Goal progress');$('composer-host').prepend(progress);
  $('scheduled-create').onclick=()=>openScheduledEditor(null,scheduledUi.tab==='goals'?'goal':'scheduled').catch(showError);
  let searchTimer;
  $('scheduled-search').oninput=()=>{scheduledUi.search=$('scheduled-search').value;clearTimeout(searchTimer);searchTimer=setTimeout(()=>refreshScheduledView().catch(showError),180);};
  document.querySelectorAll('[data-scheduled-tab]').forEach(button=>button.onclick=()=>{scheduledUi.tab=button.dataset.scheduledTab;scheduledUi.filter='all';scheduledUi.selected=null;$('scheduled-detail').hidden=true;renderScheduledControls();refreshScheduledView().catch(showError);});
  document.querySelectorAll('[data-scheduled-filter]').forEach(button=>button.onclick=()=>{scheduledUi.filter=button.dataset.scheduledFilter;renderScheduledControls();refreshScheduledView().catch(showError);});
  for(const name of ['newThread','openThread','switchMode','showLibrary','openSettingsView','openProject','openDocument']){
    const base=window[name];if(typeof base!=='function')continue;
    window[name]=function(...args){closeScheduledView();scheduledUi.goalRequest++;return Promise.resolve(base.apply(this,args)).then(result=>{refreshCurrentGoal().catch(showError);return result;});};
  }
  const baseClose=closeModal;closeModal=function(){const cancel=scheduledUi.confirm;scheduledUi.confirm=null;if(cancel)cancel(false);baseClose();};
  const baseSend=sendTurn;sendTurn=async function(){const raw=$('composer-input').value.trim();if(/^\/goal(?:\s|$)/i.test(raw))return handleGoalCommand(raw);return baseSend();};
  const baseNotification=handleServerNotification;
  handleServerNotification=function(method,params){
    if(method==='thread/goal/updated'||method==='thread/goal/cleared'){
      if(params.threadId===currentThread){scheduledUi.goalRequest++;scheduledUi.goal=method.endsWith('/cleared')?null:params.goal;renderGoalProgress();}
      if(scheduledUi.open&&scheduledUi.tab==='goals')refreshScheduledView().catch(showError);return;
    }
    if(method==='scheduler/changed'||method==='scheduler/run/updated'){
      if(scheduledUi.open){refreshScheduledView().catch(showError);if(scheduledUi.selected)openScheduledTask(scheduledUi.selected).catch(showError);}return;
    }
    baseNotification(method,params);
  };
  setInterval(()=>{const goal=scheduledUi.goal;if(goal?.status==='active'){const usage=$('goal-progress')?.querySelector('small');if(usage)usage.textContent=goalUsage(goal)+(goal.suppressed?' · Needs attention':'');}},1000);
  await refreshCurrentGoal();
}
const scheduledUiReady=installScheduledUi().catch(showError);

const goalBaseSubmit=handleSubmitOrStop;
handleSubmitOrStop=function(){
  const raw=$('composer-input').value.trim();
  if(/^\/goal(?:\s|$)/i.test(raw)){return handleGoalCommand(raw).catch(showError);}
  if(isTurnActive&&scheduledUi.goal?.status==='active'&&raw){return queueGoalMessage().catch(showError);}
  return goalBaseSubmit();
};
async function queueGoalMessage(){
  const threadId=currentThread,ticket=desktop.navigation,text=$('composer-input').value.trim();if(!threadId||!text)return;
  const files=[...desktopAttachments],mentions=[...mentionedFiles];
  $('composer-input').value='';desktopAttachments=[];mentionedFiles=[];renderComposerChips();autoResizeInput();saveDraft();
  try{await rpcCall('turn/queue',{threadId,model:currentModel,reasoningEffort:currentEffort,input:[{type:'text',text},...files,...mentions.map(path=>({type:'mention',name:path,path}))]});toast('Message queued');}
  catch(error){if(ticket===desktop.navigation&&threadId===currentThread){$('composer-input').value=text;desktopAttachments=files;mentionedFiles=mentions;renderComposerChips();autoResizeInput();}else desktop.drafts.set(threadId,text);throw error;}
}
const goalBaseSubmitState=updateSubmitButton;
updateSubmitButton=function(){goalBaseSubmitState();const queue=isTurnActive&&scheduledUi.goal?.status==='active'&&$('composer-input').value.trim();if(queue){$('submit-btn').setAttribute('aria-label','Queue message');$('submit-btn').title='Queue message';$('submit-btn').classList.remove('stop');$('submit-icon-send').style.display='block';$('submit-icon-stop').style.display='none';}};

$('composer-input').addEventListener('input',()=>updateSubmitButton());

const goalBaseRetry=retryLastTurn;
retryLastTurn=async function(){if(scheduledUi.goal){await startDesktopGoal(scheduledUi.goal.objective,{threadId:scheduledUi.goal.threadId,tokenBudget:scheduledUi.goal.tokenBudget});return;}return goalBaseRetry();};
