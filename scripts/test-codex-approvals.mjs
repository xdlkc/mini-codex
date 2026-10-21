import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source=readFileSync(new URL('../src/gui_codex_permissions.js',import.meta.url),'utf8');
const production=source.slice(source.indexOf('function codexRenderApprovalCard('));
const desktopSource=readFileSync(new URL('../src/gui_desktop.js',import.meta.url),'utf8');
const manager=desktopSource.slice(desktopSource.indexOf('function renderPendingApprovals()'),desktopSource.indexOf('const previousNotification='));
function harness({language='en',mode='codex',editing=false}={}){
  let context;const calls=[];let fallback=0;
  const all=node=>[node,...node.children.flatMap(all)];
  const element=tag=>({tag,children:[],dataset:{},attributes:{},textContent:'',className:'',open:false,
    get lastElementChild(){return this.children.at(-1);},
    append(...children){for(const child of children){child.parent=this;this.children.push(child);}},
    setAttribute(name,value){this.attributes[name]=value;},getAttribute(name){return this.attributes[name]??null;},
    querySelectorAll(selector){return all(this).filter(node=>selector==='[data-approval]'?node.dataset.approval!==undefined:node.tag==='button'&&node.dataset.dec);},
    closest(){return this.tag==='input'?this:null;},focus(){context.document.activeElement=this;},remove(){this.removed=true;if(this.parent)this.parent.children=this.parent.children.filter(child=>child!==this);},
  });
  const messages=element('main'),input=element('input');
  context=vm.createContext({desktop:{mode,approvals:new Map()},currentThread:'thread',$:()=>messages,ICONS:{term:'<svg></svg>',diff:'<svg></svg>'},
    setViewMode(){},scrollBottom(){},rpcSend:message=>calls.push(JSON.parse(JSON.stringify(message))),
    codexText:(en,zh)=>language==='zh'?zh:en,renderApprovalCard(){fallback++;},
    document:{createElement:element,getElementById:()=>messages,activeElement:editing?input:null},
  });
  vm.runInContext(production+'\n'+manager,context);
  const render=(params={},method='item/commandExecution/requestApproval')=>context.renderApprovalCard('request',method,params);
  return {context,calls,messages,input,render,all:()=>all(messages),fallback:()=>fallback,
    nodes:className=>all(messages).filter(node=>node.className.split(' ').includes(className)),
    buttons:()=>all(messages).filter(node=>node.tag==='button'),};
}
test('Deny declines the action instead of turning a command rejection into cancel',()=>{
  const h=harness();h.render({command:'printf test',availableDecisions:['accept','cancel']});
  const deny=h.buttons().find(button=>button.textContent==='Deny');deny.onclick();deny.onclick();
  assert.deepEqual(h.calls,[{id:'request',result:{decision:'decline'}}]);
});
test('a command without a supported scoped grant offers only Deny and Allow once',()=>{
  const h=harness();h.render({command:'printf test',availableDecisions:['accept','cancel']});
  assert.deepEqual(h.buttons().map(button=>button.textContent),['Deny','Allow once']);
  h.buttons()[1].onclick();assert.equal(h.calls[0].result.decision,'accept');
});
test('a file grant is an option under Allow once and sends acceptForSession',()=>{
  const h=harness();h.render({files:['/workspace/a'],availableDecisions:['accept','acceptForSession','decline','cancel']},'item/fileChange/requestApproval');
  assert.equal(h.nodes('codex-approval-options').length,1);
  const session=h.buttons().find(button=>button.textContent==='Allow all edits');
  assert.equal(session.attributes.role,'menuitem');session.onclick();assert.equal(h.calls[0].result.decision,'acceptForSession');
});
test('unsupported file session approval is never offered',()=>{
  const h=harness();h.render({files:['/workspace/a'],availableDecisions:['accept','decline']},'item/fileChange/requestApproval');
  assert.equal(h.nodes('codex-approval-options').length,0);assert.equal(h.buttons().length,2);
});
test('the request reason becomes the title and untrusted values remain text',()=>{
  const h=harness(),reason='<img src=x onerror=alert(1)>',command='<script>bad()</script>';
  h.render({reason,command});
  assert.equal(h.nodes('codex-approval-title')[0].textContent,reason);
  assert.equal(h.nodes('codex-approval-title')[0].innerHTML,undefined);
  assert.equal(h.nodes('approval-cmd')[0].textContent,command);
  assert.equal(h.nodes('approval-cmd')[0].innerHTML,undefined);
  assert.equal(h.nodes('codex-approval-intro')[0].attributes.role,'alert');
});
test('Chinese labels and file paths use the reference wording',()=>{
  const h=harness({language:'zh'});h.render({files:['/workspace/<file>']},'item/fileChange/requestApproval');
  assert.equal(h.nodes('codex-approval-title')[0].textContent,'是否允许 ChatGPT 编辑以下文件？');
  assert.equal(h.nodes('approval-cmd')[0].textContent,'/workspace/<file>');
  assert.ok(h.buttons().some(button=>button.textContent==='允许所有修改'));
});
test('approval options support keyboard entry, movement and Escape',()=>{
  const h=harness();h.render({files:['/workspace/a']},'item/fileChange/requestApproval');
  const options=h.nodes('codex-approval-options')[0],summary=options.children[0],items=options.children[1].children;
  const event=key=>({key,preventDefault(){},stopPropagation(){this.stopped=true;}});
  const down=event('ArrowDown');summary.onkeydown(down);assert.equal(down.stopped,true);assert.equal(options.open,true);assert.equal(h.context.document.activeElement,items[0]);
  options.onkeydown(event('ArrowDown'));assert.equal(h.context.document.activeElement,items[1]);
  options.onkeydown(event('Escape'));assert.equal(options.open,false);assert.equal(h.context.document.activeElement,summary);
});
test('a new request does not take focus away from an active editor',()=>{
  const h=harness({editing:true});h.render({command:'printf test'});assert.equal(h.context.document.activeElement,h.input);
});
test('Chat and Work keep their own approval renderer',()=>{
  const h=harness({mode:'work'});h.render({command:'printf test'});assert.equal(h.fallback(),1);assert.equal(h.messages.children.length,0);
});
test('the desktop approval manager preserves the renderer response guard and resolves its pending entry',()=>{
  const h=harness();h.context.desktop.approvals.set('request',{id:'request',method:'item/commandExecution/requestApproval',params:{threadId:'thread',command:'printf test'}});
  h.context.renderPendingApprovals();h.context.renderPendingApprovals();assert.equal(h.messages.children.length,1);
  const deny=h.buttons().find(button=>button.textContent==='Deny');deny.onclick();deny.onclick();
  assert.deepEqual(h.calls,[{id:'request',result:{decision:'decline'}}]);assert.equal(h.context.desktop.approvals.size,0);assert.equal(h.messages.children.length,0);
});
test('Allow similar commands sends the exact captured prefix as a structured decision',()=>{
  const h=harness(),prefix=['git','push','remote with spaces'];
  h.render({command:'git push',proposedExecpolicyAmendment:prefix,availableDecisions:['accept',{acceptWithExecpolicyAmendment:{execpolicy_amendment:prefix}},'cancel']});
  const option=h.buttons().find(button=>button.textContent==='Allow similar commands');
  assert.ok(option.title.includes("git push 'remote with spaces'"));
  prefix[0]='changed-after-render';option.onclick();
  assert.deepEqual(h.calls,[{id:'request',result:{decision:{acceptWithExecpolicyAmendment:{execpolicy_amendment:['git','push','remote with spaces']}}}}]);
});
test('invalid or multiline command prefixes do not create a reusable approval option',()=>{
  for(const prefix of [[],['git',3],['sh','-c','first\nsecond']]){
    const h=harness();h.render({command:'example',proposedExecpolicyAmendment:prefix});assert.equal(h.buttons().length,2);
  }
});
