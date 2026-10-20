#!/usr/bin/env node
// Run with: node scripts/test-desktop-state.mjs
// Execute the production navigation/save functions with controlled RPC completion order.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../src/gui_desktop.js', import.meta.url), 'utf8');
function between(start, next) {
  const from = source.indexOf(`\n${start}`);
  const to = source.indexOf(`\n${next}`, from + 1);
  if (from < 0 || to < 0) throw new Error(`Cannot locate production function: ${start}`);
  return source.slice(from + 1, to);
}
const production = [
  between('const desktop =', 'const $ ='),
  between('const $ =', 'const titleCase ='),
  between('const titleCase =', 'const uiIcon ='),
  between('function draftKey(', 'async function loadDesktop('),
  between('async function switchMode(', 'function fillPrompt('),
  between('function addTab(', 'function renderTabs('),
  between('openThread=async function(', 'function menuButton('),
  between('sendTurn=async function(', 'handleSubmitOrStop='),
  between('async function flushDocument(', 'async function openDocument('),
  between('const originalUpsert=', 'async function retryLastTurn('),
].join('\n');

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
async function until(condition, message) {
  for (let n = 0; n < 30 && !condition(); n++) await Promise.resolve();
  assert.ok(condition(), message);
}
const plain = value => JSON.parse(JSON.stringify(value));

function harness(respond = () => ({})) {
  const nodes = new Map();
  for (const id of ['composer-input', 'topbar-title', 'nav-home']) {
    const classes = new Set();
    nodes.set(id, {
      value: '', textContent: '', focus() {},
      classList: { add: name => classes.add(name), remove: name => classes.delete(name) },
    });
  }
  const calls = [], view = { mode: 'codex', items: [], blocks: [] };
  let desktop;
  const context = vm.createContext({
    currentThread: null, currentTurn: null, activeTurnBlock: null,
    currentModel: 'original-model', currentCwd: '/original', currentEffort: 'high',
    currentSandbox: 'workspace-write', isTurnActive: false,
    desktopAttachments: [], mentionedFiles: [],
    newThread() {}, openThread() {}, sendTurn() {},
    document: { getElementById: id => nodes.get(id) || null },
    clearTimeout,
    rpcCall(method, params = {}) {
      calls.push({ method, params });
      return Promise.resolve(respond(method, params));
    },
    updateModeUI() { view.mode = desktop.mode; },
    updateCwdUI(cwd) { context.currentCwd = cwd; },
    clearTimeline() { view.items.length = 0; },
    itemEls:new Map(),itemTexts:new Map(),
    ensureTurnBlock(){const block={};view.blocks.push(block);return block;},
    upsertTimelineItem(item) { view.items.push(item); },
    setTurnStatus(active) { context.isTurnActive = active; },
    hideLibrary() { desktop.library = false; },
    refreshThreads: async () => {},
    closeFloatingMenu() {}, closeModal() {}, closeDrawer() {}, closeSettingsView() {},
    autoResizeInput() {}, renderComposerChips() {}, renderTabs() {},
    closeAutocomplete() {}, finalizeTurnWorkedDivider() {}, renderPendingApprovals() {},
  });
  vm.runInContext(production, context, { filename: 'gui_desktop.js (production functions)' });
  desktop = vm.runInContext('desktop', context);
  return { context, desktop, nodes, calls, view, input: nodes.get('composer-input') };
}

test('Chat, Work and Codex keep separate unsent drafts across mode changes', async () => {
  const h = harness();
  h.input.value = 'Codex draft';
  await h.context.switchMode('chat');
  assert.equal(h.input.value, '', 'a fresh Chat must not inherit the Codex draft');
  h.input.value = 'Chat draft';
  await h.context.switchMode('work');
  assert.equal(h.input.value, '', 'a fresh Work view must not inherit the Chat draft');
  h.input.value = 'Work draft';
  for (const [mode, expected] of [['codex', 'Codex draft'], ['chat', 'Chat draft'], ['work', 'Work draft']]) {
    await h.context.switchMode(mode);
    assert.equal(h.desktop.mode, mode);
    assert.equal(h.view.mode, mode);
    assert.equal(h.context.currentThread, null);
    assert.equal(h.input.value, expected);
  }
});

test('opening B after A keeps B visible when A resumes last', async () => {
  const resumes = { A: deferred(), B: deferred() };
  const h = harness((method, params) => method === 'thread/resume' ? resumes[params.threadId].promise : {});
  h.desktop.drafts.set('B', 'Draft for B');
  const openingA = h.context.openThread('A');
  await until(() => h.calls.some(c => c.method === 'thread/resume' && c.params.threadId === 'A'), 'A must be awaiting its RPC');
  const openingB = h.context.openThread('B');
  await until(() => h.calls.some(c => c.method === 'thread/resume' && c.params.threadId === 'B'), 'B must be awaiting its RPC');
  resumes.B.resolve({ cwd: '/B', thread: {
    id: 'B', name: 'Thread B', mode: 'work', projectId: 'project-B',
    turns: [{ items: [{ id: 'message-B', text: 'B content' }] }],
  } });
  await openingB;
  assert.equal(h.input.value, 'Draft for B');
  h.input.value = 'New typing in B';
  resumes.A.resolve({ cwd: '/A', thread: { id: 'A', name: 'Thread A', mode: 'chat' } });
  await openingA;
  assert.equal(h.context.currentThread, 'B');
  assert.equal(h.desktop.mode, 'work');
  assert.equal(h.desktop.projectId, 'project-B');
  assert.equal(h.context.currentCwd, '/B');
  assert.equal(h.nodes.get('topbar-title').textContent, 'Thread B');
  assert.equal(h.input.value, 'New typing in B');
  assert.deepEqual(h.view.items.map(item => item.id), ['message-B']);
  assert.deepEqual(Array.from(h.desktop.tabs, tab => tab.id), ['B']);
});

test('a delayed thread/start sends to its captured target without changing the new view', async () => {
  const starting = deferred(), turning = deferred();
  const h = harness((method, params) => {
    if (method === 'thread/start') return starting.promise;
    if (method === 'turn/start') return turning.promise;
    if (method === 'thread/resume') return { cwd: '/B', thread: { id: params.threadId, name: 'Thread B', mode: 'chat' } };
    return {};
  });
  h.desktop.projectId = 'original-project';
  h.input.value = 'Original task';
  let tier='priority';h.context.selectedServiceTier=()=>tier;
  h.context.desktopAttachments = [{ type: 'image', imageUrl: 'data:image/png;base64,AA==' }];
  h.context.mentionedFiles = ['/original/source.rs'];
  const sending = h.context.sendTurn();
  await until(() => h.calls.some(c => c.method === 'thread/start'), 'send must wait for thread/start');
  await h.context.switchMode('chat');
  await h.context.openThread('B');
  h.input.value = 'New typing in B';
  h.context.currentModel = 'new-model';
  h.context.currentEffort = 'low';tier='default';
  h.context.currentSandbox = 'read-only';
  const assertNewView = () => {
    assert.equal(h.desktop.mode, 'chat');
    assert.equal(h.view.mode, 'chat');
    assert.equal(h.context.currentThread, 'B');
    assert.equal(h.context.currentCwd, '/B');
    assert.equal(h.nodes.get('topbar-title').textContent, 'Thread B');
    assert.equal(h.input.value, 'New typing in B');
    assert.deepEqual(Array.from(h.desktop.tabs, tab => tab.id), ['B']);
  };
  starting.resolve({ cwd: '/original', thread: { id: 'created-A', mode: 'codex' } });
  await until(() => h.calls.some(c => c.method === 'turn/start'), 'the original task must still be submitted');
  assertNewView();
  const started = h.calls.find(c => c.method === 'thread/start');
  assert.deepEqual(plain(started.params), {
    cwd: '/original', mode: 'codex', projectId: 'original-project', model: 'original-model', sandbox: 'workspace-write',
  });
  const sent = h.calls.find(c => c.method === 'turn/start');
  assert.deepEqual(plain(sent.params), {
    threadId: 'created-A', model: 'original-model', reasoningEffort: 'high', serviceTier:'priority', sandboxPolicy: { type: 'workspaceWrite' },
    input: [
      { type: 'text', text: 'Original task' },
      { type: 'image', imageUrl: 'data:image/png;base64,AA==' },
      { type: 'mention', name: '/original/source.rs', path: '/original/source.rs' },
    ],
  });
  turning.resolve({});
  await sending;
  assertNewView();
});

test('flushDocument waits for an active save, then saves edits made during that request', async () => {
  const firstSave = deferred(), secondSave = deferred();
  let updates = 0;
  const h = harness(method => method === 'desktop/document/update' ? [firstSave, secondSave][updates++].promise : {});
  h.desktop.document = { id: 'doc-1', title: 'Original title', content: 'Original content' };
  h.nodes.set('document-title', { value: 'First title' });
  h.nodes.set('document-content', { value: 'First edit' });
  h.nodes.set('document-save-status', { textContent: 'Saving…' });
  const first = h.context.flushDocument();
  assert.equal(updates, 1);
  h.nodes.get('document-title').value = 'Latest title';
  h.nodes.get('document-content').value = 'Edit made while saving';
  let finished = false;
  const second = h.context.flushDocument().then(() => { finished = true; });
  await Promise.resolve();
  assert.equal(updates, 1, 'save requests for one document must be ordered');
  assert.equal(finished, false, 'flush must wait for the pending save');
  firstSave.resolve({ document: { id: 'doc-1', title: 'First title', content: 'First edit' } });
  await first;
  await until(() => updates === 2, 'edits made during the first save must trigger another update');
  assert.equal(finished, false, 'flush must also wait for the latest edit to persist');
  assert.deepEqual(plain(h.calls.filter(c => c.method === 'desktop/document/update').map(c => c.params)), [
    { id: 'doc-1', title: 'First title', content: 'First edit' },
    { id: 'doc-1', title: 'Latest title', content: 'Edit made while saving' },
  ]);
  secondSave.resolve({ document: { id: 'doc-1', title: 'Latest title', content: 'Edit made while saving' } });
  await second;
  assert.equal(h.desktop.document.content, 'Edit made while saving');
  assert.equal(h.desktop.document.title, 'Latest title');
  assert.equal(h.desktop.docSaving, null);
  assert.equal(h.nodes.get('document-save-status').textContent, 'Saved');
});

test('internal Goal continuation prompts create one activity block without rendering a user bubble',()=>{
  const h=harness();h.context.upsertTimelineItem({type:'userMessage',id:'human',content:[{text:'Human objective'}]});
  const item={type:'userMessage',id:'automatic',internal:true,content:[{text:'Private continuation instruction'}]};h.context.upsertTimelineItem(item);h.context.upsertTimelineItem(item);
  assert.deepEqual(h.view.items.map(item=>item.id),['human']);assert.equal(h.view.blocks.length,1);assert.equal(h.context.itemEls.has('automatic'),true);
});

test('prefilled Work requests enable Send without requiring an extra keystroke',()=>{
  const input={value:'',focus(){}},button={disabled:true,setAttribute(){}};
  const context=vm.createContext({$:id=>id==='composer-input'?input:button,autoResizeInput(){},isTurnActive:false,desktop:{loading:false},desktopAttachments:[],mentionedFiles:[]});
  vm.runInContext(between('function fillPrompt(','runPromptPreset=')+'\n'+between('function updateSubmitButton(','$(\'composer-input\').addEventListener'),context);
  context.fillPrompt('Prompt from a local Page');assert.equal(button.disabled,false);
  context.fillPrompt('  ');assert.equal(button.disabled,true);
});
