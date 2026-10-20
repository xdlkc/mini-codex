#!/usr/bin/env node
// Exercise the shipped controller with fake media. This never records audio or contacts a provider.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source=readFileSync(new URL('../src/gui_voice.js',import.meta.url),'utf8');
const plain=value=>JSON.parse(JSON.stringify(value));
function deferred(){let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};}
async function settle(){for(let i=0;i<12;i++)await Promise.resolve();await new Promise(resolve=>setImmediate(resolve));}

function harness(options={}){
  const nodes=new Map(),elements=[],calls=[],errors=[],opened=[],beacons=[],sent=[],listeners=new Map(),peers=[],streams=[],recorders=[],mediaCalls=[],focusCalls=[],observers=[],ready=deferred();
  let serial=0;
  function make(tag='div'){
    let id='',markup='',inert=false;const classes=new Set();
    const node={tagName:tag.toUpperCase(),attributes:{},style:{},dataset:{},children:[],hidden:false,disabled:false,value:'',textContent:'',parentElement:null,
      classList:{add:value=>classes.add(value),remove:value=>classes.delete(value),contains:value=>classes.has(value),toggle(value,force){const add=force??!classes.has(value);if(add)classes.add(value);else classes.delete(value);return add;}},
      setAttribute(key,value){this.attributes[key]=String(value);if(key==='id')this.id=value;},getAttribute(key){return this.attributes[key]??null;},removeAttribute(key){delete this.attributes[key];},
      append(...items){for(const item of items){item.parentElement=this;this.children.push(item);}},
      replaceChildren(...items){this.children=[];this.append(...items);},
      before(item){const parent=this.parentElement;if(parent){item.parentElement=parent;parent.children.splice(parent.children.indexOf(this),0,item);}},
      remove(){this.removed=true;if(this.parentElement)this.parentElement.children=this.parentElement.children.filter(child=>child!==this);},
      contains(other){for(let current=other;current;current=current.parentElement)if(current===this)return true;return false;},
      focus(){focusCalls.push(this.id);for(let current=this;current;current=current.parentElement)if(current.inert||current.hidden||current.disabled)return;document.activeElement=this;listeners.get('document:focusin')?.({target:this});},getClientRects(){for(let current=this;current;current=current.parentElement)if(current.hidden||current.removed)return [];return [{}];},
      querySelectorAll(selector){const result=[];function walk(parent){for(const child of parent.children){if(selector.split(',').some(tag=>child.tagName===tag.toUpperCase()))result.push(child);walk(child);}}walk(this);return result;},
      addEventListener(){},play(){this.playCount=(this.playCount||0)+1;return Promise.resolve();},pause(){this.pauseCount=(this.pauseCount||0)+1;},
    };
    Object.defineProperty(node,'id',{get:()=>id,set:value=>{id=value;nodes.set(value,node);}});
    Object.defineProperty(node,'inert',{get:()=>inert,set:value=>{inert=Boolean(value);for(const observer of observers)if(observer.target===node)queueMicrotask(()=>observer.callback([{attributeName:'inert'}]));}});
    Object.defineProperty(node,'isConnected',{get:()=>!node.removed&&document.body.contains(node)});
    Object.defineProperty(node,'options',{get:()=>node.children.filter(child=>child.tagName==='OPTION')});
    Object.defineProperty(node,'innerHTML',{get:()=>markup,set:value=>{markup=value;node.children=[];for(const match of value.matchAll(/<(\w+)\b([^>]*)>/g)){const attributes=Object.fromEntries(Array.from(match[2].matchAll(/([\w-]+)="([^"]*)"/g),entry=>[entry[1],entry[2]]));if(!attributes.id&&match[1]!=='summary')continue;const child=make(match[1]);if(attributes.id)child.id=attributes.id;child.attributes=attributes;node.append(child);}}});
    elements.push(node);return node;
  }
  const document={body:make('body'),activeElement:null,createElement:make,getElementById:id=>nodes.get(id)||null,addEventListener(type,callback){listeners.set('document:'+type,callback);}};
  const app=make('div');app.id='app';document.body.append(app);
  for(const id of ['composer-input','submit-btn']){const node=make(id==='composer-input'?'textarea':'button');node.id=id;app.append(node);}
  nodes.get('composer-input').selectionStart=0;nodes.get('composer-input').selectionEnd=0;
  class Track{constructor(){this.enabled=true;this.stopCount=0;}stop(){this.stopCount++;this.readyState='ended';}}
  class Stream{constructor(tracks=[new Track()]){this.tracks=tracks;streams.push(this);}getTracks(){return this.tracks;}getAudioTracks(){return this.tracks;}}
  class Channel{constructor(){this.readyState='connecting';this.sent=[];}send(value){this.sent.push(JSON.parse(value));}close(){this.closed=true;this.readyState='closed';}event(value){this.onmessage?.({data:JSON.stringify(value)});}}
  class Peer{
    constructor(){this.connectionState='new';this.tracks=[];this.remoteDescriptions=[];peers.push(this);}
    addTrack(track,stream){this.tracks.push({track,stream});}
    createDataChannel(label){this.channel=new Channel();this.label=label;return this.channel;}
    async createOffer(){return {type:'offer',sdp:'v=0\r\nfake-offer'};}
    async setLocalDescription(description){this.localDescription=description;}
    async setRemoteDescription(description){this.remoteDescriptions.push(description);this.remoteDescription=description;this.connectionState='connected';this.onconnectionstatechange?.();this.channel.readyState='open';this.channel.onopen?.();}
    close(){this.closeCount=(this.closeCount||0)+1;this.connectionState='closed';}
    fail(){this.connectionState='failed';this.onconnectionstatechange?.();}
  }
  class Recorder{
    static isTypeSupported(type){return type==='audio/webm;codecs=opus';}
    constructor(stream,settings={}){this.stream=stream;this.mimeType=settings.mimeType||'audio/webm';this.state='inactive';recorders.push(this);}
    start(){this.state='recording';}
    stop(){this.state='inactive';queueMicrotask(()=>{this.ondataavailable?.({data:new Blob(['fake-recorded-audio'],{type:this.mimeType})});this.onstop?.();});}
  }
  class Reader{readAsDataURL(blob){blob.arrayBuffer().then(buffer=>{this.result='data:'+blob.type+';base64,'+Buffer.from(buffer).toString('base64');this.onload?.();}).catch(error=>this.onerror?.(error));}}
  const desktop={mode:'chat',navigation:0,projectId:null,threads:[{id:'chat-A'}],drafts:new Map()};
  const h={calls,errors,opened,beacons,sent,listeners,peers,streams,recorders,mediaCalls,focusCalls,nodes,desktop,Track,Stream,getMedia:()=>new Stream(),respond:options.respond};
  const context=vm.createContext({
    console,Date,Map,Set,JSON,Promise,TextEncoder,Blob,FileReader:Reader,setTimeout,clearTimeout,queueMicrotask,document,desktop,desktopReady:ready.promise,MutationObserver:class{constructor(callback){this.callback=callback;}observe(target){this.target=target;observers.push(this);}},
    $:id=>nodes.get(id)||null,crypto:{randomUUID:()=>`call-${++serial}`},navigator:{mediaDevices:{getUserMedia(settings){mediaCalls.push(settings);return Promise.resolve(h.getMedia());}},sendBeacon(url,blob){beacons.push({url,blob});return true;}},
    RTCPeerConnection:Peer,MediaStream:Stream,MediaRecorder:Recorder,currentThread:'chat-A',currentCwd:'/workspace',currentModel:'gpt-test',isTurnActive:false,scheduledUi:{goal:null},eventStream:null,
    rpcCall(method,params={},timeout){calls.push({method,params:plain(params),timeout});return Promise.resolve().then(()=>{
      const custom=h.respond?.(method,params,timeout);if(custom!==undefined)return custom;
      if(method==='voice/capabilities')return {available:options.available!==false,reason:'Connect an API key',defaultModel:'gpt-realtime-2.1',voices:['marin','cedar']};
      if(method==='voice/start')return {id:params.id,threadId:params.threadId,sdp:'v=0\r\nfake-answer'};
      if(method==='voice/end')return {threadId:'chat-A'};
      if(method==='voice/transcribe')return {text:'dictated words'};
      if(method==='thread/start')return {thread:{id:'new-chat'},cwd:'/workspace'};
      if(method==='thread/fork')return {thread:{id:'copied-chat'}};
      return {};
    });},
    rpcSend:request=>sent.push(plain(request)),showError:error=>errors.push(String(error?.message||error)),toast:message=>h.toast=message,
    saveDraft(){desktop.drafts.set(context.currentThread||desktop.mode,nodes.get('composer-input').value);},savePreferences:async values=>{h.preferences=values;},autoResizeInput(){},updateSubmitButton(){},updateCwdUI(){},addTab(){},refreshThreads:async()=>{},
    openThread:async id=>{if(h.openFailure)throw new Error(h.openFailure);opened.push(id);context.currentThread=id;desktop.navigation++;nodes.get('composer-input').value=desktop.drafts.get(id)||'';},
    newThread:async()=>{context.currentThread=null;desktop.navigation++;},switchMode:async mode=>{desktop.mode=mode;context.currentThread=null;desktop.navigation++;},
    openSettingsView:async section=>{h.settings=section;desktop.navigation++;},showLibrary:async()=>{desktop.navigation++;},showFilesLibrary:async()=>{desktop.navigation++;},openScheduledView:async()=>{desktop.navigation++;},openProject:async()=>{desktop.navigation++;},openDocument:async()=>{desktop.navigation++;},openSearch:async()=>{desktop.navigation++;},useWorkspace:async()=>{desktop.navigation++;},
    sendTurn:async()=>calls.push({method:'ordinary-send'}),handleServerNotification(){},__requestClose:async()=>{h.closed=true;},
    addEventListener(type,callback){listeners.set(type,callback);},
  });
  context.window=context;h.context=context;h.node=id=>nodes.get(id);vm.runInContext(source,context,{filename:'gui_voice.js'});h.state=vm.runInContext('voiceUi',context);const installed=vm.runInContext('voiceReady',context);
  h.install=async()=>{ready.resolve();await installed;};
  h.start=async()=>{await context.startVoiceChat();return h.state.current;};
  h.event=event=>context.receiveVoiceEvent(h.state.current,event);
  h.methods=name=>calls.filter(call=>call.method===name);
  return h;
}
const item=(id,role,previous,content=[])=>({type:'conversation.item.created',previous_item_id:previous,item:{id,role,content}});
const final=(id,role,text)=>({type:role==='user'?'conversation.item.input_audio_transcription.completed':'response.output_audio_transcript.done',item_id:id,transcript:text});
const done=output=>({type:'response.done',response:{id:'response-A',status:'completed',output:output||[]}});

test('boot and the unavailable API-key action never request microphone access',async()=>{
  const h=harness({available:false});await h.install();assert.equal(h.mediaCalls.length,0);assert.ok(h.node('voice-button'));assert.ok(h.node('dictation-button'));
  await h.context.openVoiceOverlay();assert.equal(h.node('voice-start').hidden,true);assert.equal(h.node('voice-connect').hidden,false);assert.equal(h.mediaCalls.length,0);
  await h.node('voice-connect').onclick();assert.equal(h.settings,'account');assert.equal(h.mediaCalls.length,0);
  await h.context.startVoiceChat();assert.equal(h.mediaCalls.length,0);assert.match(h.state.error,/API key/);
});

test('the real WebRTC controller attaches audio, handles speaking/mute/interrupt, and releases every resource',async()=>{
  const h=harness();await h.install();const session=await h.start(),peer=h.peers[0],local=h.streams[0].getTracks()[0];
  assert.equal(peer.label,'oai-events');assert.equal(peer.tracks[0].track,local);assert.equal(h.node('voice-status').textContent,'Listening');
  const start=h.methods('voice/start')[0];assert.equal(start.params.threadId,'chat-A');assert.equal(start.params.voice,'marin');assert.equal(Object.hasOwn(start.params,'instructions'),false);
  const remote=new h.Stream(),audio=session.audio;peer.ontrack({streams:[remote],track:remote.getTracks()[0]});await settle();assert.equal(audio.srcObject,remote);assert.equal(audio.playCount,1);
  h.event({type:'input_audio_buffer.speech_started'});assert.equal(h.node('voice-status').textContent,'You are speaking');assert.equal(h.node('voice-orb').classList.contains('is-hearing'),true);
  h.context.toggleVoiceMute();assert.equal(local.enabled,false);assert.equal(h.node('voice-status').textContent,'Microphone muted');h.context.toggleVoiceMute();assert.equal(local.enabled,true);
  h.event({type:'input_audio_buffer.speech_stopped'});h.event({type:'output_audio_buffer.started'});assert.equal(h.node('voice-status').textContent,'Speaking');assert.equal(h.node('voice-orb').classList.contains('is-speaking'),true);
  h.context.interruptVoiceResponse();assert.deepEqual(plain(peer.channel.sent),[{type:'response.cancel'},{type:'output_audio_buffer.clear'}]);
  h.event({type:'output_audio_buffer.cleared'});assert.equal(h.node('voice-orb').classList.contains('is-speaking'),false);
  await assert.rejects(h.context.sendTurn(),/End voice/);await h.context.endVoiceChat({refresh:false});
  assert.equal(local.stopCount,1);assert.equal(remote.getTracks()[0].stopCount,1);assert.equal(peer.closeCount,1);assert.equal(peer.channel.closed,true);assert.equal(audio.srcObject,null);assert.equal(audio.pauseCount,1);assert.equal(audio.removed,true);assert.equal(h.state.current,null);
  await h.context.endVoiceChat({session,refresh:false});assert.equal(h.methods('voice/end').length,1);assert.equal(local.stopCount,1);
});

test('voice creates a native chat only after media access and keeps its scope',async()=>{
  const h=harness();await h.install();h.context.currentThread=null;const session=await h.start();assert.equal(session.threadId,'new-chat');assert.equal(h.context.currentThread,'new-chat');
  assert.equal(h.methods('thread/start').length,1);assert.equal(h.methods('voice/start')[0].params.threadId,'new-chat');await h.context.endVoiceChat({refresh:false});
});

test('navigation during microphone permission stops the late stream and never starts a provider call',async()=>{
  const permission=deferred(),h=harness();await h.install();h.getMedia=()=>permission.promise;const start=h.start();await settle();assert.equal(h.mediaCalls.length,1);
  await h.context.openThread('chat-B');const stream=new h.Stream();permission.resolve(stream);await start;
  assert.equal(stream.getTracks()[0].stopCount,1);assert.equal(h.methods('voice/start').length,0);assert.equal(h.context.currentThread,'chat-B');assert.equal(h.state.current,null);assert.equal(h.peers.length,0);
});

test('ending while signaling is pending cleans local media and hangs up a late successful call',async()=>{
  const answer=deferred(),h=harness({respond:method=>method==='voice/start'?answer.promise:undefined});await h.install();const start=h.start();await settle();const session=h.state.current,peer=h.peers[0];
  assert.equal(h.methods('voice/start').length,1);await h.context.endVoiceChat({refresh:false});assert.equal(peer.closeCount,1);assert.equal(h.streams[0].getTracks()[0].readyState,'ended');
  answer.resolve({id:session.id,sdp:'late-answer'});await start;await settle();assert.equal(peer.remoteDescriptions.length,0);assert.equal(h.methods('voice/end').length,2);assert.equal(h.methods('voice/end')[1].params.id,session.id);
});

test('connection errors release resources and retry uses a fresh call without changing a Goal',async()=>{
  const h=harness();await h.install();await h.start();const old=h.peers[0],id=h.state.current.id;old.fail();await settle();assert.equal(old.closeCount,1);assert.equal(h.state.current,null);assert.match(h.state.error,/connection ended/);assert.equal(h.node('voice-retry').hidden,false);
  await h.context.retryVoiceChat();assert.notEqual(h.state.current.id,id);assert.equal(h.peers.length,2);await h.context.endVoiceChat({refresh:false});
  h.context.scheduledUi.goal={status:'active'};const prior=h.mediaCalls.length;await h.context.startVoiceChat();assert.equal(h.mediaCalls.length,prior);assert.equal(h.context.scheduledUi.goal.status,'active');assert.match(h.state.error,/Pause the active goal/);
});

test('a late input transcript gates the complete assistant response and saved prefixes stay immutable',async()=>{
  const h=harness();await h.install();const session=await h.start();
  h.event(item('user-1','user',null));h.event(item('assistant-1','assistant','user-1'));h.event(final('assistant-1','assistant','Hello back'));h.event(done());await settle();assert.equal(h.methods('voice/transcript').length,0);
  h.event(final('user-1','user','Hello'));await settle();assert.deepEqual(h.methods('voice/transcript')[0].params.messages,[{id:'user-1',role:'user',text:'Hello',interrupted:false},{id:'assistant-1',role:'assistant',text:'Hello back',interrupted:false}]);
  h.event(final('user-1','user','Changed transcript'));h.event({type:'conversation.item.truncated',item_id:'assistant-1'});h.event(done());await settle();assert.equal(h.methods('voice/transcript').length,1);assert.equal(session.saved[0].text,'Hello');assert.equal(session.saved[1].interrupted,false);
  h.event(item('user-2','user','assistant-1'));h.event(final('user-2','user','Next question'));await settle();assert.equal(h.methods('voice/transcript').length,1,'new messages wait for their own response.done');
  h.event(done());await settle();assert.equal(h.methods('voice/transcript').length,2);assert.equal(h.methods('voice/transcript')[1].params.messages[2].text,'Next question');await h.context.endVoiceChat({refresh:false});
});

test('response completion may arrive before item anchors without saving an orphan or reversing the conversation',async()=>{
  const h=harness();await h.install();await h.start();h.event(final('assistant-1','assistant','Answer'));h.event(done([{id:'assistant-1',role:'assistant',content:[{type:'audio',transcript:'Answer'}]}]));await settle();assert.equal(h.methods('voice/transcript').length,0);
  h.event(item('assistant-1','assistant','user-1'));h.event(final('user-1','user','Question'));await settle();assert.equal(h.methods('voice/transcript').length,0);
  h.event(item('user-1','user',null));await settle();assert.deepEqual(h.methods('voice/transcript')[0].params.messages.map(message=>message.id),['user-1','assistant-1']);await h.context.endVoiceChat({refresh:false});
});

test('end waits for a pending prefix save and retains partial transcripts as interrupted for retry',async()=>{
  const saved=deferred();let endAttempts=0;const h=harness({respond:method=>method==='voice/transcript'?saved.promise:method==='voice/end'?(++endAttempts===1?Promise.reject(new Error('Try ending again')):{threadId:'chat-A'}):undefined});await h.install();const session=await h.start();
  h.event(item('user-1','user',null));h.event(final('user-1','user','First'));h.event(done());await settle();h.event(item('assistant-1','assistant','user-1'));h.event({type:'response.output_audio_transcript.delta',item_id:'assistant-1',delta:'Received partial'});
  const end=h.context.endVoiceChat({refresh:false});await settle();assert.equal(h.methods('voice/end').length,0);saved.resolve({saved:1});await assert.rejects(end,/Try ending again/);
  const messages=h.methods('voice/end')[0].params.messages;assert.deepEqual(messages,[{id:'user-1',role:'user',text:'First',interrupted:false},{id:'assistant-1',role:'assistant',text:'Received partial',interrupted:true}]);
  await h.context.retryVoiceChat();assert.deepEqual(h.methods('voice/end')[1].params.messages,messages);assert.notEqual(h.state.current.id,session.id);await h.context.endVoiceChat({refresh:false});
});

test('transcript UTF-8 limits stop unsafe saves, and empty unknown text is never invented',async()=>{
  const h=harness();await h.install();await h.start();h.event(item('user-1','user',null));h.event(final('user-1','user','文'.repeat(22000)));h.event(done());await settle();assert.equal(h.methods('voice/transcript').length,0);assert.match(h.state.error,/message limit/);
  const session=h.state.current;await assert.rejects(h.context.endVoiceChat({refresh:false}),/message limit/);assert.deepEqual(h.methods('voice/end')[0].params.messages,[]);assert.equal(h.state.unsaved.get(session.id),session);assert.equal(session.items.get('user-1').text,'文'.repeat(22000));
  await h.context.__requestClose();assert.equal(h.closed,undefined);assert.deepEqual(h.sent,[{__window:'closeCancelled'}]);await assert.rejects(h.context.retryVoiceSave(),/message limit/);h.state.closeRequested=false;await h.context.discardVoiceTranscript();await h.context.discardVoiceTranscript();assert.equal(h.state.unsaved.size,0);
  await h.start();h.event(item('empty','user',null));await h.context.endVoiceChat({refresh:false});assert.deepEqual(h.methods('voice/end').at(-1).params.messages,[]);
});

test('dictation inserts text at the original selection, uses a long RPC timeout, and never sends the draft',async()=>{
  const h=harness();await h.install();const input=h.node('composer-input');input.value='Say old now';input.selectionStart=4;input.selectionEnd=7;
  await h.context.startDictation();assert.equal(h.state.dictation.status,'recording');assert.equal(h.recorders[0].state,'recording');assert.equal(h.mediaCalls.length,1);assert.equal(h.methods('voice/transcribe').length,0);
  h.context.stopDictation(false);await settle();assert.equal(input.value,'Say dictated words now');assert.equal(h.desktop.drafts.get('chat-A'),input.value);assert.equal(h.methods('voice/transcribe')[0].timeout,60000);assert.equal(h.methods('voice/transcribe')[0].params.mimeType,'audio/webm;codecs=opus');assert.equal(h.methods('ordinary-send').length,0);assert.equal(h.state.dictation,null);assert.equal(h.streams[0].getTracks()[0].readyState,'ended');
});

test('a late transcription updates only the original draft after navigation',async()=>{
  const transcript=deferred(),h=harness({respond:method=>method==='voice/transcribe'?transcript.promise:undefined});await h.install();const input=h.node('composer-input');input.value='Original ';input.selectionStart=input.selectionEnd=input.value.length;await h.context.startDictation();h.context.stopDictation(false);await settle();assert.equal(h.state.dictation.status,'transcribing');
  await h.context.openThread('chat-B');input.value='Do not overwrite B';transcript.resolve({text:'saved words'});await settle();assert.equal(input.value,'Do not overwrite B');assert.equal(h.context.currentThread,'chat-B');assert.equal(h.desktop.drafts.get('chat-A'),'Original saved words');assert.equal(h.toast,'Dictation saved to the original draft');assert.equal(h.methods('ordinary-send').length,0);
});

test('cancel, navigation and late permission never upload a cancelled dictation',async()=>{
  for(const action of ['cancel','navigate','pagehide']){const h=harness();await h.install();await h.context.startDictation();if(action==='cancel')h.context.cancelDictation(true);if(action==='navigate')await h.context.openThread('chat-B');if(action==='pagehide')h.listeners.get('pagehide')();await settle();assert.equal(h.methods('voice/transcribe').length,0);assert.equal(h.streams[0].getTracks()[0].readyState,'ended');assert.equal(h.state.dictation,null);}
  const permission=deferred(),h=harness();await h.install();h.getMedia=()=>permission.promise;const start=h.context.startDictation();await settle();await h.context.newThread();const stream=new h.Stream();permission.resolve(stream);await start;assert.equal(stream.getTracks()[0].readyState,'ended');assert.equal(h.recorders.length,0);assert.equal(h.methods('voice/transcribe').length,0);
});

test('repeated dictation clicks cannot create concurrent permission requests',async()=>{
  const permission=deferred(),h=harness();await h.install();h.getMedia=()=>permission.promise;const start=h.context.startDictation();await settle();assert.equal(h.node('dictation-button').disabled,true);await h.context.startDictation();assert.equal(h.mediaCalls.length,1);
  permission.resolve(new h.Stream());await start;assert.equal(h.recorders.length,1);h.context.cancelDictation(true);await settle();assert.equal(h.methods('voice/transcribe').length,0);
});

test('native close awaits end while pagehide uses an identified native RPC or a preview beacon',async()=>{
  const ended=deferred(),h=harness({respond:method=>method==='voice/end'?ended.promise:undefined});await h.install();await h.start();const close=h.context.__requestClose();await settle();assert.equal(h.closed,undefined);assert.equal(h.peers[0].closeCount,1);ended.resolve({threadId:'chat-A'});await close;assert.equal(h.closed,true);
  for(const native of [true,false]){const h=harness();await h.install();await h.start();if(native)h.context.ipc={};else h.context.eventStream={};h.listeners.get('pagehide')();assert.equal(h.peers[0].closeCount,1);assert.equal(h.streams[0].getTracks()[0].readyState,'ended');assert.equal(h.state.current,null);if(native){assert.equal(h.sent.length,1);assert.match(h.sent[0].id,/^voice-exit-call-/);assert.equal(h.sent[0].method,'voice/end');assert.equal(h.beacons.length,0);}else{assert.equal(h.beacons.length,1);assert.equal(h.beacons[0].url,'/rpc');const request=JSON.parse(await h.beacons[0].blob.text());assert.ok(request.id);assert.equal(request.method,'voice/end');}}
});

test('close and pagehide retain an end that was already pending before the window closed',async()=>{
  const ended=deferred(),h=harness({respond:method=>method==='voice/end'?ended.promise:undefined});await h.install();await h.start();const end=h.context.endVoiceChat({refresh:false});await settle();assert.equal(h.state.current,null);
  const close=h.context.__requestClose();await settle();assert.equal(h.closed,undefined);h.context.ipc={};h.listeners.get('pagehide')();assert.equal(h.sent.length,1);assert.equal(h.sent[0].method,'voice/end');ended.resolve({threadId:'chat-A'});await end;await close;assert.equal(h.closed,true);assert.equal(h.state.ending.size,0);
});

test('account logout and navigation close voice without sending late errors into the next chat',async()=>{
  const h=harness();await h.install();await h.start();h.context.handleServerNotification('account/updated',{account:null});await settle();assert.equal(h.state.current,null);assert.equal(h.peers[0].closeCount,1);
  await h.start();h.event(item('user-1','user',null));h.event({type:'conversation.item.input_audio_transcription.delta',item_id:'user-1',delta:'Old chat partial'});await h.context.openThread('chat-B');await settle();assert.equal(h.state.current,null);assert.equal(h.state.open,false);assert.equal(h.methods('voice/end').at(-1).params.messages[0].interrupted,true);assert.equal(h.context.currentThread,'chat-B');assert.equal(h.opened.at(-1),'chat-B');
});

test('a late hangup failure cannot reopen an old voice surface after navigation',async()=>{
  const ended=deferred(),h=harness({respond:method=>method==='voice/end'?ended.promise:undefined});await h.install();const session=await h.start();const failure=h.context.failVoiceSession(session,new Error('Connection lost'));await settle();await h.context.openThread('chat-B');h.state.error='';ended.reject(new Error('Old hangup failed'));await failure;assert.equal(h.state.open,false);assert.equal(h.node('voice-overlay').hidden,true);assert.equal(h.state.error,'');assert.equal(session.endError,'Old hangup failed');assert.equal(h.context.currentThread,'chat-B');
});

test('failed native close preserves the exact transcript and cancels closing until an explicit retry succeeds',async()=>{
  let failures=2;const h=harness({respond:method=>method==='voice/end'&&failures-->0?Promise.reject(new Error('Transcript store is unavailable')):undefined});await h.install();const session=await h.start();
  h.event(item('user-1','user',null));h.event({type:'conversation.item.input_audio_transcription.delta',item_id:'user-1',delta:'Preserve this unfinished message'});
  await h.context.__requestClose();assert.equal(h.closed,undefined);assert.deepEqual(h.sent,[{__window:'closeCancelled'}]);assert.equal(h.state.unsaved.get(session.id),session);assert.equal(h.state.open,true);assert.equal(h.node('voice-overlay').hidden,false);assert.equal(h.node('voice-save-retry').textContent,'Retry save and close');
  const messages=plain(session.finalMessages);assert.deepEqual(messages,[{id:'user-1',role:'user',text:'Preserve this unfinished message',interrupted:true}]);assert.equal(h.node('voice-transcript-items').children[0].children[1].textContent,messages[0].text);
  await assert.rejects(h.context.retryVoiceSave(),/store is unavailable/);assert.equal(h.closed,undefined);assert.deepEqual(plain(session.finalMessages),messages);assert.equal(h.state.unsaved.size,1);
  await h.node('voice-save-retry').onclick();assert.equal(h.closed,true);assert.equal(h.state.unsaved.size,0);assert.equal(h.mediaCalls.length,1,'retry saving must not record again');assert.deepEqual(h.methods('voice/end').map(call=>call.params.messages),[messages,messages,messages]);
});

test('navigation save failure remains reviewable and blocks a new call from replacing its transcript',async()=>{
  const ended=deferred();let first=true;const h=harness({respond:method=>method==='voice/end'&&first?(first=false,ended.promise):undefined});await h.install();const session=await h.start();h.event(item('user-1','user',null));h.event(final('user-1','user','Keep the original chat transcript'));
  await h.context.openThread('chat-B');const start=h.context.startVoiceChat();await settle();assert.equal(h.mediaCalls.length,1);assert.equal(h.state.last,session);ended.reject(new Error('Disk write failed'));await start;assert.equal(h.state.current,null);assert.equal(h.state.last,session);assert.equal(h.state.unsaved.get(session.id),session);assert.equal(h.state.open,true);assert.equal(h.context.currentThread,'chat-B');
  await h.node('voice-close').onclick();assert.equal(h.state.open,false);assert.equal(h.node('voice-recovery-banner').hidden,false);assert.match(h.node('voice-recovery-summary').textContent,/chat-A.*Disk write failed/);assert.equal(h.node('voice-recovery-review').hidden,false);
  await h.node('voice-recovery-review').onclick();assert.equal(h.state.open,true);assert.equal(h.node('voice-transcript-items').children[0].children[1].textContent,'Keep the original chat transcript');
  await h.node('voice-save-retry').onclick();assert.equal(h.state.unsaved.size,0);assert.equal(h.mediaCalls.length,1);assert.equal(h.context.currentThread,'chat-B');assert.equal(h.methods('voice/end')[1].params.id,session.id);assert.equal(h.methods('voice/end')[1].params.messages[0].text,'Keep the original chat transcript');
  const next=await h.start();assert.equal(next.threadId,'chat-B');assert.notEqual(next.id,session.id);await h.context.endVoiceChat({refresh:false});
});

test('explicit discard reviews the original content and requires a second concrete action before closing',async()=>{
  let fail=true;const h=harness({respond:method=>method==='voice/end'&&fail?(fail=false,Promise.reject(new Error('Cannot write transcript'))):undefined});await h.install();const session=await h.start();h.event(item('user-1','user',null));h.event(final('user-1','user','This message will be discarded'));
  await h.context.__requestClose();assert.equal(h.closed,undefined);assert.equal(h.node('voice-discard-confirm').hidden,true);assert.equal(h.node('voice-transcript-items').children[0].children[1].textContent,'This message will be discarded');
  await h.node('voice-discard').onclick();assert.equal(h.closed,undefined);assert.equal(h.methods('voice/end').length,1);assert.equal(h.node('voice-discard-confirm').hidden,false);assert.equal(h.node('voice-discard-confirm').textContent,'Discard 1 unsaved message and close');assert.equal(session.items.get('user-1').text,'This message will be discarded');
  await h.node('voice-discard-cancel').onclick();assert.equal(h.node('voice-discard-confirm').hidden,true);assert.equal(h.closed,undefined);
  await h.node('voice-discard').onclick();await h.node('voice-discard-confirm').onclick();assert.equal(h.closed,true);assert.equal(h.state.unsaved.size,0);assert.deepEqual(h.methods('voice/end')[1].params.messages,[]);
});

test('every successful account change stops the old API-key call and cancels pending dictation',async()=>{
  const h=harness();await h.install();const session=await h.start();h.event(item('user-1','user',null));h.event(final('user-1','user','Old account transcript'));h.context.handleServerNotification('account/updated',{account:{type:'apiKey'}});assert.equal(h.state.current,null);assert.equal(h.peers[0].closeCount,1);assert.equal(h.streams[0].getTracks()[0].readyState,'ended');await settle();assert.equal(h.methods('voice/end')[0].params.id,session.id);assert.equal(h.methods('voice/end')[0].params.messages[0].text,'Old account transcript');
  await h.context.startDictation();h.context.handleServerNotification('account/updated',{account:{type:'apiKey'}});await settle();assert.equal(h.state.dictation,null);assert.equal(h.streams.at(-1).getTracks()[0].readyState,'ended');assert.equal(h.methods('voice/transcribe').length,0);
});

test('a matching server-ended notification stops media and saves partial text while an unrelated call does nothing',async()=>{
  const h=harness();await h.install();const session=await h.start();h.event(item('user-1','user',null));h.event({type:'conversation.item.input_audio_transcription.delta',item_id:'user-1',delta:'Server interruption partial'});
  h.context.handleServerNotification('voice/status',{id:'another-call',status:'ended'});assert.equal(h.state.current,session);assert.equal(h.peers[0].closeCount,undefined);
  h.context.handleServerNotification('voice/status',{id:session.id,status:'ended'});assert.equal(h.state.current,null);assert.equal(h.peers[0].closeCount,1);assert.equal(h.streams[0].getTracks()[0].readyState,'ended');await settle();assert.deepEqual(h.methods('voice/end')[0].params.messages,[{id:'user-1',role:'user',text:'Server interruption partial',interrupted:true}]);
});

test('an already acknowledged transcript save stays saved if refreshing the chat UI fails',async()=>{
  const h=harness();await h.install();await h.start();h.event(item('user-1','user',null));h.event(final('user-1','user','Acknowledged text'));h.openFailure='Chat refresh failed';await h.context.endVoiceChat();assert.equal(h.state.unsaved.size,0);assert.equal(h.state.last.saved[0].text,'Acknowledged text');assert.ok(h.errors.includes('Chat refresh failed'));assert.equal(h.methods('voice/end').length,1);await h.context.__requestClose();assert.equal(h.closed,true);assert.equal(h.sent.length,0);
});

test('opening voice moves focus into the modal, excludes the app and restores the launcher on close',async()=>{
  const h=harness();await h.install();const launcher=h.node('voice-button'),app=h.node('app'),document=h.context.document;launcher.focus();await h.context.openVoiceOverlay();
  assert.equal(document.activeElement,h.node('voice-start'));assert.equal(app.inert,true);assert.equal(app.getAttribute('aria-hidden'),'true');assert.equal(document.body.inert,false);assert.equal(h.mediaCalls.length,0);
  h.node('composer-input').focus();assert.equal(document.activeElement,h.node('voice-start'),'inert background cannot receive focus');
  await h.node('voice-close').onclick();assert.equal(app.inert,false);assert.equal(app.getAttribute('aria-hidden'),null);assert.equal(document.activeElement,launcher);assert.equal(h.state.modal,null);
});

test('unavailable voice focuses Connect and reports an unconnected state',async()=>{
  const h=harness({available:false});await h.install();h.node('voice-button').focus();await h.context.openVoiceOverlay();assert.equal(h.context.document.activeElement,h.node('voice-connect'));assert.equal(h.node('voice-status').textContent,'Voice not connected');assert.equal(h.mediaCalls.length,0);
  await h.node('voice-connect').onclick();assert.equal(h.settings,'account');assert.equal(h.node('app').inert,false);assert.equal(h.node('app').getAttribute('aria-hidden'),null);
});

test('Tab enters the modal from an outside focus and wraps both visible boundaries',async()=>{
  const h=harness();await h.install();await h.context.openVoiceOverlay();const document=h.context.document,options=h.context.voiceFocusOptions(),first=options[0],last=options.at(-1);
  const key=shiftKey=>{const event={key:'Tab',shiftKey,preventDefault(){this.prevented=true;},stopPropagation(){}};h.listeners.get('document:keydown')(event);return event;};
  document.activeElement=h.node('voice-button');assert.equal(key(false).prevented,true);assert.equal(document.activeElement,first);assert.equal(key(true).prevented,true);assert.equal(document.activeElement,last);assert.equal(key(false).prevented,true);assert.equal(document.activeElement,first);
  document.activeElement=h.node('composer-input');assert.equal(key(true).prevented,true);assert.equal(document.activeElement,last);await h.context.dismissVoiceOverlay();
});

test('capability and transcript renders preserve a valid focus instead of repeatedly choosing the primary action',async()=>{
  const h=harness();await h.install();await h.context.openVoiceOverlay();const choice=h.node('voice-choice');choice.focus();const initial=h.focusCalls.length;
  h.context.renderVoiceUi();h.context.renderVoiceUi();await h.context.refreshVoiceCapabilities();assert.equal(h.context.document.activeElement,choice);assert.equal(h.focusCalls.length,initial);
  await h.start();const mute=h.node('voice-mute');mute.focus();const listening=h.focusCalls.length;for(const type of ['input_audio_buffer.speech_started','input_audio_buffer.speech_stopped','output_audio_buffer.started','output_audio_buffer.stopped'])h.event({type});h.context.renderVoiceUi();assert.equal(h.context.document.activeElement,mute);assert.equal(h.focusCalls.length,listening);await h.context.endVoiceChat({refresh:false});
});

test('modal isolation restores previous app attributes and never changes a native close body lock',async()=>{
  const h=harness();await h.install();const app=h.node('app'),document=h.context.document;app.inert=true;app.setAttribute('aria-hidden','false');await h.context.openVoiceOverlay();assert.equal(app.inert,true);assert.equal(app.getAttribute('aria-hidden'),'true');
  document.body.inert=true;const count=h.focusCalls.length;await h.context.dismissVoiceOverlay();assert.equal(document.body.inert,true);assert.equal(app.inert,true);assert.equal(app.getAttribute('aria-hidden'),'false');assert.equal(h.focusCalls.length,count);document.body.inert=false;await settle();
});

test('a cancelled native close focuses recovery only after the native body lock is released',async()=>{
  const h=harness({respond:method=>method==='voice/end'?Promise.reject(new Error('Write failed')):undefined});await h.install();h.node('voice-button').focus();await h.start();const document=h.context.document,oldFocus=document.activeElement;document.body.inert=true;await h.context.__requestClose();assert.equal(h.closed,undefined);assert.equal(document.body.inert,true);assert.equal(h.node('app').inert,true);assert.equal(h.node('app').getAttribute('aria-hidden'),'true');assert.equal(h.state.open,true);
  document.body.inert=false;oldFocus.focus();await settle();assert.equal(document.activeElement,h.node('voice-save-retry'));assert.equal(h.node('voice-save-retry').textContent,'Retry save and close');assert.equal(document.body.inert,false);assert.deepEqual(h.sent,[{__window:'closeCancelled'}]);
});
