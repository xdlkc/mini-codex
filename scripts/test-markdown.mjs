#!/usr/bin/env node
// Exercise the exact offline libraries and production renderer used by native and browser clients.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const read=path=>readFileSync(new URL(path,import.meta.url),'utf8');
const vendor=read('../src/vendor/markdown.js');
const html=read('../src/gui_app.html');
const production=html.match(/<script>([\s\S]*?)<\/script>/)[1];
function harness(){
  const listeners=[];
  const context=vm.createContext({console,URL,setTimeout,clearTimeout,document:{
    compatMode:'CSS1Compat',getElementById:()=>({addEventListener(){}}),
    addEventListener(type,handler,capture){listeners.push({type,handler,capture});},
  },addEventListener(){}});
  context.window=context;
  vm.runInContext(vendor,context,{filename:'vendor/markdown.js'});
  vm.runInContext(production,context,{filename:'gui_app.html'});
  return {context,render:context.renderMarkdown,click:listeners.find(x=>x.type==='click'&&x.capture)?.handler};
}
const decode=value=>value.replace(/&(#x[0-9a-f]+|#\d+|amp|lt|gt|quot|apos);/gi,(_,entity)=>{
  if(entity[0]==='#')return String.fromCodePoint(entity[1].toLowerCase()==='x'?parseInt(entity.slice(2),16):Number(entity.slice(1)));
  return {amp:'&',lt:'<',gt:'>',quot:'"',apos:"'"}[entity.toLowerCase()];
});
function attributes(text){return Object.fromEntries(Array.from(text.matchAll(/([\w:-]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'))?/g),m=>[m[1].toLowerCase(),decode(m[2]??m[3]??'')]));}
function tags(markup,name='[a-z][a-z0-9-]*'){return Array.from(markup.matchAll(new RegExp(`<(${name})\\b([^>]*)>`,'gi')),m=>({tag:m[1].toLowerCase(),attrs:attributes(m[2])}));}
function codeText(output){const code=output.match(/<code class="hljs">([\s\S]*?)<\/code>/);assert.ok(code);return decode(code[1].replace(/<[^>]*>/g,''));}

test('CommonMark preserves nested lists, loose paragraphs, blockquotes and ordered list starts',()=>{
  const {render}=harness();
  const output=render('3. Parent\n   - child *emphasis*\n     > nested quote\n\n   A second paragraph.\n\n4. Next\n');
  assert.match(output,/<ol start="3">/);
  assert.match(output,/<li>[\s\S]*<ul>[\s\S]*<li>child <em>emphasis<\/em>[\s\S]*<blockquote>/);
  assert.match(output,/<p>A second paragraph\.<\/p>/);
  assert.equal(tags(output,'ol|ul|blockquote').length,3);
  assert.match(render('Heading\n=======\n\n###### Small heading'),/<h1>Heading<\/h1>[\s\S]*<h6>Small heading<\/h6>/);
});

test('GFM supports escaped table pipes, alignment, strikethrough, task lists and autolinks',()=>{
  const {render}=harness();
  const output=render('| Name | Value |\n| :--- | ---: |\n| a\\|b | **two** |\n\n- [x] ~~Done~~\n- [ ] Next\n\nhttps://example.com and person@example.com');
  assert.match(output,/<div class="markdown-table-wrap"><table>/);
  assert.match(output,/<td style="text-align:left">a\|b<\/td>/);
  assert.match(output,/<td style="text-align:right"><strong>two<\/strong><\/td>/);
  const checkboxes=tags(output,'input');assert.equal(checkboxes.length,2);
  assert.ok(checkboxes.every(x=>x.attrs.type==='checkbox'&&Object.hasOwn(x.attrs,'disabled')));
  assert.ok(Object.hasOwn(checkboxes[0].attrs,'checked'));assert.ok(!Object.hasOwn(checkboxes[1].attrs,'checked'));
  assert.match(output,/<del>Done<\/del>/);
  assert.match(render('~single~ and ~~double~~ but ~~~literal~~~'),/<del>single<\/del> and <del>double<\/del> but ~~~literal~~~/);
  assert.equal(tags(render('- [ ]'),'input').length,1);
  assert.deepEqual(tags(output,'a').map(x=>x.attrs.href),['https://example.com','mailto:person@example.com']);
});

test('escaping, reference links and inline code follow parser boundaries',()=>{
  const {render}=harness();
  const output=render('\\*literal\\* &copy; <b>raw HTML</b>\n\n[Reference][target]\n\n[target]: https://example.com "A title"\n\n``a ` b`` and `**not bold**`');
  assert.ok(output.includes('*literal* © &lt;b&gt;raw HTML&lt;/b&gt;'));
  assert.equal(tags(output,'b').length,0);
  assert.equal(tags(output,'a')[0].attrs.title,'A title');
  assert.match(output,/<code class="inline-code">a ` b<\/code>/);
  assert.match(output,/<code class="inline-code">\*\*not bold\*\*<\/code>/);
  assert.ok(!render('soft\nline').includes('<br'));
  assert.match(render('hard  \nline'),/hard<br>\nline/);
});

test('fenced and indented code are highlighted safely and copied without changing their source',()=>{
  const {render}=harness();
  const code='fn main() { println!("<script>alert(1)</script> & $x$"); }\n';
  const output=render('```rust\n'+code+'```');
  assert.match(output,/class="hljs-keyword">fn<\/span>/);assert.equal(codeText(output),code);
  assert.equal(tags(output,'script|math|img').length,0);
  const copy=tags(output,'button')[0];assert.equal(copy.attrs.onclick,'copyCodeBlock(this)');
  const unknown='<img src=x onerror="bad()">\n';assert.equal(codeText(render('```unknown-language\n'+unknown+'```')),unknown);
  assert.equal(codeText(render('    first\n    second\n')),'first\nsecond\n');
  assert.match(render('`\\(not math\\)`'),/<code class="inline-code">\\\(not math\\\)<\/code>/);
});

test('inline/display TeX renders accessible math while incomplete and invalid TeX remain safe',()=>{
  const {render}=harness();
  const output=render('Inline $x^2$ and \\(y_1\\).\n\n$$\n\\frac{1}{2} + \\sum_{n=1}^{3} n\n$$\n\n\\[\\sqrt{4}\\]');
  assert.equal(tags(output,'math').length,4);assert.ok(tags(output,'mfrac').length>=1);
  assert.ok(output.includes('class="katex-html"'));assert.ok(output.includes('encoding="application/x-tex"'));
  assert.equal(tags(render('The prices are $5 and $10 today.'),'math').length,0);
  for(const value of ['Unclosed $x +','$$\n\\frac{a}{','\\(x +','\\[x +'])assert.equal(tags(render(value),'math').length,0);
  assert.doesNotThrow(()=>render('$\\frac{bad}$'));
  const macro=render('$\\def\\a{\\a}\\a$');assert.match(macro,/katex-error/);
});

test('untrusted HTML, dangerous URL protocols and trusted-only math commands never execute',()=>{
  const {render}=harness();
  for(const destination of ['javascript:alert(1)','java&#x73;cript:alert(1)','javascript&#58;alert(1)','data:text/html,bad','vbscript:bad','ftp://example.com','//example.com/a','file://remote.example/a']){
    const output=render(`[Blocked](${destination}) ![Image](${destination})`);
    assert.equal(tags(output,'a|img').length,0,destination);
  }
  const output=render('<script>alert(1)</script>\n<img src=x onerror=bad()>\n\n$\\href{javascript:bad}{x}$ $\\includegraphics{https://example.com/tracker}$');
  assert.equal(tags(output,'script|img|a|iframe|object|embed').length,0);
  assert.ok(!tags(output).some(x=>Object.keys(x.attrs).some(key=>key.startsWith('on'))));
  assert.equal(tags(render('![remote](https://example.com/a.png)'),'img')[0].attrs.referrerpolicy,'no-referrer');
});

test('file links preserve decoded paths and dispatch one safe function argument',()=>{
  const h=harness(),path='/tmp/My Project/a\'"<&>.rs';
  for(const [markdown,expected] of [
    [`[File](<${encodeURI(path)}>)`,path],
    ['[File](file:///tmp/a%20b.rs#L3)','/tmp/a b.rs'],
    ['[文件](src/%E4%B8%AD%E6%96%87.rs)','src/中文.rs'],
  ]){
    const [anchor]=tags(h.render(markdown),'a');assert.ok(anchor,markdown);
    assert.equal(anchor.attrs['data-ui-value'],expected);assert.equal(anchor.attrs.href,'#');assert.ok(!Object.hasOwn(anchor.attrs,'onclick'));
    const values=[];h.context.openFileInDrawer=value=>values.push(value);
    const element={getAttribute:key=>anchor.attrs[key]??null,hasAttribute:key=>Object.hasOwn(anchor.attrs,key)};
    h.click({target:{closest:()=>element},preventDefault(){},stopPropagation(){}});assert.deepEqual(values,[expected]);
  }
});

test('every prefix of streamed Markdown is renderable and unclosed fences escape HTML',()=>{
  const {render}=harness();
  const message='# Streaming\n\n- parent\n  - child **bold**\n\n| A | B |\n|---|---|\n|x|y|\n\n$$\\frac{x}{y}$$\n\n```javascript\nconst x = "<img src=x onerror=bad()>";\n```\n[Link](https://example.com)';
  for(let end=0;end<=message.length;end++){
    let output;assert.doesNotThrow(()=>{output=render(message.slice(0,end));});assert.equal(typeof output,'string');
    assert.equal(tags(output,'script|iframe|object|embed').length,0);
  }
  const partial=render('```html\n<img src=x onerror="bad()">');assert.equal(tags(partial,'img').length,0);assert.ok(partial.includes('&lt;'));
});

test('bundled libraries and fonts match the recorded offline assets',()=>{
  const manifest=JSON.parse(read('../src/vendor/manifest.json'));
  for(const file of ['markdown.js','markdown.css'])assert.equal(createHash('sha256').update(read('../src/vendor/'+file)).digest('hex'),manifest.outputs[file].sha256);
  const css=read('../src/vendor/markdown.css');assert.equal((css.match(/url\(data:font\/woff2;base64,/g)||[]).length,manifest.fontFiles.length);
  assert.ok(manifest.fontFiles.length>10);assert.ok(!/url\((?!data:)/.test(css));
  const {context}=harness();assert.equal(context.katex.version,manifest.packages.find(x=>x.name==='katex').version);assert.equal(context.hljs.versionString,manifest.packages.find(x=>x.name==='@highlightjs/cdn-assets').version);
});
