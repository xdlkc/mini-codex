import {Editor,Node,Extension,getSchema} from '@tiptap/core';
import StarterKit from '@tiptap/starter-kit';
import Link from '@tiptap/extension-link';
import {Table,TableRow,TableCell,TableHeader} from '@tiptap/extension-table';
import {TaskList,TaskItem} from '@tiptap/extension-list';
import {InlineMath,BlockMath} from '@tiptap/extension-mathematics';
import {DragHandle} from '@tiptap/extension-drag-handle';
import {Plugin,PluginKey} from '@tiptap/pm/state';
import {MarkdownManager} from '@tiptap/markdown';
import {Marked} from 'marked';

const VERSION='3.32.0';
const ATTACHMENT=/^att_[a-fA-F0-9]{32}$/;
const MAX_BYTES=8*1024*1024;
const bytes=value=>new TextEncoder().encode(value).length;
const safeText=(value,max)=>typeof value==='string'&&bytes(value)<=max&&!/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/.test(value)&&!/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?:^|[^\uD800-\uDBFF])[\uDC00-\uDFFF]/.test(value);
const SCHEMA_VERSION=2;
const KATEX={trust:false,throwOnError:false,maxExpand:1000,maxSize:20,strict:'warn'};
export function safeLink(value){
  if(!safeText(value,4096)||/[\s\u0000-\u001f\u007f-\u009f]/u.test(value))return false;
  try{const url=new URL(value);return ['http:','https:'].includes(url.protocol)?Boolean(url.hostname)&&!url.username&&!url.password&&!/^https?:\/\/[^/?#]*@/i.test(value):url.protocol==='mailto:'&&Boolean(url.pathname);}catch{return false;}
}
function mdLabel(value){return String(value||'').replace(/[\\[\]]/g,'\\$&').replace(/[\r\n]/g,' ');}
const SafeLink=Link.extend({
  addAttributes(){return {href:{default:null,parseHTML:element=>element.getAttribute('href')}};},
  parseMarkdown(token,helpers){return helpers.applyMark('link',helpers.parseInline(token.tokens||[]),{href:token.href});},
  renderHTML({HTMLAttributes}){return ['a',{href:safeLink(HTMLAttributes.href)?HTMLAttributes.href:'#',rel:'noopener noreferrer nofollow',target:'_blank'},0];},
}).configure({autolink:false,openOnClick:false,linkOnPaste:false,isAllowedUri:safeLink});
const WorkTable=Table.extend({
  addAttributes(){return {layout:{default:'normal',parseHTML:element=>['normal','flexible','full'].includes(element.dataset.layout)?element.dataset.layout:'normal',renderHTML:attrs=>({'data-layout':attrs.layout})}};},
  addCommands(){return {...this.parent?.(),setTableLayout:layout=>({commands})=>['normal','flexible','full'].includes(layout)&&commands.updateAttributes(this.name,{layout}),fitTable:()=>({state,tr,dispatch})=>{let table=null,position=0;const from=state.selection.$from;for(let depth=from.depth;depth>0;depth--)if(from.node(depth).type.name==='table'){table=from.node(depth);position=from.before(depth);break;}if(!table&&from.nodeAfter?.type.name==='table'){table=from.nodeAfter;position=from.pos;}if(!table)return false;if(dispatch)table.descendants((node,pos)=>{if(node.type.name==='tableCell'||node.type.name==='tableHeader')tr.setNodeMarkup(position+1+pos,undefined,{...node.attrs,colwidth:null});});return true;}};},
}).configure({resizable:true,renderWrapper:true});
function mathTokenizer(block){return {name:block?'blockMath':'inlineMath',level:block?'block':'inline',start:src=>{const a=src.indexOf(block?'$$':'$'),b=src.indexOf(block?'\\[':'\\(');return a<0?b:b<0?a:Math.min(a,b);},tokenize:src=>{const match=block?(src.match(/^\$\$([\s\S]+?)\$\$(?:\n|$)/)||src.match(/^\\\[([\s\S]+?)\\\](?:\n|$)/)):(src.match(/^\$(?!\$)((?:\\.|[^$\n])+?)\$(?!\$)/)||src.match(/^\\\(([^\n]+?)\\\)/));return match?{type:block?'blockMath':'inlineMath',raw:match[0],latex:match[1].trim()}:undefined;}};}
const SafeInlineMath=InlineMath.extend({markdownTokenizer:mathTokenizer(false)}).configure({katexOptions:KATEX});
const SafeBlockMath=BlockMath.extend({markdownTokenizer:mathTokenizer(true)}).configure({katexOptions:KATEX});
function attachmentNode(kind,options){
  const image=kind==='image';
  return Node.create({
    name:kind,group:'block',atom:true,selectable:true,draggable:true,
    addAttributes(){return image?{attachmentId:{default:null},name:{default:''},alt:{default:''},title:{default:''},width:{default:null},height:{default:null}}:{attachmentId:{default:null},name:{default:''},mimeType:{default:'application/octet-stream'},size:{default:null}};},
    parseHTML(){return [];},
    renderHTML({node}){return ['figure',{'data-type':kind,'data-attachment-id':node.attrs.attachmentId},['span',{},node.attrs.alt||node.attrs.name||(image?'Image':'File')]];},
    addCommands(){return {[image?'insertAttachmentImage':'insertFileAttachment']:attrs=>({commands})=>commands.insertContent({type:kind,attrs})};},
    markdownTokenName:image?'image':'fileAttachment',
    parseMarkdown(token){return {type:kind,attrs:{attachmentId:image?String(token.href||'').replace(/^attachment:/,''):token.attachmentId,name:image?'':token.name||'',...(image?{alt:token.text||'',title:token.title||''}:{})}};},
    renderMarkdown(node){return image?`![${mdLabel(node.attrs.alt||node.attrs.name)}](attachment:${node.attrs.attachmentId}${node.attrs.title?' "'+String(node.attrs.title).replace(/["\\\r\n]/g,'\\$&')+'"':''})`:`[${mdLabel(node.attrs.name||'File')}](attachment:${node.attrs.attachmentId})`;},
    ...(image?{}:{markdownTokenizer:{name:'fileAttachment',level:'block',start:()=>-1,tokenize:src=>{const match=src.match(/^\[((?:\\.|[^\]\\\n])*)\]\(attachment:(att_[a-fA-F0-9]{32})\)(?:\n|$)/);return match?{type:'fileAttachment',raw:match[0],attachmentId:match[2],name:match[1].replace(/\\([\\[\]])/g,'$1')}:undefined;}}}),
    addNodeView(){return ({node,getPos,editor})=>{
      let disposed=false,request=0,current=node;const dom=document.createElement('figure');dom.className=image?'editor-attachment-image':'editor-file-attachment';dom.contentEditable='false';dom.draggable=true;
      const caption=document.createElement('span');caption.className='editor-attachment-caption';dom.append(caption);let picture;
      if(image){picture=document.createElement('img');picture.alt=node.attrs.alt||node.attrs.name||'Image';dom.prepend(picture);}
      const refresh=()=>{caption.textContent=current.attrs.name||current.attrs.alt||(image?'Image':'File');dom.dataset.attachmentId=current.attrs.attachmentId;
        if(!image||typeof options.resolveImage!=='function')return;const serial=++request;picture.removeAttribute('src');picture.alt=current.attrs.alt||current.attrs.name||'Image';
        if(current.attrs.width)picture.width=current.attrs.width;else picture.removeAttribute('width');if(current.attrs.height)picture.height=current.attrs.height;else picture.removeAttribute('height');
        Promise.resolve().then(()=>disposed||serial!==request?null:options.resolveImage(current.attrs.attachmentId)).then(url=>{if(disposed||serial!==request)return;if(typeof url!=='string'||url.length>30*1024*1024||!/^data:image\/(?:png|jpeg|webp);base64,[A-Za-z0-9+/=\s]+$/.test(url))throw new Error('Image resolver must return a verified local PNG, JPEG, or WebP data URL');picture.src=url;}).catch(error=>{if(!disposed&&serial===request){caption.textContent='Image unavailable · '+(current.attrs.name||'attachment');options.onError?.(error);}});
      };
      const select=()=>{const position=getPos();if(typeof position!=='number')return null;editor.commands.setNodeSelection(position);return position;};
      dom.onclick=event=>{event.preventDefault();const position=select();if(position!==null)options.onAttachmentClick?.({attachmentId:current.attrs.attachmentId,kind,node:current,position,open:false});};
      dom.ondblclick=event=>{event.preventDefault();event.stopPropagation();const position=select();if(position!==null)options.onAttachmentClick?.({attachmentId:current.attrs.attachmentId,kind,node:current,position,open:true});};
      refresh();return {dom,update(next){if(next.type.name!==kind)return false;const changed=JSON.stringify(next.attrs)!==JSON.stringify(current.attrs);current=next;if(changed)refresh();return true;},destroy(){disposed=true;request++;dom.onclick=null;dom.ondblclick=null;picture?.removeAttribute('src');}};
    };},
  });
}
export function sandboxHTML(html){return '<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src \'none\'; script-src \'unsafe-inline\'; style-src \'unsafe-inline\'; img-src data: blob:; font-src data:; connect-src \'none\'; frame-src \'none\'; form-action \'none\'; base-uri \'none\'"><meta name="viewport" content="width=device-width,initial-scale=1"></head><body>'+html+'</body></html>';}
function workBlock(options){return Node.create({
  name:'workBlock',group:'block',atom:true,draggable:true,
  addAttributes(){return {kind:{default:'prompt'},title:{default:''},text:{default:''},targetId:{default:''},html:{default:''}};},
  parseHTML(){return [{tag:'aside[data-work-block]',getAttrs:element=>{try{return JSON.parse(element.getAttribute('data-work-block'));}catch{return false;}}}];},
  renderHTML({node}){return ['aside',{'data-work-block':JSON.stringify(node.attrs)},['strong',{},node.attrs.title||node.attrs.kind],['pre',{},node.attrs.text||node.attrs.targetId||node.attrs.html]];},
  markdownTokenName:'workBlock',
  markdownTokenizer:{name:'workBlock',level:'block',start:src=>src.indexOf('```mcodex-block\n'),tokenize:src=>{const match=src.match(/^```mcodex-block\n([^\n]+)\n```(?:\n|$)/);if(!match)return;try{return {type:'workBlock',raw:match[0],attrs:JSON.parse(match[1])};}catch{return;}}},
  parseMarkdown:token=>({type:'workBlock',attrs:token.attrs}),
  renderMarkdown:node=>'```mcodex-block\n'+JSON.stringify(node.attrs)+'\n```',
  addNodeView(){return ({node,editor,getPos})=>{
    let current=node;const dom=document.createElement('aside');dom.className='editor-work-block';dom.contentEditable='false';dom.draggable=true;
    const act=action=>{const position=getPos();if(typeof position!=='number')return;editor.commands.setNodeSelection(position);options.onWorkBlockAction?.({action,attrs:{...current.attrs},position});};
    const refresh=()=>{dom.dataset.kind=current.attrs.kind;dom.replaceChildren();const title=document.createElement('strong');title.textContent=current.attrs.title||({prompt:'Prompt',task:'Task',instructions:'Agent Instructions',highlight:'Highlight',page:'Page',chat:'Chat',visualize:'Visualization'})[current.attrs.kind];dom.append(title);
      if(current.attrs.kind==='visualize'){const frame=document.createElement('iframe');frame.title=current.attrs.title||'Local visualization';frame.setAttribute('sandbox','allow-scripts');frame.setAttribute('referrerpolicy','no-referrer');frame.srcdoc=sandboxHTML(current.attrs.html);dom.append(frame);}
      else {const text=document.createElement('p');text.textContent=current.attrs.text||current.attrs.targetId;dom.append(text);}
      const actions=document.createElement('div');actions.className='editor-work-actions';for(const [label,action] of [['Edit block','edit'],...(['page','chat'].includes(current.attrs.kind)?[['Open','open']]:['prompt','task','instructions'].includes(current.attrs.kind)?[['Chat about this','run']]:[])]){const button=document.createElement('button');button.type='button';button.textContent=label;button.onclick=event=>{event.preventDefault();event.stopPropagation();act(action);};actions.append(button);}dom.append(actions);
    };refresh();return {dom,update(next){if(next.type.name!=='workBlock')return false;const changed=JSON.stringify(next.attrs)!==JSON.stringify(current.attrs);current=next;if(changed)refresh();return true;},destroy(){dom.replaceChildren();}};
  };},
});}

function extensions(options={}){
  const list=[StarterKit.configure({link:false,trailingNode:false}),SafeLink,WorkTable,TableRow,TableCell,TableHeader,TaskList,TaskItem.configure({nested:true}),attachmentNode('image',options),attachmentNode('fileAttachment',options),workBlock(options),SafeInlineMath,SafeBlockMath];
  if(options.dragHandle){const config=options.dragHandle===true?{}:options.dragHandle;list.push(DragHandle.configure({...config,render:config.render||(()=>{const handle=document.createElement('button');handle.type='button';handle.className='editor-drag-handle';handle.textContent='⠿';handle.title='Drag block';return handle;})}));}
  return list;
}
const BASE=extensions(),SCHEMA=getSchema(BASE);
const attrNames=type=>Object.keys(type.spec.attrs||{});
export const schema={schemaVersion:SCHEMA_VERSION,tiptapVersion:VERSION,nodes:Object.fromEntries(Object.entries(SCHEMA.nodes).map(([name,type])=>[name,{content:type.spec.content||null,group:type.spec.group||null,marks:type.spec.marks??null,inline:type.isInline,atom:type.isAtom,attrs:Object.fromEntries(Object.entries(type.spec.attrs||{}).map(([key,value])=>[key,{default:value.default??null}]))}])),marks:Object.fromEntries(Object.entries(SCHEMA.marks).map(([name,type])=>[name,{excludes:type.spec.excludes??null,attrs:Object.fromEntries(Object.entries(type.spec.attrs||{}).map(([key,value])=>[key,{default:value.default??null}]))}]))};
function checkAttrs(type,attrs,kind){
  if(attrs!==undefined&&(!attrs||typeof attrs!=='object'||Array.isArray(attrs)))throw new Error('Invalid '+kind+' attributes');
  for(const key of Object.keys(attrs||{}))if(!attrNames(type).includes(key))throw new Error('Unknown '+kind+' attribute: '+key);
  const a={...Object.fromEntries(Object.entries(type.spec.attrs||{}).map(([key,value])=>[key,value.default])),...attrs};
  const positive=value=>Number.isSafeInteger(value)&&value>0&&value<=100000;
  if(kind==='workBlock'){if(!['prompt','task','instructions','highlight','page','chat','visualize'].includes(a.kind)||!safeText(a.title,1024)||!safeText(a.text,65536)||!safeText(a.targetId,256)||!safeText(a.html,65536))throw new Error('Invalid local Work block');if(['page','chat'].includes(a.kind)&&!a.targetId)throw new Error('A local reference ID is required');}
  if(kind==='heading'&&(!positive(a.level)||a.level>6))throw new Error('Invalid heading level');
  if(kind==='orderedList'&&(!(Number.isSafeInteger(a.start)&&a.start>0&&a.start<=1000000000)||![null,'1','a','A','i','I'].includes(a.type)))throw new Error('Invalid ordered list attributes');
  if(kind==='codeBlock'&&a.language!==null&&(!safeText(a.language,128)||/[\u0000-\u001f\u007f-\u009f]/.test(a.language)))throw new Error('Invalid code language');
  if(kind==='taskItem'&&typeof a.checked!=='boolean')throw new Error('Invalid task checked value');
  if(kind==='table'&&!['normal','flexible','full'].includes(a.layout))throw new Error('Invalid table layout');
  if(['tableCell','tableHeader'].includes(kind)&&(!positive(a.colspan)||a.colspan>1000||!positive(a.rowspan)||a.rowspan>1000||![null,'left','center','right'].includes(a.align)||(a.colwidth!==null&&(!Array.isArray(a.colwidth)||a.colwidth.length!==a.colspan||!a.colwidth.every(positive)))))throw new Error('Invalid table cell attributes');
  if(['image','fileAttachment'].includes(kind)){
    if(!ATTACHMENT.test(a.attachmentId))throw new Error('A local attachment ID is required');
    for(const key of ['name',...(kind==='image'?['alt','title']:['mimeType'])])if(!safeText(a[key],key==='name'?1024:key==='mimeType'?255:4096))throw new Error('Invalid attachment label');
    if(kind==='image'&&['width','height'].some(key=>a[key]!==null&&!positive(a[key])))throw new Error('Invalid image dimensions');
    if(kind==='fileAttachment'&&a.size!==null&&(!Number.isSafeInteger(a.size)||a.size<0))throw new Error('Invalid file size');
  }
  if(['inlineMath','blockMath'].includes(kind)&&!safeText(a.latex,65536))throw new Error('Invalid math source');
  if(kind==='link'&&!safeLink(a.href))throw new Error('Links must use http, https, or mailto');
}
export function validateJSON(json){
  const raw=JSON.stringify(json);if(typeof raw!=='string'||bytes(raw)>MAX_BYTES)throw new Error('Rich text document exceeds 8 MiB');let count=0;
  function visit(node,depth=0){if(!node||typeof node!=='object'||Array.isArray(node)||depth>64||++count>100000)throw new Error('Invalid or excessively nested rich text document');
    for(const key of Object.keys(node))if(!['type','attrs','content','marks','text'].includes(key))throw new Error('Unknown rich text node field: '+key);
    const type=SCHEMA.nodes[node.type];if(!type)throw new Error('Unknown rich text node: '+node.type);checkAttrs(type,node.attrs,node.type);
    if(node.text!==undefined&&(node.type!=='text'||!safeText(node.text,MAX_BYTES)||!node.text))throw new Error('Invalid text node');
    if(node.marks!==undefined){if(!Array.isArray(node.marks))throw new Error('Invalid marks');const seen=new Set();for(const mark of node.marks){if(!mark||Object.keys(mark).some(key=>!['type','attrs'].includes(key)))throw new Error('Invalid mark fields');const markType=SCHEMA.marks[mark.type];if(!markType||seen.has(mark.type))throw new Error('Unknown or duplicate rich text mark');seen.add(mark.type);checkAttrs(markType,mark.attrs,mark.type);}if(seen.has('code')&&seen.size>1)throw new Error('Code cannot combine with other marks');}
    if(node.content!==undefined){if(!Array.isArray(node.content))throw new Error('Invalid node content');node.content.forEach(child=>visit(child,depth+1));}
  }
  visit(json);if(json.type!=='doc')throw new Error('Rich text must have a doc root');const node=SCHEMA.nodeFromJSON(json);node.check();return node.toJSON();
}
function manager(){const engine=new Marked({gfm:true,breaks:false});const markdown=new MarkdownManager({marked:engine,markedOptions:{gfm:true,breaks:false},extensions:BASE});return {engine,markdown};}
function literalMarkdown(source){return {type:'doc',content:[source?{type:'paragraph',content:[{type:'text',text:source}]}:{type:'paragraph'}]};}
export function importMarkdown(source){
  if(!safeText(source,MAX_BYTES))throw new Error('Markdown must be valid Unicode text of at most 8 MiB');
  if(!source)return {schemaVersion:SCHEMA_VERSION,json:{type:'doc',content:[{type:'paragraph'}]},sourceMarkdown:source,warnings:[],lossless:true};
  const {engine,markdown}=manager(),warnings=[],seen=new Set();const warn=(code,message)=>{if(!seen.has(code)){seen.add(code);warnings.push({code,message});}};
  if(/^\s*\[\^[^\]]+\]:/m.test(source))warn('footnotes','Footnotes stay in the source editor.');
  const known=new Set(['space','heading','paragraph','text','strong','em','del','link','image','codespan','code','blockquote','list','list_item','hr','br','escape','table','def','underline','inlineMath','blockMath','fileAttachment','taskList','taskItem','checkbox','workBlock']);
  const tokens=engine.lexer(source);
  function inspect(value){if(!value||typeof value!=='object')return;if(Array.isArray(value)){value.forEach(inspect);return;}
    if(typeof value.type==='string'){
      if(value.type==='html')warn('html','Raw HTML stays as literal source and is never executed.');
      else if(!known.has(value.type))warn('unsupported-token','Unsupported Markdown stays in the source editor.');
      if(value.type==='image'&&!ATTACHMENT.test(String(value.href||'').replace(/^attachment:/,'')))warn('external-image','Images must use local attachment IDs. Remote images are not loaded.');
      if(value.type==='link'&&value.title)warn('link-title','Link titles stay in the source editor.');
      if(value.type==='link'&&!safeLink(value.href))warn('unsafe-link','Unsupported link targets stay as literal source.');
    }
    for(const [key,child] of Object.entries(value))if(key!=='links')inspect(child);
  }
  inspect(tokens);let json;
  if(!warnings.length){try{json=validateJSON(markdown.parse(source));const again=validateJSON(markdown.parse(markdown.serialize(json)));if(JSON.stringify(json)!==JSON.stringify(again))warn('roundtrip','Markdown cannot represent this structure without changes.');}catch{warn('unsupported-structure','This Markdown structure stays in the source editor.');}}
  if(warnings.length)json=validateJSON(literalMarkdown(source));
  return {schemaVersion:SCHEMA_VERSION,json,sourceMarkdown:source,warnings,lossless:!warnings.length};
}
export function exportMarkdown(json){
  const clean=validateJSON(json),warnings=[];const add=(code,message)=>{if(!warnings.some(item=>item.code===code))warnings.push({code,message});};
  function inspect(node){if(node.type==='table'&&node.attrs?.layout!=='normal')add('table-layout','Markdown does not store table layout.');if(['tableCell','tableHeader'].includes(node.type)&&(node.attrs?.colspan>1||node.attrs?.rowspan>1||node.attrs?.colwidth||node.content?.length>1))add('table-structure','Markdown cannot retain merged cells, column widths, or multiple blocks in a cell.');if(node.type==='image'&&(node.attrs?.name||node.attrs?.width||node.attrs?.height))add('image-metadata','Image names and dimensions remain in rich text JSON.');if(node.type==='fileAttachment'&&(node.attrs?.size!==null||node.attrs?.mimeType!=='application/octet-stream'))add('file-metadata','File size and MIME type remain in rich text JSON.');node.content?.forEach(inspect);}
  inspect(clean);const markdown=manager().markdown.serialize(clean),again=importMarkdown(markdown);if(!again.lossless||JSON.stringify(again.json)!==JSON.stringify(clean))add('roundtrip','Some rich text structure or metadata cannot round-trip through Markdown.');return {markdown,warnings,lossless:!warnings.length};
}
export function create(options={}){
  let report=options.json!==undefined?null:importMarkdown(options.markdown||''),destroyed=false,mounted=Boolean(options.element),host=options.element;
  const initial=options.json!==undefined?validateJSON(options.json):report.json;let bridge;
  const notifyError=error=>{try{options.onError?.(error);}catch{}};
  const Safety=Extension.create({name:'editorBridgeSafety',addProseMirrorPlugins(){return [new Plugin({key:new PluginKey('editorBridgeSafety'),filterTransaction(transaction){if(!transaction.docChanged)return true;try{validateJSON(transaction.doc.toJSON());return true;}catch(error){notifyError(error);return false;}}})];}});
  host?.classList.add('mini-rich-editor');
  const editor=new Editor({element:options.element||null,content:initial,extensions:[...extensions(options),Safety],editable:options.editable!==false,injectCSS:false,autofocus:false,
    editorProps:{transformPastedHTML:()=>'',handleDrop(view,event){const data=event.dataTransfer;if(data?.files?.length){event.preventDefault();options.onFilesDrop?.(Array.from(data.files));return true;}if(data?.types?.includes('text/html')){event.preventDefault();view.dispatch(view.state.tr.insertText(data.getData('text/plain')));return true;}return false;},attributes:{class:'mini-rich-editor-content',role:'textbox','aria-multiline':'true','aria-label':options.ariaLabel||'Document content'},handlePaste(view,event){const clipboard=event.clipboardData;if(!clipboard?.types?.includes('text/html'))return false;const text=clipboard.getData('text/plain');event.preventDefault();view.dispatch(view.state.tr.insertText(text));return true;},handleClick(view,pos,event){if(event.target?.closest?.('a')){event.preventDefault();return true;}return false;}},
    onUpdate:({transaction})=>{report=null;options.onUpdate?.({json:editor.getJSON(),editor,transaction,bridge});},onSelectionUpdate:({transaction})=>options.onSelectionUpdate?.({editor,transaction,bridge,selection:{from:editor.state.selection.from,to:editor.state.selection.to,empty:editor.state.selection.empty}}),onContentError:({error})=>notifyError(error),
  });
  bridge={editor,get destroyed(){return destroyed;},get mounted(){return mounted;},get commands(){return editor.commands;},getJSON:()=>editor.getJSON(),getMarkdown:()=>report&&!report.lossless?report.sourceMarkdown:exportMarkdown(editor.getJSON()).markdown,exportMarkdown:()=>report&&!report.lossless?{markdown:report.sourceMarkdown,warnings:report.warnings,lossless:false}:exportMarkdown(editor.getJSON()),getImportReport:()=>report,
    setJSON(json,settings={}){const clean=validateJSON(json);report=null;return editor.commands.setContent(clean,{emitUpdate:settings.emitUpdate!==false});},
    setMarkdown(markdown,settings={}){const imported=importMarkdown(markdown);editor.commands.setContent(imported.json,{emitUpdate:false});report=imported;if(settings.emitUpdate!==false)options.onUpdate?.({json:editor.getJSON(),editor,bridge,importReport:imported});return imported;},
    focus:position=>{if(mounted&&!destroyed)return editor.commands.focus(position);return false;},isActive:(...args)=>editor.isActive(...args),canUndo:()=>editor.can().undo(),canRedo:()=>editor.can().redo(),
    mount(element){if(destroyed)throw new Error('Editor is destroyed');if(mounted)editor.unmount();host=element;host.classList.add('mini-rich-editor');editor.mount(element);mounted=true;return bridge;},unmount(){if(!destroyed&&mounted){editor.unmount();mounted=false;}return bridge;},destroy(){if(!destroyed){destroyed=true;editor.destroy();}},
  };
  return bridge;
}
export const EditorBridge=Object.freeze({sandboxHTML,version:VERSION,schemaVersion:SCHEMA_VERSION,create,validateJSON,importMarkdown,exportMarkdown,safeLink,schema,DragHandle});
if(typeof window!=='undefined')window.EditorBridge=EditorBridge;
