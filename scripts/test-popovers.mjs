#!/usr/bin/env node
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
const source=readFileSync(new URL('../src/gui_popovers.js',import.meta.url),'utf8');
function harness(){
  let open=false,naturalHeight=10;const style={maxHeight:'',left:'',top:''};
  const menu={style,classList:{contains:()=>open},getBoundingClientRect(){return {width:320,height:Math.min(naturalHeight,parseFloat(style.maxHeight)||Infinity)};}};
  const context=vm.createContext({window:{innerWidth:735,innerHeight:953,addEventListener(){}},$:()=>menu,openFloatingMenuAt(){open=true;},closeFloatingMenu(){open=false;},MutationObserver:class{constructor(callback){this.callback=callback;}observe(){}}});
  vm.runInContext(source,context);return {context,style,setHeight:h=>naturalHeight=h,isOpen:()=>open};
}
test('a dynamically populated composer menu is repositioned fully above its anchor',()=>{
  const h=harness();h.context.openFloatingMenuAt('model',{isConnected:true,getBoundingClientRect:()=>({left:220,top:900,bottom:930})},'',true);h.setHeight(280);h.context.positionFloatingMenu();assert.equal(h.style.top,'612px');assert.equal(h.style.left,'220px');
});
test('a menu flips above a bottom anchor and stays within horizontal bounds',()=>{
  const h=harness();h.setHeight(300);h.context.openFloatingMenuAt('context',{isConnected:true,getBoundingClientRect:()=>({left:700,top:820,bottom:850})},'',false);assert.equal(h.style.top,'512px');assert.equal(h.style.left,'403px');
});
test('large menus scroll in the available space and close when their anchor is removed',()=>{
  const h=harness(),anchor={isConnected:true,getBoundingClientRect:()=>({left:30,top:450,bottom:480})};h.setHeight(800);h.context.openFloatingMenuAt('large',anchor,'',true);assert.equal(h.style.maxHeight,'453px');assert.equal(h.style.top,'488px');anchor.isConnected=false;h.context.positionFloatingMenu();assert.equal(h.isOpen(),false);
});
test('a missing or detached menu anchor never reaches the base geometry call',()=>{
  const h=harness();h.context.openFloatingMenuAt('missing',null,'');assert.equal(h.isOpen(),false);h.context.openFloatingMenuAt('detached',{isConnected:false},'');assert.equal(h.isOpen(),false);
});
