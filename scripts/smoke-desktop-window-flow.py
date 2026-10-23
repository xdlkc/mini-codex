#!/usr/bin/env python3
"""Real browser + Rust + model loopback: automatic titles and a bottom PTY dock."""
import argparse,importlib.util,json,os,re,shlex,subprocess,sys,tempfile,time,uuid
from pathlib import Path
sys.dont_write_bytecode=True
ROOT=Path(__file__).resolve().parent.parent
spec=importlib.util.spec_from_file_location('window_flow_mock',ROOT/'scripts/smoke-turns.py');f=importlib.util.module_from_spec(spec);sys.modules[spec.name]=f;spec.loader.exec_module(f)
def run(binary,browser,screenshot):
 session='desktop-flow-'+uuid.uuid4().hex[:12]
 def ab(*args):
  p=subprocess.run([*browser,'--session',session,'--json',*args],text=True,capture_output=True,timeout=45);assert p.returncode==0,(args,p.stderr,p.stdout);r=json.loads(p.stdout);assert r['success'],r;return r.get('data',{})
 def js(code):return ab('eval',code).get('result')
 def wait(condition):
  end=time.monotonic()+30
  while time.monotonic()<end:
   if js('Boolean('+condition+')'):return
   time.sleep(.05)
  raise AssertionError((condition,ab('snapshot','-i')))
 with tempfile.TemporaryDirectory(prefix='mini-codex-window-flow-') as d:
  root=Path(d);cwd=root/'workspace';cwd.mkdir();home=root/'profile';home.mkdir();(cwd/'README.md').write_text('# Bottom terminal fixture\n')
  (home/'desktop.json').write_text(json.dumps({'version':1,'threadMetadata':{},'projects':{},'documents':{},'settings':{'mode':'codex','preferences':{'codexLanguage':'en','theme':'light'}}}))
  shell=root/'fixture-shell';shell.write_text('#!/bin/bash\nexport PS1="qa $ "\nunset PROMPT_COMMAND\nexec /bin/bash --noprofile --norc -i\n');shell.chmod(0o755)
  mock=f.MockResponses(25);mock.plan('success','你好，终端布局已经就绪。');mock.plan('success',json.dumps({'title':'添加底部会话终端','note':'你好'},ensure_ascii=False))
  env={k:v for k,v in os.environ.items() if not k.lower().endswith('_proxy') and not k.startswith(('OPENAI_','MCODEX_'))};env.update(MCODEX_HOME=str(home),MCODEX_NO_CODEX_IMPORT='1',OPENAI_API_KEY='local-test-only',OPENAI_BASE_URL=mock.base_url,NO_PROXY='127.0.0.1,localhost',SHELL=str(shell))
  with tempfile.TemporaryFile(mode='w+t') as log:
   process=subprocess.Popen([str(binary),'--model','local-mock-model','-C',str(cwd),'preview','--port','0'],env=env,cwd=cwd,stdout=log,stderr=log)
   try:
    end=time.monotonic()+25
    while True:
     log.seek(0);match=re.search(r'Desktop preview: (http://127\.0\.0\.1:\d+)',log.read())
     if match:break
     assert process.poll() is None and time.monotonic()<end,'Preview did not start';time.sleep(.05)
    ab('open',match[1]);wait("document.querySelector('#composer-input') && document.querySelector('#desktop-mode-name')?.textContent==='Codex'")
    ab('fill','#composer-input','请添加会话底部终端，保持文件面板可见');ab('click','#submit-btn')
    title='添加底部会话终端';wait("document.getElementById('topbar-title').textContent==="+json.dumps(title)+" && document.title==="+json.dumps(title)+" && document.querySelector('#thread-list').innerText.includes("+json.dumps(title)+')')
    first=mock.request();naming=mock.request();assert first.get('tools') and not naming.get('tools');assert js("desktop.tabs.find(t=>t.id===currentThread).title")==title
    ab('click','[title="Browse workspace files"]');wait("!document.getElementById('inspector-drawer').classList.contains('closed') && document.getElementById('drawer-content').innerText.includes('README.md')")
    ab('click','[title="Quick shell command"]');wait("document.querySelector('#codex-terminal-dock .xterm-helper-textarea') && !document.getElementById('codex-terminal-dock').hidden")
    actual_cwd=js('currentCwd');assert Path(actual_cwd).resolve()==cwd.resolve(),{'actual':actual_cwd,'expected':str(cwd.resolve())}
    geometry=js("(()=>{const dock=document.getElementById('codex-terminal-dock').getBoundingClientRect(),composer=document.getElementById('composer-host').getBoundingClientRect(),main=document.getElementById('main-col').getBoundingClientRect();return {dockTop:dock.top,dockBottom:dock.bottom,composerBottom:composer.bottom,mainBottom:main.bottom,sideOpen:!document.getElementById('inspector-drawer').classList.contains('closed'),tab:currentDrawerTab};})()")
    assert geometry['sideOpen'] and geometry['tab']=='files' and geometry['dockTop']>=geometry['composerBottom']-.5 and geometry['dockBottom']<=geometry['mainBottom']+1,geometry
    ab('click','#codex-terminal-dock .xterm-helper-textarea');ab('keyboard','inserttext',"export TERM_PANEL_TEST=preserved; printf 'BOTTOM_TERMINAL_OK\\n' > terminal-proof.txt");ab('press','Enter')
    end=time.monotonic()+20
    while not (cwd/'terminal-proof.txt').exists():assert time.monotonic()<end,'Terminal command did not execute';time.sleep(.05)
    assert (cwd/'terminal-proof.txt').read_text()=='BOTTOM_TERMINAL_OK\n'
    ab('click','[aria-label="Close terminal panel"]');assert js("document.getElementById('codex-terminal-dock').hidden");assert js("!document.getElementById('inspector-drawer').classList.contains('closed')")
    ab('click','[title="Quick shell command"]');wait("!document.getElementById('codex-terminal-dock').hidden && document.querySelector('#codex-terminal-dock .xterm-helper-textarea')")
    ab('click','#codex-terminal-dock .xterm-helper-textarea');ab('keyboard','inserttext',"printf '%s' \"$TERM_PANEL_TEST\" > restore-proof.txt");ab('press','Enter')
    end=time.monotonic()+20
    while not (cwd/'restore-proof.txt').exists():assert time.monotonic()<end,'Terminal restore failed';time.sleep(.05)
    assert (cwd/'restore-proof.txt').read_text()=='preserved'
    ab('click','.desktop-thread-menu');ab('find','role','button','click','--name','Rename','--exact');ab('fill','#rename-input','自定义会话标题');ab('click','#rename-save-btn');wait("document.getElementById('topbar-title').textContent==='自定义会话标题' && document.title==='自定义会话标题'")
    ab('click','[data-rail-view="more"]');wait("document.getElementById('settings-view').classList.contains('open')");ab('click','.settings-back-btn');wait("document.getElementById('topbar-title').textContent==='自定义会话标题' && document.title==='自定义会话标题' && !document.getElementById('codex-terminal-dock').hidden && document.querySelector('[data-rail-view=home]').getAttribute('aria-current')==='page'")
    ab('click','#codex-terminal-dock .xterm-helper-textarea');ab('keyboard','inserttext',"printf '%s' \"$TERM_PANEL_TEST\" > after-settings-proof.txt");ab('press','Enter')
    end=time.monotonic()+20
    while not (cwd/'after-settings-proof.txt').exists():assert time.monotonic()<end,'Terminal input stayed bound to the old navigation';time.sleep(.05)
    assert (cwd/'after-settings-proof.txt').read_text()=='preserved'
    ab('click','[title="Browse workspace files"]');wait("!document.getElementById('inspector-drawer').classList.contains('closed')")
    if screenshot:screenshot.parent.mkdir(parents=True,exist_ok=True);ab('screenshot',str(screenshot))
    assert mock.requests.empty() and not mock.errors,mock.errors
    print('Desktop flow passed: model-generated title synchronizes header/sidebar/tab/document title; manual rename; terminal below composer with Files visible; real command; panel reopen preserves the same shell; no external model calls')
   finally:
    subprocess.run([*browser,'--session',session,'close'],capture_output=True,timeout=20);process.terminate()
    try:process.wait(timeout=5)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
    mock.close()
if __name__=='__main__':
 p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,default=ROOT/'target/debug/mcodex');p.add_argument('--browser',default='agent-browser');p.add_argument('--screenshot',type=Path);a=p.parse_args();run(a.binary.resolve(),shlex.split(a.browser),a.screenshot.resolve() if a.screenshot else None)
