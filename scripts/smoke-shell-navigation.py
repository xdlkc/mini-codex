#!/usr/bin/env python3
"""Real sidebar clicks against the Rust preview, using isolated state and no model calls."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import time
import uuid

ROOT=Path(__file__).resolve().parent.parent
spec=importlib.util.spec_from_file_location('rail_backend_fixture',ROOT/'scripts/smoke-desktop.py')
f=importlib.util.module_from_spec(spec);spec.loader.exec_module(f)

def run(binary,browser,screenshot):
    session='rail-qa-'+uuid.uuid4().hex[:12]
    def ab(*args):
        result=subprocess.run([*browser,'--session',session,'--json',*args],text=True,capture_output=True,timeout=40)
        assert result.returncode==0,(args,result.stderr,result.stdout)
        value=json.loads(result.stdout);assert value['success'],value
        return value.get('data',{})
    def js(expression):return ab('eval',expression).get('result')
    def wait(expression):
        deadline=time.monotonic()+25
        while time.monotonic()<deadline:
            if js('Boolean('+expression+')'):return
            time.sleep(.05)
        raise AssertionError((expression,ab('snapshot','-i')))
    with tempfile.TemporaryDirectory(prefix='mini-codex-rail-smoke-') as directory:
        root=Path(directory);home=root/'profile';home.mkdir();cwd=root/'workspace';cwd.mkdir()
        subprocess.run(['git','init','-q',str(cwd)],check=True)
        with f.AppServer(binary,home,cwd,30) as app:
            app.initialize(home);app.rpc('desktop/settings/update',{'mode':'codex','preferences':{'theme':'light','codexLanguage':'en'}})
            app.rpc('desktop/document/create',{'title':'Sidebar verification page','content':'# Navigation fixture\n','kind':'document'})
        env={**os.environ,'MCODEX_HOME':str(home),'MCODEX_NO_CODEX_IMPORT':'1','OPENAI_BASE_URL':'http://127.0.0.1:1'};env.pop('OPENAI_API_KEY',None)
        with tempfile.TemporaryFile(mode='w+t') as log:
            process=subprocess.Popen([str(binary),'-C',str(cwd),'preview','--port','0'],cwd=cwd,env=env,stdout=log,stderr=log)
            try:
                deadline=time.monotonic()+25
                while True:
                    log.seek(0);match=re.search(r'Desktop preview: (http://127\.0\.0\.1:\d+)',log.read())
                    if match:break
                    assert process.poll() is None and time.monotonic()<deadline,'Preview did not start'
                    time.sleep(.05)
                ab('open',match[1]);wait("document.querySelectorAll('[data-rail-view]').length===7")
                ab('fill','#composer-input','Navigation draft preserved')
                def state():return js("(()=>{const visible=id=>{const el=document.getElementById(id);return !!el&&el.getClientRects().length>0};const main=document.getElementById('main-col').getBoundingClientRect();return {mode:document.body.dataset.mode,selected:[...document.querySelectorAll('[data-rail-view][aria-current=page]')].map(el=>el.dataset.railView),visible:['home-hero','work-library','scheduled-view','settings-view'].filter(visible),main:[main.x,main.width],sidebar:document.getElementById('sidebar').getBoundingClientRect().width,worktrees:visible('sidebar')&&document.querySelector('[data-rail-view=git]').getClientRects().length>0}})()")
                baseline=state();checks=0
                views=[('library','work-library'),('scheduled','scheduled-view'),('git','settings-view'),('apps','settings-view'),('more','settings-view'),('account','settings-view'),('home','home-hero')]
                for _ in range(3):
                    for view,panel in views:
                        ab('click',f'[data-rail-view="{view}"]')
                        wait("document.querySelector('[data-rail-view=\""+view+"\"]').getAttribute('aria-current')==='page' && pendingRpc.size===0")
                        actual=state()
                        assert actual['mode']=='codex',actual
                        assert actual['selected']==[view] and actual['visible']==[panel],actual
                        assert actual['worktrees'] and abs(actual['sidebar']-baseline['sidebar'])<1,actual
                        assert all(abs(a-b)<1 for a,b in zip(actual['main'],baseline['main'])),actual
                        checks+=1
                assert ab('get','value','#composer-input')['value']=='Navigation draft preserved'
                for view in ['more','apps','scheduled','library','home','account','home']:ab('click',f'[data-rail-view="{view}"]')
                wait("document.querySelector('[data-rail-view=home]').getAttribute('aria-current')==='page' && pendingRpc.size===0")
                assert state()['visible']==['home-hero'] and state()['mode']=='codex'
                ab('click','[data-rail-view="library"]');wait("document.querySelector('[aria-label=\"Page: Sidebar verification page\"]') && pendingRpc.size===0")
                assert js("document.querySelectorAll('.library-header button').length") == 3,'Library tabs duplicated'
                if screenshot:screenshot.parent.mkdir(parents=True,exist_ok=True);ab('screenshot',str(screenshot))
                print(f'Sidebar smoke passed: {checks} full navigation checks, 7 rapid switches, preserved draft/mode/sidebar/main layout, single visible page and no duplicate Library actions; real Rust preview, no model calls')
            finally:
                subprocess.run([*browser,'--session',session,'close'],capture_output=True,timeout=20)
                process.terminate()
                try:process.wait(timeout=5)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=ROOT/'target/debug/mcodex');parser.add_argument('--browser',default='agent-browser');parser.add_argument('--screenshot',type=Path)
    args=parser.parse_args();run(args.binary.resolve(),shlex.split(args.browser),args.screenshot.resolve() if args.screenshot else None)
