import {readFile,writeFile,copyFile} from 'node:fs/promises';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
const root=new URL('./',import.meta.url);
if(!process.argv.includes('--copy-only')){
  const install=spawnSync('npm',['ci','--ignore-scripts','--no-audit','--no-fund'],{cwd:fileURLToPath(root),stdio:'inherit'});
  if(install.status!==0)process.exit(install.status??1);
}
for(const [source,target] of [['@xterm/xterm/lib/xterm.js','xterm.js'],['@xterm/addon-fit/lib/addon-fit.js','addon-fit.js']]){
  let bytes=await readFile(new URL('node_modules/'+source,root));
  // Do not split Unicode lines: xterm contains literal Unicode separators.
  const marker=bytes.lastIndexOf(Buffer.from('\n//# sourceMappingURL='));
  if(marker!==-1){const end=bytes.indexOf(10,marker+1);bytes=Buffer.concat([bytes.subarray(0,marker),end===-1?Buffer.alloc(0):bytes.subarray(end)]);}
  await writeFile(new URL(target,root),bytes);
}
await copyFile(new URL('node_modules/@xterm/xterm/css/xterm.css',root),new URL('xterm.css',root));
await copyFile(new URL('node_modules/@xterm/xterm/LICENSE',root),new URL('../LICENSE-XTERM.txt',root));
await copyFile(new URL('node_modules/@xterm/addon-fit/LICENSE',root),new URL('../LICENSE-XTERM-ADDON-FIT.txt',root));
