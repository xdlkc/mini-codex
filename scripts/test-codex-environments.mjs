import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import path from 'node:path';
import test from 'node:test';
const require=createRequire(import.meta.url);
const {JSDOM}=process.env.EDITOR_TEST_NODE_MODULES?require(path.join(process.env.EDITOR_TEST_NODE_MODULES,'jsdom')):createRequire(new URL('../src/vendor/editor/package.json',import.meta.url))('jsdom');
const source=readFileSync(new URL('../src/gui_codex_environments.js',import.meta.url),'utf8');
const workspace=readFileSync(new URL('../src/gui_workspaces.js',import.meta.url),'utf8');
const buttonHelper=workspace.slice(workspace.indexOf('function workspaceButton('),workspace.indexOf('function workspaceField('));
const plain=value=>JSON.parse(JSON.stringify(value));
const entry={type:'success',configPath:'/repo/.codex/environments/environment.toml',cwdRelativeToGitRoot:'.',environment:{version:1,name:'local',setup:{script:'echo initial'},actions:[]}};
const tick=async()=>{for(let i=0;i<20;i++)await Promise.resolve();};
function harness(t){
  const dom=new JSDOM('<!doctype html><div id="settings-body"></div><div id="composer-host"></div><section id="codex-environment-status" hidden></section><div id="codex-environment-actions"></div><div id="floating-menu"></div>',{runScripts:'outside-only',pretendToBeVisual:true,url:'http://127.0.0.1/'}),w=dom.window;
  t.after(()=>dom.window.close());const h={w,calls:[],errors:[],toasts:[],entry:plain(entry),handler:null,selection:entry.configPath};
  Object.assign(w,{$:id=>w.document.getElementById(id),desktop:{mode:'codex',navigation:1},currentCwd:'/repo',currentThread:null,currentSettingsSection:'environments',host:'alpha',
    codexText:(en)=>en,baseName:path.basename,codexChromeReady:new Promise(()=>{}),
    remoteContext:()=>w.host,remoteUi:{byConnection:new Map([['connection-a',{profileId:'alpha'}],['connection-b',{profileId:'beta'}]])},
    workspaceCapture:()=>{const host=w.host;return {key:host+'@connection',navigation:w.desktop.navigation,call:async(method,params={})=>{
      h.calls.push({host,method,params:plain(params)});const custom=h.handler?.(method,params);if(custom!==undefined)return custom;
      if(method==='desktop/project/list')return {items:[]};
      if(method==='workspace/environment/list')return {environments:[plain(h.entry)],platform:'linux',defaultConfigPath:entry.configPath};
      if(method==='workspace/environment/selection/read')return {configPath:h.selection};
      if(method==='workspace/environment/read')return {exists:true,raw:'saved',revision:'sha256:original',environment:plain(h.entry)};
      if(method==='workspace/environment/save')return {type:'success',revision:'sha256:saved'};
      if(method==='workspace/environment/selection/set'){h.selection=params.configPath;return {configPath:params.configPath};}
      if(method==='workspace/environment/run')return {run:{id:'envrun_'+'1'.repeat(32),workspaceRoot:params.cwd,configPath:params.configPath,scriptType:params.scriptType,status:'running',log:''}};
      if(method==='workspace/environment/cancel')return {run:null};
      throw new Error('Unexpected request '+method);
    }};},
    workspaceSameHost:context=>context.key===w.host+'@connection',workspaceCurrent:context=>context.key===w.host+'@connection'&&context.navigation===w.desktop.navigation,
    workspaceRequire:context=>{if(!w.workspaceCurrent(context))throw new Error('Execution context changed');},
    codexPermissionsInert:()=>{w.$('composer-host').inert=w.codexEnvironmentBusy();},
    showError:error=>h.errors.push(error.message),toast:message=>h.toasts.push(message),
    renderSettingsSection:async()=>{},updateCwdUI:()=>{},openThread:async()=>{},switchMode:async()=>{},handleServerNotification:()=>{},closeSettingsView:()=>{},
    menuButton:(label,run)=>{const button=w.document.createElement('button');button.textContent=label;button.onclick=run;return button;},
    openFloatingMenuAt:()=>{},closeFloatingMenu:()=>{},codexMenuKeyboard:()=>{},
  });
  w.eval(buttonHelper+'\n'+source);h.button=text=>[...w.document.querySelectorAll('button')].find(button=>button.textContent===text);return h;
}
test('the real editor saves platform scripts and action data with a revision without running setup',async t=>{
  const h=harness(t),w=h.w;await w.codexEnvironmentEditor(entry.configPath);
  w.document.querySelector('.codex-env-field input').value='dev "quoted"';
  const setup=w.document.querySelector('.codex-env-script'),select=setup.querySelector('select'),script=setup.querySelector('textarea');script.value='echo default';select.value='darwin';select.dispatchEvent(new w.Event('change'));script.value='echo mac\nprintf 中文';
  await h.button('Add action').onclick();const row=w.document.querySelector('.codex-env-action-row');row.querySelector('input').value='Test';row.querySelector('textarea').value='cargo test';
  await h.button('Save').onclick();const request=h.calls.find(call=>call.method==='workspace/environment/save');assert.equal(request.host,'alpha');assert.equal(request.params.expectedRevision,'sha256:original');assert.match(request.params.raw,/\[setup\.darwin\]/);assert.match(request.params.raw,/\[\[actions\]\]/);assert.match(request.params.raw,/dev \\"quoted\\"/);
  assert.ok(!h.calls.some(call=>call.method==='workspace/environment/run'));assert.deepEqual(h.errors,[]);
});
test('a revision conflict keeps the form edits and requires explicit discard before reload',async t=>{
  const h=harness(t),w=h.w;h.handler=method=>method==='workspace/environment/save'?{type:'conflict'}:undefined;
  await w.codexEnvironmentEditor(entry.configPath);const name=w.document.querySelector('input');name.value='Keep unsaved';await h.button('Save').onclick();
  assert.equal(name.value,'Keep unsaved');assert.match(w.document.querySelector('[role=alert]').textContent,/changed on disk/);assert.ok(!h.calls.some(call=>call.method==='workspace/environment/selection/set'));
  assert.equal(h.button('Save').disabled,true);
  h.entry.environment.name='Disk value';await h.button('Discard edits').onclick();assert.equal(w.document.querySelector('input').value,'Disk value');
});
test('editing an existing unselected environment preserves its version and selection without executing it',async t=>{
  const h=harness(t);h.selection=null;h.entry.environment.version=2;
  await h.w.codexEnvironmentEditor(entry.configPath);h.w.document.querySelector('input').value='Updated';await h.button('Save').onclick();
  assert.match(h.calls.find(call=>call.method==='workspace/environment/save').params.raw,/\nversion = 2\n/);
  assert.ok(!h.calls.some(call=>['workspace/environment/selection/set','workspace/environment/run'].includes(call.method)));assert.equal(h.button('Set up project'),undefined);
});
test('a host change during save cannot select an environment or rewrite the newer settings',async t=>{
  const h=harness(t),w=h.w;let resolve;const pending=new Promise(done=>resolve=done);h.handler=method=>method==='workspace/environment/save'?pending:undefined;
  await w.codexEnvironmentEditor(entry.configPath);const saving=h.button('Save').onclick();await tick();w.host='beta';w.desktop.navigation++;w.$('settings-body').textContent='New host settings';resolve({type:'success',revision:'new'});await saving;
  assert.equal(w.$('settings-body').textContent,'New host settings');assert.equal(h.calls.filter(call=>call.method==='workspace/environment/save')[0].host,'alpha');assert.ok(!h.calls.some(call=>call.method==='workspace/environment/selection/set'));
});
test('background and remote run events cannot block the current host composer',async t=>{
  const h=harness(t),w=h.w;const run={id:'envrun_'+'a'.repeat(32),workspaceRoot:'/repo',scriptType:'setup',status:'running',log:'<img src=x onerror=alert(1)>'};
  w.handleServerNotification('workspace/environment/updated',{connectionId:'connection-b',run});assert.notEqual(w.$('composer-host').inert,true);assert.equal(w.$('codex-environment-status').hidden,true);
  w.handleServerNotification('workspace/environment/updated',{connectionId:'connection-a',run});assert.equal(w.$('composer-host').inert,true);assert.equal(w.$('codex-environment-status').hidden,false);assert.equal(w.document.querySelector('img'),null);assert.equal(w.document.querySelector('pre').textContent,run.log);
  await h.button('Stop').onclick();assert.equal(h.calls.at(-1).host,'alpha');assert.equal(h.calls.at(-1).params.id,run.id);
  w.handleServerNotification('workspace/environment/updated',{connectionId:'connection-a',run:{...run,status:'succeeded'}});assert.equal(w.$('composer-host').inert,false);
});
test('toolbar actions use the execution host platform and retain their original indices',async t=>{
  const h=harness(t);h.w.currentThread='thread-alpha';h.entry.environment.actions=[{name:'Mac only',command:'echo mac',platform:'darwin'},{name:'Linux only',command:'echo linux',platform:'linux'}];
  await h.w.codexEnvironmentRefresh();assert.equal(h.button('Mac only'),undefined);await h.button('Linux only').onclick();const call=h.calls.find(call=>call.method==='workspace/environment/run');assert.equal(call.host,'alpha');assert.equal(call.params.actionIndex,1);assert.equal(call.params.scriptType,'action');assert.notEqual(h.w.$('composer-host').inert,true);
});
test('canonical run paths still bind to a legacy trailing-slash checkout',async t=>{
  const h=harness(t);h.w.currentCwd='/repo/';h.w.codexEnvironmentRemember({id:'envrun_'+'d'.repeat(32),workspaceRoot:'/repo',status:'running',scriptType:'setup',log:''},'alpha');
  assert.equal(h.w.codexEnvironmentBusy(),true);assert.equal(h.w.$('composer-host').inert,true);assert.equal(h.w.$('codex-environment-status').hidden,false);
});
test('a queued output frame cannot restore a stale running state after completion',async t=>{
  const h=harness(t),w=h.w;const run={id:'envrun_'+'c'.repeat(32),workspaceRoot:'/repo',scriptType:'setup',status:'running',startedAt:10,outputSequence:0,log:''};
  w.handleServerNotification('workspace/environment/updated',{connectionId:'connection-a',run});w.document.querySelector('details').open=true;
  w.handleServerNotification('workspace/environment/outputDelta',{connectionId:'connection-a',id:run.id,workspaceRoot:'/repo',sequence:1,delta:'中文🙂'});
  w.handleServerNotification('workspace/environment/updated',{connectionId:'connection-a',run:{...run,status:'succeeded',outputSequence:1,log:'中文🙂'}});
  await new Promise(resolve=>setTimeout(resolve,30));assert.equal(w.codexEnvironmentRun().status,'succeeded');assert.equal(w.$('composer-host').inert,false);assert.equal(w.document.querySelector('details').open,true);
  w.codexEnvironmentRemember({...run,outputSequence:0},'alpha');assert.equal(w.codexEnvironmentRun().status,'succeeded');
});
test('failed local environment settings expose a working retry and never keep stale actions',async t=>{
  const h=harness(t);let failed=true;h.handler=method=>method==='workspace/environment/list'&&failed?Promise.reject(new Error('Host unavailable')):undefined;
  await h.w.renderSettingsSection('environments');assert.match(h.w.$('settings-body').textContent,/Host unavailable/);assert.ok(h.button('Retry'));failed=false;await h.button('Retry').onclick();assert.match(h.w.$('settings-body').textContent,/Create local environment/);assert.deepEqual(h.errors,[]);
});
test('new environment names use the first free source filename and inherited files stay grouped',async t=>{
  const h=harness(t),w=h.w;
  const inherited={...plain(entry),configPath:'/repo/.codex/environments/environment.toml'};
  h.handler=(method,params)=>method==='desktop/project/list'?{items:[{path:'/repo/nested',name:'Nested'}]}:method==='workspace/environment/list'&&params.cwd==='/repo/nested'?{environments:[inherited,{...plain(entry),configPath:'/repo/nested/.codex/environments/environment.toml'},{...plain(entry),configPath:'/repo/nested/.codex/environments/environment-2.toml'}]}:undefined;
  await w.renderSettingsSection('environments');const nested=[...w.document.querySelectorAll('[role=listitem]')].find(card=>card.textContent.includes('Nested'));
  assert.match(nested.querySelector('summary').textContent,/Inherited environments \(1\)/);
  assert.equal(w.codexEnvironmentNewPath('/repo/nested',[inherited,{configPath:'/repo/nested/.codex/environments/environment.toml'},{configPath:'/repo/nested/.codex/environments/environment-2.toml'}]),'/repo/nested/.codex/environments/environment-3.toml');
  await [...nested.querySelectorAll('button')].find(button=>button.textContent==='Create local environment').onclick();
  assert.equal(h.calls.filter(call=>call.method==='workspace/environment/read').at(-1).params.cwd,'/repo/nested');assert.equal(h.calls.at(-1).params.configPath,'/repo/nested/.codex/environments/environment-3.toml');assert.equal(w.currentCwd,'/repo');
});
test('unchanged and incomplete drafts disable Save while edits enable it',async t=>{
  const h=harness(t),w=h.w;await w.codexEnvironmentEditor(entry.configPath);assert.equal(h.button('Save').disabled,true);
  const name=w.document.querySelector('input');name.value='Edited';name.dispatchEvent(new w.Event('input',{bubbles:true}));assert.equal(h.button('Save').disabled,false);
  await h.button('Add action').onclick();await tick();assert.equal(h.button('Save').disabled,false);
  const row=w.document.querySelector('.codex-env-action-row');row.querySelector('input').value='Test';row.querySelector('input').dispatchEvent(new w.Event('input',{bubbles:true}));assert.equal(h.button('Save').disabled,true);assert.match(row.textContent,/Add a command/);row.querySelector('textarea').value='npm test';row.querySelector('textarea').dispatchEvent(new w.Event('input',{bubbles:true}));assert.equal(h.button('Save').disabled,false);
});
test('finishing a save cannot replace a newer settings view on the same host',async t=>{
  const h=harness(t),w=h.w;let resolve;h.handler=method=>method==='workspace/environment/save'?new Promise(done=>resolve=done):undefined;
  await w.codexEnvironmentEditor(entry.configPath);const saving=h.button('Save').onclick();await tick();await w.renderSettingsSection('environments');const newer=w.$('settings-body').textContent;resolve({type:'success',revision:'new'});await saving;assert.equal(w.$('settings-body').textContent,newer);
});
test('blank action rows are ignored and lifecycle script whitespace is preserved in source-format TOML',async t=>{
  const h=harness(t),w=h.w;await w.codexEnvironmentEditor(entry.configPath);w.document.querySelector('input').value='Edited';const script=w.document.querySelector('.codex-env-script textarea');script.value=' \n  echo preserved\n ';
  await h.button('Add action').onclick();await tick();await h.button('Save').onclick();const raw=h.calls.find(call=>call.method==='workspace/environment/save').params.raw;
  assert.ok(raw.startsWith('# THIS IS AUTOGENERATED. DO NOT EDIT MANUALLY\nversion = 1\n'));assert.ok(raw.includes("script = '''\n \n  echo preserved\n '''"));assert.ok(!raw.includes('[[actions]]'));assert.deepEqual(h.errors,[]);
});
test('action icon selection preserves an unset icon until explicitly selected',async t=>{
  const h=harness(t),w=h.w;h.entry.environment.actions=[{name:'Build',command:'make',icon:null}];await w.codexEnvironmentEditor(entry.configPath);
  const icon=w.document.querySelector('.codex-env-action-icon');assert.equal(icon.value,'');assert.equal(icon.getAttribute('aria-label'),'Tool');icon.onclick();assert.equal(w.document.querySelector('[role=menuitemradio][aria-checked=true]').textContent,'Tool');
  await h.button('Run').onclick();assert.equal(icon.value,'run');assert.equal(icon.getAttribute('aria-label'),'Run');await h.button('Save').onclick();assert.match(h.calls.find(call=>call.method==='workspace/environment/save').params.raw,/icon = "run"\ncommand = "make"/);
});
test('an existing environment with an empty name requires a name instead of inventing one',async t=>{
  const h=harness(t);h.entry.environment.name='';await h.w.codexEnvironmentEditor(entry.configPath);assert.equal(h.w.document.querySelector('input').value,'');assert.equal(h.button('Save').disabled,true);
  await h.w.renderSettingsSection('environments');assert.equal(h.w.document.querySelector('.codex-env-entry').textContent,'environment.toml');
});
test('an editor read failure has a retry and cannot expose a blank replacement form',async t=>{
  const h=harness(t);let failed=true;h.handler=method=>method==='workspace/environment/read'&&failed?Promise.reject(new Error('Read unavailable')):undefined;
  await h.w.codexEnvironmentEditor(entry.configPath);assert.match(h.w.$('settings-body').textContent,/Read unavailable/);assert.equal(h.button('Save'),undefined);assert.ok(h.button('Back'));failed=false;await h.button('Retry').onclick();assert.equal(h.w.document.querySelector('input').value,'local');
});
test('a removed selected environment resolves the default while explicit None and invalid files remain unselected',async t=>{
  const h=harness(t),w=h.w;w.currentThread='thread-alpha';h.entry.environment.actions=[{name:'Default action',command:'echo ready'}];h.selection='/repo/.codex/environments/removed.toml';
  await w.codexEnvironmentRefresh();assert.ok(h.button('Default action'));assert.ok(!h.calls.some(call=>call.method==='workspace/environment/selection/set'));
  h.selection=null;await w.codexEnvironmentRefresh();assert.equal(h.button('Default action'),undefined);
  const bad={type:'error',configPath:'/repo/.codex/environments/broken.toml',error:{message:'invalid TOML'}};h.selection=bad.configPath;h.handler=method=>method==='workspace/environment/list'?{environments:[bad,h.entry],platform:'linux'}:undefined;
  await w.codexEnvironmentRefresh();assert.equal(h.button('Default action'),undefined);
  const other={...plain(h.entry),configPath:'/repo/.codex/environments/other.toml'};assert.equal(w.codexEnvResolveSelection([other,h.entry],'/missing'),h.entry.configPath);assert.equal(w.codexEnvResolveSelection([other],'/missing'),other.configPath);
});
