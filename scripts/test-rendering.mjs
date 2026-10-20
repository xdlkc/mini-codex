#!/usr/bin/env node
// Run the actual desktop renderer and event delegation without external DOM dependencies.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const html = readFileSync(new URL('../src/gui_app.html', import.meta.url), 'utf8');
const vendor = readFileSync(new URL('../src/vendor/markdown.js', import.meta.url), 'utf8');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1].replace('// MARKDOWN_VENDOR',()=>vendor);
const payload = `quote'";globalThis.compromised=true;//<&>`;

class Element {
  constructor() {
    this.innerHTML = ''; this.style = {}; this.value = ''; this.children = [];
    const classes = new Set();
    this.classList = {
      add: name => classes.add(name), remove: name => classes.delete(name), contains: name => classes.has(name),
      toggle(name, force = !classes.has(name)) { force ? classes.add(name) : classes.delete(name); },
    };
    this.queries = new Map();
  }
  addEventListener() {}
  focus() {}
  appendChild(child) { this.children.push(child); return child; }
  querySelector(selector) {
    if (!this.queries.has(selector)) this.queries.set(selector, new Element());
    return this.queries.get(selector);
  }
  querySelectorAll() { return []; }
  getBoundingClientRect() { return { left: 100, top: 300, bottom: 330, width: 220, height: 100 }; }
}

function harness() {
  const nodes = new Map(), listeners = [];
  const node = id => { if (!nodes.has(id)) nodes.set(id, new Element()); return nodes.get(id); };
  const context = vm.createContext({
    console, URL, setTimeout, clearTimeout, innerWidth: 1200, compromised: false,
    document: {
      compatMode: 'CSS1Compat',
      getElementById: node, createElement: () => new Element(), querySelectorAll: () => [],
      addEventListener(type, fn, capture) { listeners.push({ type, fn, capture }); },
    },
    addEventListener() {},
  });
  context.window = context;
  vm.runInContext(script, context, { filename: 'gui_app.html' });
  context.refreshProjects = async () => {};
  context.fixtures = {
    projects: [{ path: `/tmp/${payload}`, label: payload }],
    models: [{ model: payload, displayName: payload, supportedReasoningEfforts: [{ reasoningEffort: payload, label: payload }] }],
    apps: [{ id: payload, label: payload }],
  };
  vm.runInContext('projectsList = fixtures.projects; modelCatalog = fixtures.models; currentModel = fixtures.models[0].model; openInApps = fixtures.apps;', context);
  context.rpcCall = async method => {
    if (method === 'settings/read') return { mcpServers: [{ name: payload }] };
    if (method === 'fs/readDirectory') return { cwd: '/tmp', files: [{ path: `/tmp/${payload}` }] };
    if (method === 'fs/readFile') return { content: 'File content' };
    return {};
  };
  const click = listeners.find(listener => listener.type === 'click' && listener.capture === true)?.fn;
  assert.equal(typeof click, 'function', 'dynamic actions must use capture to beat parent card handlers');
  return { context, node, click };
}

// Parse only emitted start tags and attributes. Attribute entities are decoded once, as in HTML.
function decode(value) {
  return value.replace(/&(#x[\da-f]+|#\d+|amp|lt|gt|quot|apos);/gi, (_, entity) => {
    if (entity[0] === '#') return String.fromCodePoint(entity[1].toLowerCase() === 'x' ? parseInt(entity.slice(2), 16) : Number(entity.slice(1)));
    return { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'" }[entity.toLowerCase()];
  });
}
function tags(markup, name = '[a-z][a-z0-9-]*') {
  return Array.from(markup.matchAll(new RegExp(`<(${name})\\b([^>]*)>`, 'gi')), match => {
    const attributes = new Map();
    for (const attr of match[2].matchAll(/([^\s=/>]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?/g)) {
      attributes.set(attr[1].toLowerCase(), decode(attr[2] ?? attr[3] ?? attr[4] ?? ''));
    }
    return {
      tag: match[1], attributes,
      getAttribute: key => attributes.get(key) ?? null,
      hasAttribute: key => attributes.has(key),
      closest(selector) { return selector === '[data-ui-action]' && attributes.has('data-ui-action') ? this : null; },
    };
  });
}
function dynamicElements(markup) {
  const elements = tags(markup).filter(tag => tag.hasAttribute('data-ui-action'));
  assert.ok(elements.length, 'the renderer must emit usable dynamic actions');
  for (const element of elements) {
    assert.ok(!Array.from(element.attributes.keys()).some(name => /^on/i.test(name)), 'dynamic values must never enter inline event handlers');
  }
  return elements;
}
function dispatch(h, element, functionName, expected) {
  const received = [];
  h.context[functionName] = value => received.push(value);
  let prevented = false, stopped = false;
  // The click target can be a child icon. closest must still find the action owner.
  h.click({ target: { closest: () => element }, preventDefault() { prevented = true; }, stopPropagation() { stopped = true; } });
  assert.deepEqual(received, [expected], 'decoded data must arrive unchanged as one function argument');
  assert.ok(prevented && stopped, 'file/menu clicks must not trigger navigation or parent card toggles');
  assert.equal(h.context.compromised, false);
}
const menuEvent = () => ({ stopPropagation() {}, currentTarget: new Element() });

test('Markdown blocks executable and unsupported URL schemes while keeping ordinary external links', () => {
  const h = harness();
  for (const destination of [
    'javascript:globalThis.compromised=true', 'JaVaScRiPt:globalThis.compromised=true',
    'java\tscript:globalThis.compromised=true', 'java&#x73;cript:globalThis.compromised=true',
    'data:text/html,%3Cscript%3E', 'vbscript:msgbox', 'ftp://example.com/file',
    '//example.com/file', 'file://remote.example/private.txt',
  ]) {
    const output = h.context.renderMarkdown(`[Blocked label](${destination})`);
    assert.equal(tags(output, 'a').length, 0, `${destination} must remain plain text`);
    assert.ok(output.includes('Blocked label'));
  }
  for (const destination of ['https://example.com/?a=1&b=2', 'http://localhost:3000/page', 'mailto:person@example.com']) {
    const [anchor] = tags(h.context.renderMarkdown(`[Normal link](${destination})`), 'a');
    assert.equal(anchor.getAttribute('href'), destination);
    assert.equal(anchor.getAttribute('target'), '_blank');
    assert.match(anchor.getAttribute('rel'), /noopener/);
  }
});

test('Markdown file links preserve quoted paths as data and still open the file', () => {
  const h = harness();
  const maliciousPath = `/tmp/${payload}.rs`;
  const output = h.context.renderMarkdown(`[Open source](${maliciousPath})`);
  const [anchor] = dynamicElements(output);
  assert.equal(anchor.getAttribute('data-ui-action'), 'open-file');
  assert.equal(anchor.getAttribute('href'), '#');
  assert.equal(tags(output, 'script|img').length, 0);
  dispatch(h, anchor, 'openFileInDrawer', maliciousPath);
  for (const [destination, expected] of [
    ['/tmp/main.rs#L9', '/tmp/main.rs'], ['src/main.rs', 'src/main.rs'],
    ['file:///tmp/with%20space.rs#L2', '/tmp/with space.rs'],
  ]) {
    const [file] = dynamicElements(h.context.renderMarkdown(`[File](${destination})`));
    dispatch(h, file, 'openFileInDrawer', expected);
  }
});

test('dynamic project, model, effort and app menu values are passed through data attributes', async () => {
  for (const [render, action, handler, value] of [
    ['toggleProjectPicker', 'project', 'selectProject', `/tmp/${payload}`],
    ['toggleModelPicker', 'model', 'selectModel', payload],
    ['toggleEffortPicker', 'effort', 'selectEffort', payload],
    ['toggleOpenInMenu', 'open-in', 'launchOpenIn', payload],
  ]) {
    const h = harness();
    await h.context[render](menuEvent());
    const element = dynamicElements(h.node('floating-menu').innerHTML).find(el => el.getAttribute('data-ui-action') === action);
    assert.ok(element, render);
    dispatch(h, element, handler, value);
  }
});

test('settings remove/switch actions preserve hostile names without executable handlers', async () => {
  for (const [section, action, handler, value] of [
    ['mcp', 'remove-mcp', 'removeMcpServerFromSettings', payload],
    ['environments', 'remove-project', 'removeProjectFromSettings', `/tmp/${payload}`],
    ['environments', 'project', 'selectProject', `/tmp/${payload}`],
  ]) {
    const h = harness();
    h.context.closeSettingsView = () => {};
    await h.context.renderSettingsSection(section);
    const element = dynamicElements(h.node('settings-body').innerHTML).find(el => el.getAttribute('data-ui-action') === action);
    assert.ok(element, action);
    dispatch(h, element, handler, value);
  }
});

test('workspace previews, mentions and autocomplete keep quoted paths usable', async () => {
  const h = harness();
  const path = `/tmp/${payload}`;
  vm.runInContext('currentDrawerTab = "files";', h.context);
  await h.context.renderDrawerContent();
  const files = dynamicElements(h.node('drawer-content').innerHTML);
  dispatch(h, files.find(el => el.getAttribute('data-ui-action') === 'mention-file'), 'addFileMention', path);
  // Preserve the production preview function before substituting it for dispatch verification.
  const preview = h.context.previewWorkspaceFile;
  dispatch(h, files.find(el => el.getAttribute('data-ui-action') === 'preview-file'), 'previewWorkspaceFile', path);
  let modal;
  h.context.openCustomModal = (_, content) => { modal = content; };
  h.context.closeModal = () => {};
  await preview(path);
  dispatch(h, dynamicElements(modal)[0], 'addFileMention', path);
  h.context.autoResizeInput = () => {};
  h.node('composer-input').value = '@quote';
  await h.context.handleComposerInput();
  dispatch(h, dynamicElements(h.node('autocomplete-popover').innerHTML)[0], 'selectAtFile', path);
});

test('read and diff file cards use the same safe delegated file action', () => {
  for (const item of [
    { id: 'read', type: 'commandExecution', status: 'completed', commandActions: [{ type: 'read', path: `/tmp/${payload}`, name: payload }] },
    { id: 'diff', type: 'fileChange', status: 'completed', changes: [{ path: `/tmp/${payload}`, diff: '+added', kind: { type: 'update' } }] },
  ]) {
    const h = harness();
    h.context.setViewMode = () => {};
    h.context.scrollBottom = () => {};
    h.context.ensureTurnBlock = () => h.node('turn');
    h.context.upsertTimelineItem(item);
    for (const part of item.type === 'fileChange' ? ['hdr', 'drawer'] : ['hdr']) {
      const rendered = vm.runInContext(`itemEls.get(${JSON.stringify(item.id + ':' + part)}).innerHTML`, h.context);
      for (const element of dynamicElements(rendered)) dispatch(h, element, 'openFileInDrawer', `/tmp/${payload}`);
    }
  }
});
