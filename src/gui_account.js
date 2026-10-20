// Account views consume public metadata only. Credentials are sent to Rust once.
const accountUi = { metadata:null, request:0, busy:false, keyForm:false };

function accountName(metadata) {
  const account=metadata?.account;
  return account?.email || (account?.type==='chatgpt'?'ChatGPT':account?.type==='apiKey'?'OpenAI API':'Mini ChatGPT');
}
function updateAccountChrome(metadata) {
  accountUi.metadata=metadata;
  $('account-label').textContent=accountName(metadata);
  $('account-sub').textContent=metadata?.account?'Settings':'Sign in to send messages';
  $('account-avatar').textContent=metadata?.account?.email?.charAt(0).toUpperCase()||'M';
  const railAccount=document.querySelector('.rail-account');if(railAccount)railAccount.textContent=$('account-avatar').textContent;
}
async function refreshAccount() {
  const metadata=await rpcCall('account/read');updateAccountChrome(metadata);return metadata;
}
function accountButton(label,run,className='btn-pill') {
  const button=document.createElement('button');button.type='button';button.className=className;button.textContent=label;
  button.onclick=async()=>{if(accountUi.busy)return;accountUi.busy=true;button.disabled=true;try{await run();}catch(error){showError(error);}finally{accountUi.busy=false;button.disabled=false;}};return button;
}
function accountSourceText(metadata) {
  return {environment:'This connection uses OPENAI_API_KEY from the app environment. Change it where the app is launched.',local:'This app stores your connection on this computer.',imported:'This app uses an existing Codex connection. Signing out here leaves the original connection unchanged.'}[metadata.credentialSource]||'Sign in to send messages and run tasks.';
}
async function renderAccountSettings() {
  const ticket=++accountUi.request;
  const metadata=await refreshAccount();
  if(ticket!==accountUi.request||currentSettingsSection!=='account')return;
  const box=$('settings-body');box.replaceChildren();
  const title=document.createElement('h1');title.className='settings-h1';title.textContent='Account';box.append(title);
  const card=document.createElement('div');card.className='settings-card account-card';box.append(card);
  const name=document.createElement('strong');name.textContent=metadata.account?accountName(metadata):'Welcome to Mini ChatGPT';card.append(name);
  const source=document.createElement('p');source.className='settings-sub';source.textContent=accountSourceText(metadata);card.append(source);
  const pending=metadata.login?.status==='pending'||metadata.login?.status==='waiting';
  if(pending){
    const state=document.createElement('p');state.className='account-pending';state.setAttribute('role','status');state.textContent='Finish signing in in your browser. This page will update when you return.';card.append(state);
    if(metadata.login.authUrl){const link=document.createElement('a');link.className='btn-pill primary account-browser-link';link.textContent='Continue in browser';link.href=metadata.login.authUrl;link.target='_blank';link.rel='noopener noreferrer';card.append(link);}
    card.append(accountButton('Cancel sign in',async()=>{await rpcCall('account/login/cancel',{loginId:metadata.login.id});await renderAccountSettings();}));
  } else if(metadata.canChangeCredentials!==false){
    const actions=document.createElement('div');actions.className='account-actions';
    actions.append(accountButton(metadata.account?'Switch ChatGPT account':'Sign in with ChatGPT',async()=>{
      await rpcCall('account/login/start',{type:'chatgpt'});accountUi.keyForm=false;await renderAccountSettings();
    },'btn-pill primary'));
    actions.append(accountButton(metadata.account?'Change API key':'Use an API key',async()=>{accountUi.keyForm=!accountUi.keyForm;await renderAccountSettings();}));card.append(actions);
    if(accountUi.keyForm){
      const form=document.createElement('form');form.className='account-key-form';form.autocomplete='off';
      const label=document.createElement('label');label.htmlFor='account-api-key';label.textContent='API key';
      const input=document.createElement('input');input.id='account-api-key';input.type='password';input.autocomplete='new-password';input.spellcheck=false;input.className='modal-input';input.placeholder='Enter your API key';input.required=true;input.maxLength=16384;
      const save=document.createElement('button');save.type='submit';save.className='btn-pill primary';save.textContent='Connect';
      const note=document.createElement('small');note.className='account-note';note.textContent='API usage is billed by your provider. A ChatGPT subscription does not include API usage.';
      form.append(label,input,note,save);
      form.onsubmit=async event=>{event.preventDefault();if(accountUi.busy)return;const apiKey=input.value.trim();if(!apiKey)return;accountUi.busy=true;save.disabled=true;try{await rpcCall('account/login/start',{type:'apiKey',apiKey});input.value='';accountUi.keyForm=false;await renderAccountSettings();toast('API key connected');}catch(error){showError(error);}finally{accountUi.busy=false;save.disabled=false;}};
      card.append(form);input.focus();
    }
    if(metadata.account)card.append(accountButton('Sign out',async()=>confirmAccountLogout(),'btn-pill danger'));
  }
  if(metadata.login?.error){const error=document.createElement('p');error.className='account-error';error.setAttribute('role','alert');error.textContent=metadata.login.error;card.append(error);}
}
function confirmAccountLogout() {
  openCustomModal('Sign out?', '<p>Running responses will stop and active goals will pause. Your local chats and documents will stay on this computer.</p><div id="account-signout-actions" class="account-actions"></div>');
  $('account-signout-actions').append(accountButton('Cancel',async()=>closeModal()),accountButton('Sign out',async()=>{await rpcCall('account/logout');closeModal();accountUi.keyForm=false;await renderAccountSettings();toast('Signed out');},'btn-pill danger'));
}
const accountBaseSettings=renderSettingsSection;
renderSettingsSection=async function(section){
  accountUi.request++;
  if(section!=='account')return accountBaseSettings(section);
  currentSettingsSection=section;document.querySelectorAll('.settings-nav-btn').forEach(button=>button.classList.toggle('active',button.dataset.sec===section));
  await renderAccountSettings();
};
const accountBaseNotification=handleServerNotification;
handleServerNotification=function(method,params){
  if(method==='account/updated'){refreshAccount().then(()=>{if(currentSettingsSection==='account'&&document.body.classList.contains('in-settings'))renderAccountSettings().catch(showError);}).catch(showError);return;}
  if(method==='account/login/completed'){
    refreshAccount().then(()=>{if(currentSettingsSection==='account'&&document.body.classList.contains('in-settings'))renderAccountSettings().catch(showError);}).catch(showError);
    if(params.success)toast('Signed in');else if(params.error&&!params.cancelled)showError(new Error(params.error));return;
  }
  accountBaseNotification(method,params);
};
const accountBaseErrorBanner=appendErrorBanner;
appendErrorBanner=function(message){
  accountBaseErrorBanner(message);
  if(/not signed in|authentication required/i.test(message))$('messages').lastElementChild?.append(accountButton('Sign in',async()=>openSettingsView('account')));
};
const accountReady=desktopReady.then(async()=>{
  const pill=document.querySelector('.account-pill');pill.onclick=()=>openSettingsView('account');pill.setAttribute('role','button');pill.tabIndex=0;setupAccountKeydown(pill);
  await refreshAccount();
}).catch(showError);
function setupAccountKeydown(pill){pill.onkeydown=event=>{if(event.key==='Enter'||event.key===' '){event.preventDefault();openSettingsView('account').catch(showError);}};}
