// Voice owns media only after a user action. Rust owns credentials and call signaling.
const voiceUi={capabilities:null,capabilityRequest:0,current:null,last:null,ending:new Set(),unsaved:new Map(),recoveryId:null,discardConfirm:false,closeRequested:false,open:false,modal:null,error:'',selectedVoice:'marin',dictation:null,serial:0};
const voiceIcons={mic:'<rect x="9" y="2" width="6" height="12" rx="3"/><path d="M5 10v2a7 7 0 0 0 14 0v-2M12 19v3m-4 0h8"/>',voice:'<path d="M4 10v4m4-8v12m4-15v18m4-15v12m4-8v4"/>'};
function voiceSvg(name){return `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" aria-hidden="true">${voiceIcons[name]}</svg>`;}
function voiceButton(id,label,action,className='btn-pill'){
  const button=document.createElement('button');button.id=id;button.type='button';button.className=className;button.textContent=label;
  button.onclick=()=>Promise.resolve().then(action).catch(error=>{voiceUi.error=voiceError(error);renderVoiceUi();});return button;
}
function voiceError(error){return ({NotAllowedError:'Microphone access was denied. Allow access, then try again.',NotFoundError:'No microphone was found.',NotReadableError:'The microphone is in use or unavailable.'})[error?.name]||String(error?.message||error||'Voice could not connect').slice(0,500);}
function voiceFocusOptions(){return Array.from($('voice-overlay')?.querySelectorAll('button,select,summary')||[]).filter(node=>!node.disabled&&!node.hidden&&node.tabIndex!==-1&&node.getClientRects().length);}
function focusVoicePrimary(){
  if(!voiceUi.open||document.body.inert)return;
  const options=voiceFocusOptions();
  (['voice-save-retry','voice-start','voice-connect','voice-end','voice-close'].map(id=>$(id)).find(node=>options.includes(node))||options[0]||$('voice-overlay'))?.focus();
}
function syncVoiceModal(){
  const dialog=$('voice-overlay');if(!dialog)return;
  if(voiceUi.open){
    if(!voiceUi.modal){
      const app=$('app'),active=document.activeElement;
      voiceUi.modal={app,inert:Boolean(app?.inert),ariaHidden:app?.getAttribute('aria-hidden')??null,returnFocus:active&&active!==document.body&&!dialog.contains(active)?active:$('voice-button')||$('composer-input')};
      // Move focus before hiding the app from assistive technology.
      focusVoicePrimary();if(app){app.inert=true;app.setAttribute('aria-hidden','true');}
    }else if(!document.body.inert){
      const active=document.activeElement;
      if(!dialog.contains(active)||active.disabled||active.hidden||!active.getClientRects().length)focusVoicePrimary();
    }
  }else if(voiceUi.modal){
    const previous=voiceUi.modal;voiceUi.modal=null;
    if(previous.app){previous.app.inert=previous.inert;if(previous.ariaHidden===null)previous.app.removeAttribute('aria-hidden');else previous.app.setAttribute('aria-hidden',previous.ariaHidden);}
    const target=previous.returnFocus;
    // Native close owns body.inert. It restores focus after its own handshake.
    if(!document.body.inert&&target?.isConnected!==false&&!target?.disabled&&!target?.hidden&&target?.getClientRects?.().length)target.focus();
  }
}
async function refreshVoiceCapabilities(){
  const ticket=++voiceUi.capabilityRequest;
  try{const result=await rpcCall('voice/capabilities');if(ticket===voiceUi.capabilityRequest){voiceUi.capabilities=result;renderVoiceUi();}return result;}
  catch(error){if(ticket===voiceUi.capabilityRequest){voiceUi.capabilities={available:false,reason:voiceError(error)};renderVoiceUi();}return voiceUi.capabilities;}
}
function voiceIsCurrent(session){return voiceUi.current===session&&!session.ended&&session.navigation===desktop.navigation&&session.mode===desktop.mode&&session.threadId===currentThread;}
function voiceOwnsSurface(session){return (!voiceUi.current||voiceUi.current===session)&&session.navigation===desktop.navigation&&session.mode===desktop.mode&&session.threadId===currentThread;}
function voiceTrackStop(stream){for(const track of stream?.getTracks?.()||[]){try{track.stop();}catch{}}}
function releaseVoiceMedia(session){
  if(session.resourcesStopped)return;session.resourcesStopped=true;
  if(session.channel){session.channel.onopen=session.channel.onmessage=session.channel.onerror=session.channel.onclose=null;try{session.channel.close();}catch{}}
  if(session.pc){session.pc.ontrack=session.pc.onconnectionstatechange=session.pc.oniceconnectionstatechange=null;try{session.pc.close();}catch{}}
  const tracks=new Set([...(session.stream?.getTracks?.()||[]),...Array.from(session.remoteStreams).flatMap(stream=>stream.getTracks?.()||[])]);
  for(const track of tracks){try{track.stop();}catch{}}
  if(session.audio){try{session.audio.pause();session.audio.srcObject=null;session.audio.remove();}catch{}}
  session.channel=null;session.pc=null;session.stream=null;session.remoteStreams.clear();
}
function voiceMessageNode(session,id){
  if(!id||typeof id!=='string')return null;
  if(!session.items.has(id))session.items.set(id,{id,role:null,text:'',final:false,interrupted:false,orderKnown:false,previous:null,responseId:null});
  return session.items.get(id);
}
function voiceSaved(session,id){return session.saved.find(message=>message.id===id);}
function voiceItemText(item){
  return (item.content||[]).map(content=>typeof content.transcript==='string'?content.transcript:typeof content.text==='string'?content.text:'').join('');
}
function voiceOrderedItems(session){
  const result=[],seen=new Set();let previous=null;
  while(true){const next=Array.from(session.items.values()).filter(item=>item.orderKnown&&item.previous===previous&&!seen.has(item.id));if(next.length!==1)break;const item=next[0];result.push(item);seen.add(item.id);previous=item.id;}
  return result;
}
function voiceStableMessages(session,ending=false){
  const result=[],ordered=voiceOrderedItems(session);let bytes=0;
  const boundary=ending?ordered.length-1:ordered.reduce((last,item,index)=>session.flushIds.has(item.id)?index:last,-1);
  for(const [index,item] of ordered.entries()){
    if(index>boundary)break;
    if(!['user','assistant'].includes(item.role))continue;
    const saved=voiceSaved(session,item.id);if(saved){result.push({...saved});continue;}
    if(!ending&&!item.final)break;
    if(!item.text.trim())continue;
    if(new TextEncoder().encode(item.text).length>65536)throw new Error('A voice transcript exceeds the message limit. End this call and start another.');
    result.push({id:item.id,role:item.role,text:item.text,interrupted:Boolean(item.interrupted||(!item.final&&ending))});
  }
  if(result.length>1000)throw new Error('This voice chat reached its transcript limit. Start another call.');
  for(const message of result)bytes+=new TextEncoder().encode(message.text).length;
  if(bytes>4*1024*1024)throw new Error('This voice chat reached its transcript limit. Start another call.');
  for(let index=0;index<session.saved.length;index++)if(JSON.stringify(result[index])!==JSON.stringify(session.saved[index]))throw new Error('Voice transcript order changed after it was saved. End this call to preserve the saved messages.');
  return result;
}
function syncVoiceTranscript(session){
  if(!session.flushRequested||session.ended)return session.sync;
  session.sync=session.sync.catch(()=>{}).then(async()=>{
    if(session.ended)return;
    const messages=voiceStableMessages(session);if(messages.length===session.saved.length)return;
    await rpcCall('voice/transcript',{id:session.id,messages});session.saved=messages.map(message=>({...message}));
  });
  session.sync.catch(error=>{if(!session.ended){voiceUi.error=voiceError(error);renderVoiceUi();}});return session.sync;
}
function receiveVoiceEvent(session,event){
  if(session.ended||!event||typeof event.type!=='string')return;
  const type=event.type,id=event.item_id||event.item?.id;
  if(type==='conversation.item.created'||type==='conversation.item.added'){
    const node=voiceMessageNode(session,event.item?.id);if(!node)return;
    if(Object.hasOwn(event,'previous_item_id')){node.orderKnown=true;node.previous=event.previous_item_id??null;}
    else if(Object.hasOwn(event.item,'previous_item_id')){node.orderKnown=true;node.previous=event.item.previous_item_id??null;}
    if(['user','assistant'].includes(event.item.role))node.role=event.item.role;
    if(!voiceSaved(session,node.id)){
      const text=voiceItemText(event.item);if(text)node.text=text;
      if((event.item.content||[]).some(content=>['text','input_text','output_text'].includes(content.type)&&typeof content.text==='string'))node.final=true;
    }
  } else if(type==='response.output_item.added'||type==='response.output_item.done'){
    const node=voiceMessageNode(session,event.item?.id);if(node){node.responseId=event.response_id||node.responseId;if(event.item.role==='assistant')node.role='assistant';}
  } else if(['conversation.item.input_audio_transcription.delta','response.audio_transcript.delta','response.output_audio_transcript.delta','response.text.delta','response.output_text.delta'].includes(type)){
    const node=voiceMessageNode(session,id);if(node&&!voiceSaved(session,id)){node.role=type.startsWith('conversation.')?'user':'assistant';node.text+=event.delta||'';node.responseId=event.response_id||node.responseId;}
  } else if(['conversation.item.input_audio_transcription.completed','response.audio_transcript.done','response.output_audio_transcript.done','response.text.done','response.output_text.done'].includes(type)){
    const node=voiceMessageNode(session,id);if(node&&!voiceSaved(session,id)){node.role=type.startsWith('conversation.')?'user':'assistant';node.text=event.transcript??event.text??node.text;node.final=true;node.responseId=event.response_id||node.responseId;}
  } else if(type==='conversation.item.input_audio_transcription.failed'){
    const node=voiceMessageNode(session,id);if(node&&!voiceSaved(session,id)){node.role='user';node.final=true;node.interrupted=true;}voiceUi.error='An input transcript could not be completed.';
  } else if(type==='input_audio_buffer.speech_started'){session.userSpeaking=true;session.waiting=false;}
  else if(type==='input_audio_buffer.speech_stopped'){session.userSpeaking=false;session.waiting=true;}
  else if(type==='response.created'){session.waiting=true;session.responseId=event.response?.id||null;}
  else if(type==='output_audio_buffer.started'){session.outputPlaying=true;session.waiting=false;}
  else if(type==='output_audio_buffer.stopped'){session.outputPlaying=false;}
  else if(type==='output_audio_buffer.cleared'){session.outputPlaying=false;markVoiceInterrupted(session);}
  else if(type==='conversation.item.truncated'){const node=session.items.get(id);if(node&&!voiceSaved(session,id))node.interrupted=true;}
  else if(type==='response.done'){
    session.waiting=false;
    if(event.response?.status==='cancelled')markVoiceInterrupted(session,event.response.id);
    if(event.response?.status==='failed'){voiceUi.error=String(event.response.status_details?.error?.message||'The voice response failed. Try speaking again.').slice(0,500);}
    for(const item of event.response?.output||[]){const node=voiceMessageNode(session,item.id);if(!node)continue;node.responseId=event.response.id;if(item.role==='assistant')node.role='assistant';if(!voiceSaved(session,node.id)){const text=voiceItemText(item);if(text)node.text=text;if((item.content||[]).some(content=>typeof content.transcript==='string'||typeof content.text==='string'))node.final=true;}}
    session.flushRequested=true;for(const item of session.items.values())session.flushIds.add(item.id);
  } else if(type==='error'){failVoiceSession(session,new Error(event.error?.message||'The voice service reported an error.'));return;}
  renderVoiceUi();if(session.flushRequested)syncVoiceTranscript(session);
}
function markVoiceInterrupted(session,responseId=null){for(const item of session.items.values())if(item.role==='assistant'&&!voiceSaved(session,item.id)&&(!responseId||item.responseId===responseId))item.interrupted=true;}
function toggleVoiceMute(){const session=voiceUi.current;if(!session||session.ended)return;session.muted=!session.muted;for(const track of session.stream?.getAudioTracks?.()||[])track.enabled=!session.muted;renderVoiceUi();}
function interruptVoiceResponse(){
  const session=voiceUi.current;if(!session||session.channel?.readyState!=='open')return;
  session.channel.send(JSON.stringify({type:'response.cancel'}));session.channel.send(JSON.stringify({type:'output_audio_buffer.clear'}));markVoiceInterrupted(session);renderVoiceUi();
}
async function openVoiceOverlay(){
  voiceUi.open=true;voiceUi.error='';mountVoiceOverlay();renderVoiceUi();await refreshVoiceCapabilities();
}
async function startVoiceChat(){
  if(voiceUi.current&&!voiceUi.current.ended)return;
  if(voiceUi.ending.size)await Promise.allSettled(Array.from(voiceUi.ending,session=>session.ending));
  if(voiceUi.current&&!voiceUi.current.ended)return;
  if(voiceUi.unsaved.size){showVoiceRecovery(voiceRecoverySession());return;}
  cancelDictation(true);
  const session={id:window.crypto?.randomUUID?.()||('voice-'+Date.now()+'-'+(++voiceUi.serial)),threadId:currentThread,mode:desktop.mode,navigation:desktop.navigation,ended:false,resourcesStopped:false,stream:null,remoteStreams:new Set(),pc:null,channel:null,audio:null,items:new Map(),saved:[],sync:Promise.resolve(),flushRequested:false,flushIds:new Set(),connected:false,channelOpen:false,userSpeaking:false,outputPlaying:false,waiting:false,muted:false,stage:'Checking connection',startedAt:null};
  voiceUi.current=session;voiceUi.last=session;voiceUi.error='';voiceUi.open=true;mountVoiceOverlay();renderVoiceUi();
  try{
    const capability=await refreshVoiceCapabilities();if(!voiceIsCurrent(session)){await endVoiceChat({session,refresh:false});return;}
    if(!capability?.available)throw new Error(capability?.reason||'Connect an API key to use voice.');
    if(isTurnActive)throw new Error('Wait for the current response to finish before starting voice.');
    if(typeof scheduledUi!=='undefined'&&scheduledUi.goal?.status==='active')throw new Error('Pause the active goal before starting voice.');
    if(desktop.threads?.find(thread=>thread.id===currentThread)?.readOnly)throw new Error('Open a copy of this imported chat before starting voice.');
    if(!navigator.mediaDevices?.getUserMedia||typeof RTCPeerConnection!=='function')throw new Error('This window does not support microphone access and WebRTC.');
    session.stage='Allow microphone access';renderVoiceUi();
    const stream=await navigator.mediaDevices.getUserMedia({audio:true});
    if(!voiceIsCurrent(session)){voiceTrackStop(stream);await endVoiceChat({session,refresh:false});return;}
    session.stream=stream;
    if(!session.threadId){
      saveDraft();const result=await rpcCall('thread/start',{mode:session.mode,cwd:currentCwd||undefined,model:currentModel,projectId:desktop.projectId});
      if(!voiceIsCurrent(session)){await endVoiceChat({session,refresh:false});return;}
      session.threadId=result.thread.id;currentThread=session.threadId;updateCwdUI(result.cwd||currentCwd);addTab(result.thread);refreshThreads().catch(showError);
    }
    const pc=new RTCPeerConnection();session.pc=pc;
    const audio=document.createElement('audio');audio.autoplay=true;audio.playsInline=true;audio.hidden=true;document.body.append(audio);session.audio=audio;
    pc.ontrack=event=>{
      const streams=event.streams?.length?event.streams:[new MediaStream([event.track])];
      if(session.ended){for(const stream of streams)voiceTrackStop(stream);return;}
      for(const stream of streams)session.remoteStreams.add(stream);audio.srcObject=streams[0];
      Promise.resolve(audio.play()).then(()=>{session.audioBlocked=false;renderVoiceUi();}).catch(()=>{session.audioBlocked=true;renderVoiceUi();});
    };
    pc.onconnectionstatechange=()=>{
      if(session.ended)return;
      if(pc.connectionState==='connected'){session.connected=true;session.startedAt??=Date.now();}
      if(['failed','disconnected','closed'].includes(pc.connectionState)){failVoiceSession(session,new Error('The voice connection ended. You can try again.'));return;}
      renderVoiceUi();
    };
    for(const track of stream.getAudioTracks())pc.addTrack(track,stream);
    const channel=pc.createDataChannel('oai-events');session.channel=channel;
    channel.onopen=()=>{session.channelOpen=true;renderVoiceUi();};
    channel.onmessage=event=>{try{receiveVoiceEvent(session,JSON.parse(event.data));}catch(error){failVoiceSession(session,error);}};
    channel.onerror=()=>failVoiceSession(session,new Error('The voice data connection failed.'));
    channel.onclose=()=>{if(!session.ended)failVoiceSession(session,new Error('The voice data connection closed.'));};
    session.stage='Connecting';renderVoiceUi();
    const offer=await pc.createOffer();if(!voiceIsCurrent(session)){await endVoiceChat({session,refresh:false});return;}
    await pc.setLocalDescription(offer);if(!voiceIsCurrent(session)){await endVoiceChat({session,refresh:false});return;}
    const result=await rpcCall('voice/start',{id:session.id,threadId:session.threadId,sdp:pc.localDescription?.sdp||offer.sdp,model:capability.defaultModel||'gpt-realtime-2.1',voice:voiceUi.selectedVoice});
    if(!voiceIsCurrent(session)){await endVoiceChat({session,refresh:false}).catch(()=>{});rpcCall('voice/end',{id:session.id}).catch(()=>{});return;}
    await pc.setRemoteDescription({type:'answer',sdp:result.sdp});
    if(!voiceIsCurrent(session)){await endVoiceChat({session,refresh:false});return;}session.stage='Connecting';renderVoiceUi();
  }catch(error){if(!session.ended)await failVoiceSession(session,error);}
}
function failVoiceSession(session,error){const relevant=voiceUi.current===session;const message=voiceError(error);if(relevant)voiceUi.error=message;return endVoiceChat({session,keepOpen:relevant,error:relevant?message:'',refresh:false}).catch(showError);}
function voiceRecoverySession(){return voiceUi.unsaved.get(voiceUi.recoveryId)||voiceUi.unsaved.values().next().value||null;}
function voiceUnsavedCount(session){return session?Math.max(0,(session.finalMessages||voiceOrderedItems(session).filter(item=>['user','assistant'].includes(item.role)&&item.text.trim())).length-session.saved.length):0;}
function showVoiceRecovery(session){if(!session)return;voiceUi.recoveryId=session.id;voiceUi.discardConfirm=false;voiceUi.open=true;mountVoiceOverlay();renderVoiceUi();}
function rememberVoiceSaveFailure(session,error){
  session.endError=voiceError(error);voiceUi.unsaved.set(session.id,session);
  if(voiceUi.closeRequested||voiceOwnsSurface(session))showVoiceRecovery(session);else renderVoiceUi();
}
function forgetVoiceSaveFailure(session){voiceUi.unsaved.delete(session.id);session.endError=null;if(voiceUi.recoveryId===session.id)voiceUi.recoveryId=null;voiceUi.discardConfirm=false;}
function dismissVoiceOverlay(){
  if(voiceUi.unsaved.size){voiceUi.closeRequested=false;voiceUi.open=false;renderVoiceUi();return Promise.resolve();}
  return endVoiceChat();
}
async function finishVoiceClose(){
  if(voiceUi.unsaved.size)return;
  voiceUi.closeRequested=false;voiceUi.open=false;renderVoiceUi();return voiceBaseRequestClose?.();
}
async function retryVoiceSave({startAfter=false}={}){
  const session=voiceRecoverySession();if(!session)return;
  if(session.retrying)return session.retrying;
  let messages;try{messages=session.projectionError?voiceStableMessages(session,true):(session.finalMessages||session.saved);}catch(error){rememberVoiceSaveFailure(session,error);throw error;}
  const request=(async()=>{
    let result;try{result=await rpcCall('voice/end',{id:session.id,messages});}catch(error){rememberVoiceSaveFailure(session,error);throw error;}
    session.cleanupAck=true;session.saved=messages;forgetVoiceSaveFailure(session);voiceUi.error='';renderVoiceUi();
    if(voiceUi.unsaved.size){showVoiceRecovery(voiceRecoverySession());return;}
    if(voiceUi.closeRequested)return await finishVoiceClose();
    if(startAfter){voiceUi.ending.delete(session);return await startVoiceChat();}return result;
  })();session.retrying=request;voiceUi.ending.add(session);session.ending=request;
  request.finally(()=>{session.retrying=null;voiceUi.ending.delete(session);renderVoiceUi();}).catch(()=>{});renderVoiceUi();return request;
}
async function discardVoiceTranscript(){
  const session=voiceRecoverySession();if(!session||session.retrying||session.discarding)return;
  if(!voiceUi.discardConfirm){voiceUi.discardConfirm=true;renderVoiceUi();return;}
  // The user has reviewed this transcript and explicitly chosen to discard its unsaved tail.
  session.finalMessages=session.saved.map(message=>({...message}));session.projectionError=null;session.items.clear();
  for(const [index,message] of session.saved.entries())session.items.set(message.id,{...message,final:true,orderKnown:true,previous:session.saved[index-1]?.id??null});
  session.discarding=true;renderVoiceUi();
  try{if(!session.cleanupAck)await rpcCall('voice/end',{id:session.id,messages:session.finalMessages});forgetVoiceSaveFailure(session);voiceUi.error='';}
  catch(error){if(!voiceUi.closeRequested){rememberVoiceSaveFailure(session,error);throw error;}forgetVoiceSaveFailure(session);}
  finally{session.discarding=false;}
  renderVoiceUi();if(voiceUi.unsaved.size)showVoiceRecovery(voiceRecoverySession());else if(voiceUi.closeRequested)await finishVoiceClose();
}
function endVoiceChat({session=voiceUi.current,keepOpen=false,error='',refresh=true}={}){
  if(!session){if(!keepOpen)voiceUi.open=false;renderVoiceUi();return Promise.resolve();}
  if(session.ending){if(!keepOpen)voiceUi.open=false;renderVoiceUi();return session.ending;}
  session.ended=true;releaseVoiceMedia(session);if(voiceUi.current===session)voiceUi.current=null;voiceUi.last=session;
  if(error)voiceUi.error=error;if(!keepOpen)voiceUi.open=false;renderVoiceUi();
  session.ending=(async()=>{
    await session.sync.catch(()=>{});
    let messages,projectionFailure=null;try{messages=voiceStableMessages(session,true);}catch(failure){projectionFailure=failure;session.projectionError=voiceError(failure);messages=session.saved.map(item=>({...item}));}
    try{
      session.finalMessages=projectionFailure?null:messages;const result=await rpcCall('voice/end',{id:session.id,messages});session.cleanupAck=true;
      if(projectionFailure)throw projectionFailure;
      session.saved=messages;forgetVoiceSaveFailure(session);
      if(refresh&&session.threadId&&currentThread===session.threadId&&session.navigation===desktop.navigation){try{await voiceBaseOpenThread(result.threadId||session.threadId);}catch(failure){showError(failure);}}
      return result;
    }catch(failure){rememberVoiceSaveFailure(session,failure);throw failure;}
  })();voiceUi.ending.add(session);session.ending.finally(()=>voiceUi.ending.delete(session)).catch(()=>{});return session.ending;
}
async function retryVoiceChat(){
  if(voiceUi.unsaved.size)return retryVoiceSave({startAfter:true});
  await startVoiceChat();
}
function voiceStatus(session){
  if(!session&&voiceRecoverySession())return voiceUnsavedCount(voiceRecoverySession())?'Voice transcript not saved':'Voice cleanup needs retry';
  if(!session)return voiceUi.error?'Could not connect':voiceUi.capabilities?.available?'Ready to talk':voiceUi.capabilities?'Voice not connected':'Checking voice connection';
  if(!session.connected||!session.channelOpen)return session.stage;
  if(session.audioBlocked)return 'Audio playback is paused';
  if(session.userSpeaking)return session.muted?'Microphone muted':'You are speaking';
  if(session.outputPlaying)return 'Speaking';if(session.waiting)return 'Thinking';return session.muted?'Microphone muted':'Listening';
}
function mountVoiceOverlay(){
  if($('voice-overlay'))return;
  const dialog=document.createElement('section');dialog.id='voice-overlay';dialog.className='voice-overlay';dialog.hidden=true;dialog.tabIndex=-1;dialog.setAttribute('role','dialog');dialog.setAttribute('aria-modal','true');dialog.setAttribute('aria-labelledby','voice-dialog-title');
  dialog.innerHTML='<div class="voice-surface"><header class="voice-header"><h2 id="voice-dialog-title">Voice</h2><button id="voice-close" class="icon-btn" aria-label="Close voice">×</button></header><label class="voice-selector-label" for="voice-choice">Voice</label><select id="voice-choice" class="settings-select"></select><div class="voice-orb" id="voice-orb" aria-hidden="true"><span></span><span></span><span></span><span></span><span></span></div><p id="voice-status" class="voice-status" role="status" aria-live="polite"></p><p id="voice-error" class="voice-error" role="alert"></p><div class="voice-actions" id="voice-actions"></div><details class="voice-transcript" open><summary>Transcript</summary><ol id="voice-transcript-items"></ol></details></div>';
  document.body.append(dialog);
  $('voice-close').onclick=()=>dismissVoiceOverlay().catch(showError);
  $('voice-choice').onchange=event=>{voiceUi.selectedVoice=event.target.value;};
  $('voice-actions').append(voiceButton('voice-start','Start voice',startVoiceChat,'btn-pill primary'),voiceButton('voice-mute','Mute',toggleVoiceMute),voiceButton('voice-interrupt','Interrupt',interruptVoiceResponse),voiceButton('voice-play','Play audio',async()=>{const session=voiceUi.current;if(session?.audio){await session.audio.play();session.audioBlocked=false;renderVoiceUi();}}),voiceButton('voice-end','End voice',()=>endVoiceChat({keepOpen:true}),'btn-pill danger'),voiceButton('voice-retry','Try again',retryVoiceChat,'btn-pill primary'),voiceButton('voice-connect','Connect an API key',async()=>{await endVoiceChat({refresh:false});await openSettingsView('account');}),voiceButton('voice-copy-chat','Open a copy',async()=>{const result=await rpcCall('thread/fork',{threadId:currentThread});await openThread(result.thread.id);await openVoiceOverlay();}));
  $('voice-actions').append(voiceButton('voice-save-retry','Retry save',()=>retryVoiceSave(),'btn-pill primary'),voiceButton('voice-discard','Discard unsaved transcript',discardVoiceTranscript,'btn-pill danger'),voiceButton('voice-discard-confirm','Confirm discard',discardVoiceTranscript,'btn-pill danger'),voiceButton('voice-discard-cancel','Keep transcript',()=>{voiceUi.discardConfirm=false;renderVoiceUi();}));
  const banner=document.createElement('aside');banner.id='voice-recovery-banner';banner.hidden=true;banner.setAttribute('role','status');
  Object.assign(banner.style,{position:'fixed',right:'24px',bottom:'24px',zIndex:'135',maxWidth:'min(380px,80vw)',padding:'14px',borderRadius:'14px',background:'var(--bg-elevated)',color:'var(--text-primary)',border:'1px solid var(--border-subtle)',boxShadow:'0 4px 24px #0002'});
  const summary=document.createElement('p');summary.id='voice-recovery-summary';banner.append(summary,voiceButton('voice-recovery-review','Review unsaved voice transcript',()=>showVoiceRecovery(voiceRecoverySession())));document.body.append(banner);
}
function renderVoiceUi(){
  const dialog=$('voice-overlay');if(!dialog)return;
  dialog.hidden=!voiceUi.open;const session=voiceUi.current,recovery=voiceRecoverySession(),active=Boolean(session&&!session.ended),available=voiceUi.capabilities?.available===true;
  const unsavedCount=voiceUnsavedCount(recovery);
  const recoverySummary=recovery?(unsavedCount?`${unsavedCount} voice message${unsavedCount===1?'':'s'} for chat ${recovery.threadId||'(new chat)'} ${unsavedCount===1?'is':'are'} not saved. `:`Voice cleanup for chat ${recovery.threadId||'(new chat)'} did not finish. `)+(recovery.endError||'Retry before starting another call.'):'';
  $('voice-recovery-banner').hidden=!recovery||voiceUi.open;$('voice-recovery-summary').textContent=recoverySummary;
  $('voice-status').textContent=voiceStatus(session);$('voice-error').textContent=recoverySummary||voiceUi.error||(!available?(voiceUi.capabilities?.reason||'Connect an API key to use voice and dictation.'):'');
  const select=$('voice-choice');const voices=voiceUi.capabilities?.voices||['alloy','ash','ballad','coral','echo','sage','shimmer','verse','marin','cedar'];
  if(select.options?.length!==voices.length){select.replaceChildren();for(const voice of voices){const option=document.createElement('option');option.value=voice;option.textContent=voice.charAt(0).toUpperCase()+voice.slice(1);select.append(option);}}
  select.value=voiceUi.selectedVoice;select.disabled=active||Boolean(recovery);
  $('voice-start').hidden=active||Boolean(recovery)||Boolean(voiceUi.error)||!available;$('voice-retry').hidden=active||Boolean(recovery)||!voiceUi.error||!available;$('voice-connect').hidden=available;
  $('voice-save-retry').hidden=!recovery;$('voice-save-retry').disabled=Boolean(recovery?.retrying||recovery?.discarding);$('voice-save-retry').textContent=voiceUi.closeRequested?'Retry save and close':'Retry save';
  $('voice-discard').hidden=!recovery||voiceUi.discardConfirm;$('voice-discard').disabled=Boolean(recovery?.retrying||recovery?.discarding);
  $('voice-discard-confirm').hidden=!recovery||!voiceUi.discardConfirm;$('voice-discard-confirm').disabled=Boolean(recovery?.retrying||recovery?.discarding);$('voice-discard-confirm').textContent=`Discard ${unsavedCount} unsaved message${unsavedCount===1?'':'s'}${voiceUi.closeRequested?' and close':''}`;
  $('voice-discard-cancel').hidden=!recovery||!voiceUi.discardConfirm;
  $('voice-mute').hidden=!active;$('voice-mute').disabled=!session?.stream;$('voice-mute').textContent=session?.muted?'Unmute':'Mute';$('voice-mute').setAttribute('aria-pressed',String(Boolean(session?.muted)));
  $('voice-end').hidden=!active;$('voice-interrupt').hidden=!active;$('voice-interrupt').disabled=!session?.outputPlaying&&!session?.waiting;$('voice-play').hidden=!session?.audioBlocked;
  $('voice-copy-chat').hidden=!desktop.threads?.find(thread=>thread.id===currentThread)?.readOnly;
  $('voice-orb').classList.toggle('is-speaking',Boolean(session?.outputPlaying));$('voice-orb').classList.toggle('is-hearing',Boolean(session?.userSpeaking&&!session?.muted));
  const transcript=$('voice-transcript-items');transcript.replaceChildren();
  for(const item of voiceOrderedItems(session||recovery||voiceUi.last||{items:new Map()})){if(!item.text.trim())continue;const row=document.createElement('li'),label=document.createElement('strong'),text=document.createElement('span');label.textContent=item.role==='user'?'You':'ChatGPT';text.textContent=item.text;row.append(label,text);if(!item.final)row.classList.add('voice-transcript-partial');transcript.append(row);}
  const mic=$('dictation-button');if(mic){const recording=voiceUi.dictation?.status==='recording';mic.disabled=active||['starting','transcribing'].includes(voiceUi.dictation?.status);mic.setAttribute('aria-pressed',String(recording));mic.title=recording?'Stop dictation':'Dictate a message';}
  if($('dictation-status'))$('dictation-status').textContent=voiceUi.dictation?.status==='recording'?'Recording…':voiceUi.dictation?.status==='transcribing'?'Transcribing…':'';
  if($('dictation-cancel'))$('dictation-cancel').hidden=!voiceUi.dictation;
  syncVoiceModal();
}

function voiceBlobBase64(blob){return new Promise((resolve,reject)=>{const reader=new FileReader();reader.onload=()=>resolve(String(reader.result).split(',')[1]||'');reader.onerror=()=>reject(new Error('Could not read recorded audio'));reader.readAsDataURL(blob);});}
async function startDictation(){
  if(voiceUi.dictation?.status==='recording')return stopDictation(false);
  if(['starting','transcribing'].includes(voiceUi.dictation?.status))return;
  if(voiceUi.current)await endVoiceChat({refresh:false});
  const input=$('composer-input'),session={id:++voiceUi.serial,key:currentThread||desktop.mode,navigation:desktop.navigation,mode:desktop.mode,threadId:currentThread,original:input.value,start:input.selectionStart??input.value.length,end:input.selectionEnd??input.value.length,status:'starting',stream:null,recorder:null,chunks:[],bytes:0,cancelled:false,finished:false};voiceUi.dictation=session;renderVoiceUi();
  try{
    const capability=await refreshVoiceCapabilities();if(voiceUi.dictation!==session||session.cancelled)return;
    if(!capability?.available)throw new Error(capability?.reason||'Connect an API key to dictate.');
    if(!navigator.mediaDevices?.getUserMedia||typeof MediaRecorder!=='function')throw new Error('This window does not support audio recording.');
    const stream=await navigator.mediaDevices.getUserMedia({audio:true});
    if(session.cancelled||voiceUi.dictation!==session||session.navigation!==desktop.navigation){voiceTrackStop(stream);return;}
    session.stream=stream;
    const mime=['audio/webm;codecs=opus','audio/webm','audio/mp4'].find(type=>MediaRecorder.isTypeSupported?.(type));
    const recorder=new MediaRecorder(stream,mime?{mimeType:mime}:undefined);session.recorder=recorder;session.mime=recorder.mimeType||mime||'audio/webm';
    recorder.ondataavailable=event=>{if(!event.data?.size||session.cancelled)return;session.bytes+=event.data.size;if(session.bytes>10*1024*1024){cancelDictation(true);showError(new Error('Recorded audio exceeds 10 MiB'));return;}session.chunks.push(event.data);};
    recorder.onerror=()=>{cancelDictation(true);showError(new Error('Audio recording failed'));};
    recorder.onstop=()=>completeDictation(session).catch(showError);
    recorder.start(1000);session.status='recording';renderVoiceUi();
  }catch(error){voiceTrackStop(session.stream);session.cancelled=true;if(voiceUi.dictation===session)voiceUi.dictation=null;renderVoiceUi();showError(error);}
}
function stopDictation(discard=false){
  const session=voiceUi.dictation;if(!session)return;
  if(discard)session.cancelled=true;
  voiceTrackStop(session.stream);
  if(session.recorder&&session.recorder.state!=='inactive'){try{session.recorder.stop();}catch{}}
  else if(!session.recorder&&discard){if(voiceUi.dictation===session)voiceUi.dictation=null;}
  if(discard&&voiceUi.dictation===session)voiceUi.dictation=null;
  renderVoiceUi();return session;
}
function cancelDictation(discard=true){return stopDictation(discard);}
async function completeDictation(session){
  if(session.finished)return;session.finished=true;voiceTrackStop(session.stream);
  if(session.cancelled){if(voiceUi.dictation===session)voiceUi.dictation=null;renderVoiceUi();return;}
  session.status='transcribing';renderVoiceUi();
  try{
    const blob=new Blob(session.chunks,{type:session.mime});if(!blob.size)throw new Error('No audio was recorded.');
    const dataBase64=await voiceBlobBase64(blob);if(session.cancelled)return;
    const mime=session.mime.split(';')[0],filename=mime==='audio/mp4'?'dictation.m4a':'dictation.webm';
    const result=await rpcCall('voice/transcribe',{dataBase64,mimeType:session.mime,filename},60000);if(session.cancelled)return;
    const text=typeof result.text==='string'?result.text.trim():'';if(!text)return;
    const same=session.navigation===desktop.navigation&&(currentThread||desktop.mode)===session.key;
    const previous=same?$('composer-input').value:(desktop.drafts.get(session.key)??session.original);
    const inserted=previous===session.original?previous.slice(0,session.start)+text+previous.slice(session.end):previous+(previous&&!/\s$/.test(previous)?' ':'')+text;
    desktop.drafts.set(session.key,inserted);
    if(same){$('composer-input').value=inserted;autoResizeInput();updateSubmitButton();saveDraft();$('composer-input').focus();}
    else toast('Dictation saved to the original draft');
    await savePreferences({drafts:Object.fromEntries(desktop.drafts)});
  }catch(error){if(!session.cancelled)showError(error);}
  finally{if(voiceUi.dictation===session)voiceUi.dictation=null;renderVoiceUi();}
}
function voiceNavigationCleanup(){
  voiceUi.closeRequested=false;
  if(voiceUi.dictation&&voiceUi.dictation.status!=='transcribing')cancelDictation(true);
  endVoiceChat({refresh:false}).catch(showError);voiceUi.open=false;renderVoiceUi();
}
const voiceBaseOpenThread=openThread;
for(const name of ['newThread','openThread','switchMode','openSettingsView','showLibrary','showFilesLibrary','openScheduledView','openProject','openDocument','openSearch','useWorkspace']){
  const base=window[name];if(typeof base==='function')window[name]=async function(...args){voiceNavigationCleanup();return base(...args);};
}
const voiceBaseSend=sendTurn;
sendTurn=async function(...args){if(voiceUi.current&&!voiceUi.current.ended)throw new Error('End voice chat before sending a text message.');return voiceBaseSend(...args);};
const voiceBaseNotification=handleServerNotification;
handleServerNotification=function(method,params){
  if(method==='account/updated'){cancelDictation(true);endVoiceChat({refresh:false}).catch(()=>{});refreshVoiceCapabilities().catch(()=>{});}
  if(method==='voice/status'&&params.status==='ended'&&params.id===voiceUi.current?.id){endVoiceChat({refresh:false}).catch(()=>{});}
  voiceBaseNotification(method,params);
};
const voiceBaseRequestClose=window.__requestClose;
window.__requestClose=async function(){
  voiceUi.closeRequested=true;cancelDictation(true);const ending=endVoiceChat({refresh:false});
  const results=await Promise.allSettled([ending,...Array.from(voiceUi.ending,session=>session.ending)]);
  if(voiceUi.unsaved.size||results.some(result=>result.status==='rejected')){rpcSend({__window:'closeCancelled'});showVoiceRecovery(voiceRecoverySession());return;}
  return finishVoiceClose();
};
window.addEventListener('pagehide',()=>{
  cancelDictation(true);const sessions=new Set([...voiceUi.ending,...voiceUi.unsaved.values()]);if(voiceUi.current)sessions.add(voiceUi.current);voiceUi.current=null;
  for(const session of sessions){
    session.ended=true;releaseVoiceMedia(session);
    let messages;try{messages=voiceStableMessages(session,true);}catch{messages=session.finalMessages||session.saved;}
    const request={id:'voice-exit-'+session.id,method:'voice/end',params:{id:session.id,messages}};
    if(window.ipc&&(typeof eventStream==='undefined'||!eventStream))rpcSend(request);else navigator.sendBeacon?.('/rpc',new Blob([JSON.stringify(request)],{type:'application/json'}));
  }
});
document.addEventListener('keydown',event=>{
  if(!voiceUi.open)return;
  if(event.key==='Escape'){event.preventDefault();event.stopPropagation();dismissVoiceOverlay().catch(showError);return;}
  if(event.key==='Tab'){
    const options=voiceFocusOptions(),first=options[0],last=options.at(-1);
    if(!options.includes(document.activeElement)){event.preventDefault();(event.shiftKey?last:first)?.focus();}
    else if(event.shiftKey&&document.activeElement===first){event.preventDefault();last?.focus();}else if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first?.focus();}
  }
},true);
document.addEventListener('focusin',event=>{if(voiceUi.open&&!document.body.inert&&!$('voice-overlay')?.contains(event.target))focusVoicePrimary();},true);
// A native close cancellation may release body.inert after the recovery dialog rendered.
if(typeof MutationObserver==='function')new MutationObserver(()=>{if(voiceUi.open&&!document.body.inert)syncVoiceModal();}).observe(document.body,{attributes:true,attributeFilter:['inert']});
const voiceReady=desktopReady.then(async()=>{
  mountVoiceOverlay();const group=document.createElement('div');group.className='voice-composer-controls';group.id='voice-composer-controls';
  const mic=voiceButton('dictation-button','',startDictation,'icon-btn');mic.innerHTML=voiceSvg('mic');mic.title='Dictate a message';mic.setAttribute('aria-label','Dictate a message');
  const call=voiceButton('voice-button','',openVoiceOverlay,'voice-entry');call.innerHTML=voiceSvg('voice')+'<span>Voice</span>';call.title='Start a voice chat';call.setAttribute('aria-label','Start a voice chat');
  group.append(mic,call);$('submit-btn').before(group);
  const status=document.createElement('span');status.id='dictation-status';status.className='dictation-status';status.setAttribute('role','status');
  const cancel=voiceButton('dictation-cancel','Cancel dictation',()=>cancelDictation(true),'dictation-cancel');cancel.hidden=true;group.append(status,cancel);
  await refreshVoiceCapabilities();renderVoiceUi();
}).catch(showError);
