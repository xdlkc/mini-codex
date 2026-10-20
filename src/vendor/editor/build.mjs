import fs from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {build} from 'esbuild';
import postcss from 'postcss';
import {schema} from './bridge.js';
const root=path.dirname(fileURLToPath(import.meta.url));
const argument=process.argv.indexOf('--out-dir');
const output=argument<0?path.dirname(root):path.resolve(process.argv[argument+1]);
await fs.mkdir(output,{recursive:true});
const metadataDir=path.join(output,'editor');await fs.mkdir(metadataDir,{recursive:true});
const hash=bytes=>createHash('sha256').update(bytes).digest('hex');
const lock=JSON.parse(await fs.readFile(path.join(root,'package-lock.json'),'utf8'));
for(const entry of Object.values(lock.packages))if(entry.resolved&&!entry.resolved.startsWith('https://registry.npmjs.org/'))throw new Error('Non-official registry URL in lockfile');
const built=await build({absWorkingDir:root,entryPoints:['bridge.js'],bundle:true,format:'iife',globalName:'MiniEditorVendor',target:['es2020','safari15'],minify:true,sourcemap:false,legalComments:'eof',metafile:true,write:false,banner:{js:'/* Offline EditorBridge. Exact dependencies and licenses: vendor/editor/manifest.json and LICENSE-editor-dependencies.txt. */'}});
const js=built.outputFiles[0].contents;await fs.writeFile(path.join(output,'editor.js'),js);
const katexRoot=path.join(root,'node_modules/katex');
const css=postcss.parse(await fs.readFile(path.join(katexRoot,'dist/katex.css'),'utf8'));
css.walkComments(comment=>{if(/sourceMappingURL|sourceURL/.test(comment.text))comment.remove();});
const fonts=[];
css.walkAtRules('font-face',rule=>{rule.walkDecls('src',declaration=>{const match=declaration.value.match(/url\(["']?([^\)"']+\.woff2)["']?\)/);if(!match)throw new Error('Expected WOFF2 font');const relative=match[1],absolute=path.join(katexRoot,'dist',relative);fonts.push({relative,absolute,declaration});});});
const fontMetadata=[];
for(const item of fonts){const bytes=await fs.readFile(item.absolute);item.declaration.value=`url(data:font/woff2;base64,${bytes.toString('base64')}) format('woff2')`;fontMetadata.push({source:'katex/dist/'+item.relative,sha256:hash(bytes),bytes:bytes.length});}
css.walkRules(rule=>{if(rule.parent.type==='atrule'&&/keyframes$/i.test(rule.parent.name))return;rule.selectors=rule.selectors.map(selector=>'.mini-rich-editor '+selector);});
css.walkDecls(declaration=>{if(declaration.prop==='font-family'||declaration.prop==='font')declaration.value=declaration.value.replace(/KaTeX_/g,'MiniEditorKaTeX_');});
const style=(await fs.readFile(path.join(root,'editor-base.css'),'utf8'))+'\n/* Namespaced official KaTeX styles; fonts are offline data URLs. */\n'+css.toString();await fs.writeFile(path.join(output,'editor.css'),style);
const names=new Set();
for(const input of Object.keys(built.metafile.inputs)){const match=input.match(/(?:^|\/)node_modules\/((?:@[^/]+\/)?[^/]+)/);if(match)names.add(match[1]);}
const licenses=[];const packages=[];const licenseDir=path.join(metadataDir,'licenses');await fs.mkdir(licenseDir,{recursive:true});
for(const name of Array.from(names).sort()){
  const directory=path.join(root,'node_modules',name),pkg=JSON.parse(await fs.readFile(path.join(directory,'package.json'),'utf8')),entry=lock.packages['node_modules/'+name];
  if(!entry?.integrity)throw new Error('Missing locked integrity for '+name);
  const candidates=(await fs.readdir(directory)).filter(file=>/^(licen[sc]e|copying)(?:[-.]|$)/i.test(file));
  if(!candidates.length)throw new Error('Missing package license: '+name);
  const files=[];for(const candidate of candidates){const absolute=path.join(directory,candidate);if(!(await fs.stat(absolute)).isFile())continue;const content=await fs.readFile(absolute,'utf8'),filename=name.replace(/[@/]/g,'_')+'-'+candidate;await fs.writeFile(path.join(licenseDir,filename),content);files.push({file:'licenses/'+filename,sha256:hash(content)});licenses.push(`\n${'='.repeat(72)}\n${name}@${pkg.version} — ${candidate}\n${'='.repeat(72)}\n${content}`);}
  packages.push({name,version:pkg.version,source:entry.resolved,integrity:entry.integrity,license:pkg.license,licenses:files});
}
await fs.writeFile(path.join(output,'LICENSE-editor-dependencies.txt'),'Third-party licenses for the offline EditorBridge bundle.\n'+licenses.join('\n'));
await fs.writeFile(path.join(metadataDir,'schema.json'),JSON.stringify(schema,null,2)+'\n');
const sources={};for(const file of ['bridge.js','editor-base.css','build.mjs','package.json','package-lock.json'])sources[file]={sha256:hash(await fs.readFile(path.join(root,file)))};
const manifest={schemaVersion:1,global:'EditorBridge',tiptapVersion:'3.32.0',packages,build:{node:process.version,esbuild:'0.28.2',command:'npm ci --ignore-scripts --registry=https://registry.npmjs.org && node build.mjs --out-dir /absolute/src/vendor',target:['es2020','safari15'],runtimeNetworkRequired:false},sources,outputs:{'editor.js':{bytes:js.length,sha256:hash(js)},'editor.css':{bytes:Buffer.byteLength(style),sha256:hash(style)}},fonts:fontMetadata};
await fs.writeFile(path.join(metadataDir,'manifest.json'),JSON.stringify(manifest,null,2)+'\n');
console.log(`Built editor.js ${js.length} bytes; editor.css ${Buffer.byteLength(style)} bytes; ${packages.length} bundled packages, ${fontMetadata.length} embedded fonts.`);
