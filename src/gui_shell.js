// Shared desktop navigation follows the app rail and workspace selector layout.
const shellIcons = {
  home:'<path d="m3 10 9-7 9 7v10a1 1 0 0 1-1 1h-5v-7H9v7H4a1 1 0 0 1-1-1z"/>',
  library:'<rect x="4" y="6" width="13" height="15" rx="2"/><path d="m8 6 1-3 12 4-4 13"/>',
  scheduled:'<circle cx="12" cy="12" r="9"/><path d="M12 6v6l-4 3"/>',
  git:'<circle cx="6" cy="5" r="2"/><circle cx="6" cy="19" r="2"/><circle cx="18" cy="6" r="2"/><path d="M6 7v10m12-9v2c0 4-6 2-9 6"/>',
  apps:'<rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/>',
  more:'<circle cx="5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/>',
  search:'<circle cx="10.5" cy="10.5" r="7"/><path d="m16 16 5 5"/>',
};
function shellSvg(name){return `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${shellIcons[name]||shellIcons.home}</svg>`;}
function syncHiddenPanels(){
  for(const [id,className] of [['sidebar','collapsed'],['inspector-drawer','closed']]){
    const panel=$(id),closed=panel.classList.contains(className);
    if(closed&&panel.contains(document.activeElement))(id==='sidebar'?$('topbar-sidebar-btn'):$('composer-input'))?.focus();
    panel.inert=closed;panel.setAttribute('aria-hidden',String(closed));
  }
}
function syncShellNavigation(){
  if(!$('desktop-app-rail'))return;
  const inSettings=document.body.classList.contains('in-settings'),inScheduled=document.body.classList.contains('in-scheduled');
  const selected=inSettings?(currentSettingsSection==='git'&&desktop.mode==='codex'?'git':currentSettingsSection==='skills'?'apps':currentSettingsSection==='account'?'account':'more'):inScheduled?'scheduled':desktop.library?'library':'home';
  document.querySelectorAll('[data-rail-view]').forEach(button=>{button.classList.toggle('active',button.dataset.railView===selected);if(button.dataset.railView===selected)button.setAttribute('aria-current','page');else button.removeAttribute('aria-current');});
  $('desktop-mode-name').textContent=desktop.mode==='chat'?'ChatGPT':titleCase(desktop.mode);
  $('desktop-mode-button').setAttribute('aria-label','Workspace: '+(desktop.mode==='chat'?'ChatGPT':titleCase(desktop.mode)));
}
function openDesktopModeMenu(event){
  event.stopPropagation();const button=$('desktop-mode-button');
  openFloatingMenuAt('desktop-mode',button,'',false);const menu=$('floating-menu');const open=menu.classList.contains('open');button.setAttribute('aria-expanded',String(open));if(!open)return;
  menu.setAttribute('role','menu');
  for(const [mode,label,description] of [['chat','ChatGPT','Ask questions and explore ideas'],['work','Work','Research, write and complete tasks'],['codex','Codex','Build and review code']]){
    const item=menuButton(label,async()=>{await switchMode(mode);button.focus();});item.classList.add('desktop-mode-option');item.setAttribute('role','menuitemradio');item.setAttribute('aria-checked',String(mode===desktop.mode));
    const text=document.createElement('span');const name=document.createElement('strong');name.textContent=label;const detail=document.createElement('small');detail.textContent=description;text.append(name,detail);item.replaceChildren(text);
    if(mode===desktop.mode){const check=document.createElement('span');check.textContent='✓';item.append(check);}menu.append(item);
  }
  const items=Array.from(menu.querySelectorAll('[role=menuitemradio]'));
  menu.onkeydown=key=>{const index=items.indexOf(document.activeElement);if(['ArrowDown','ArrowUp','Home','End'].includes(key.key)){key.preventDefault();items[key.key==='Home'?0:key.key==='End'?items.length-1:(index+(key.key==='ArrowDown'?1:items.length-1)+items.length)%items.length]?.focus();}if(key.key==='Escape'){key.preventDefault();closeFloatingMenu();button.focus();}};
  items.find(item=>item.getAttribute('aria-checked')==='true')?.focus();
}
const shellBaseCloseMenu=closeFloatingMenu;
closeFloatingMenu=function(){const wasMode=activeFloatingMenuKey==='desktop-mode';shellBaseCloseMenu();$('desktop-mode-button')?.setAttribute('aria-expanded','false');if(wasMode)$('floating-menu').onkeydown=null;};
const shellBaseModeUI=updateModeUI;
updateModeUI=function(){shellBaseModeUI();syncShellNavigation();};
const shellBaseNewThread=newThread;
newThread=async function(...args){const result=await shellBaseNewThread(...args);syncShellNavigation();return result;};
const shellBaseOpenThread=openThread;
openThread=async function(...args){const result=await shellBaseOpenThread(...args);syncShellNavigation();return result;};
const shellBaseLibrary=showLibrary;
showLibrary=async function(...args){const result=await shellBaseLibrary(...args);syncShellNavigation();return result;};
const shellBaseScheduled=openScheduledView;
openScheduledView=async function(...args){const result=await shellBaseScheduled(...args);syncShellNavigation();return result;};
const shellBaseSettings=openSettingsView;
openSettingsView=async function(...args){const result=await shellBaseSettings(...args);syncShellNavigation();return result;};
const shellReady=Promise.all([desktopReady,scheduledUiReady]).then(()=>{
  const rail=document.createElement('nav');rail.id='desktop-app-rail';rail.setAttribute('aria-label','App navigation');
  const drag=document.createElement('div');drag.className='rail-drag';drag.onmousedown=maybeDragWindow;rail.append(drag);
  const add=(view,label,action)=>{const button=document.createElement('button');button.className='rail-button';button.dataset.railView=view;button.title=label;button.setAttribute('aria-label',label);button.innerHTML=shellSvg(view);button.onclick=()=>Promise.resolve().then(action).catch(showError);rail.append(button);return button;};
  add('home','Home',()=>newThread());
  add('library','Library',()=>showLibrary());
  add('scheduled','Scheduled',()=>openScheduledView());
  add('git','Worktrees',()=>openSettingsView('git')).classList.add('codex-only');
  add('apps','Skills & plugins',()=>openSettingsView('skills'));
  add('more','Settings',()=>openSettingsView('general'));
  const spacer=document.createElement('div');spacer.className='rail-spacer';rail.append(spacer);
  const account=document.createElement('button');account.className='rail-account';account.dataset.railView='account';account.setAttribute('aria-label','Account');account.title='Account';account.textContent=$('account-avatar').textContent||'M';account.onclick=()=>openSettingsView('account').catch(showError);rail.append(account);
  $('app').prepend(rail);
  const header=document.createElement('div');header.className='desktop-mode-header';
  const mode=document.createElement('button');mode.id='desktop-mode-button';mode.className='desktop-mode-button';mode.setAttribute('aria-haspopup','menu');mode.setAttribute('aria-expanded','false');mode.innerHTML='<span id="desktop-mode-name"></span><span class="mode-chevron" aria-hidden="true">⌄</span>';mode.onclick=openDesktopModeMenu;mode.onmousedown=event=>event.stopPropagation();mode.onkeydown=event=>{if(event.key==='ArrowDown'){event.preventDefault();openDesktopModeMenu(event);}};
  const search=document.createElement('button');search.className='icon-btn';search.title='Search (⌘K)';search.setAttribute('aria-label','Search');search.innerHTML=shellSvg('search');search.onclick=()=>openSearch().catch(showError);header.append(mode,search);
  document.querySelector('.mode-switcher').before(header);document.body.classList.add('desktop-shell');syncShellNavigation();syncHiddenPanels();
  const panels=new MutationObserver(syncHiddenPanels);for(const id of ['sidebar','inspector-drawer'])panels.observe($(id),{attributes:true,attributeFilter:['class']});
}).catch(showError);
