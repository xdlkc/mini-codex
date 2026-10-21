#!/usr/bin/env python3
"""Real PTYs through JSONL RPCs, isolated Git, and no model calls."""
from __future__ import annotations
import argparse,importlib.util,json,os,subprocess,sys,tempfile,time
from pathlib import Path
sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location('terminal_smoke_fixture',Path(__file__).with_name('smoke-desktop.py'))
f=importlib.util.module_from_spec(spec);sys.modules[spec.name]=f;spec.loader.exec_module(f)

def run(binary,timeout):
    checks=0
    def check(value,message):
        nonlocal checks
        f.require(value,message);checks+=1
    with tempfile.TemporaryDirectory(prefix='.mcodex-terminal-smoke-',dir=Path.cwd()) as temporary:
        root=Path(temporary).resolve();cwd=root/'repo';cwd.mkdir();profile=root/'profile';profile.mkdir()
        shell=root/'fixture-shell';shell.write_text('#!/bin/bash\nexport PS1="__MC_PTY_READY__ "\nunset PROMPT_COMMAND\nexec /bin/bash --noprofile --norc -i\n');shell.chmod(0o755)
        def git(*args):
            return subprocess.run(['git','-C',str(cwd),'-c','core.hooksPath=/dev/null','-c','core.fsmonitor=false','-c','commit.gpgSign=false','-c','user.name=PTY fixture','-c','user.email=pty@example.invalid',*args],check=True,capture_output=True).stdout
        git('init','--template=','-b','main');(cwd/'tracked').write_text('base');git('add','tracked');git('commit','-m','fixture')
        previous=os.environ.get('SHELL');os.environ['SHELL']=str(shell)
        try:app=f.AppServer(binary,profile,cwd,timeout)
        finally:
            if previous is None:os.environ.pop('SHELL',None)
            else:os.environ['SHELL']=previous
        def wait_terminal(identifier,needle,previous_count=None):
            deadline=time.monotonic()+timeout
            while True:
                state=app.rpc('terminal/status',{'id':identifier})['terminal']
                app.rpc('terminal/ack',{'id':identifier,'sequence':state['sequence']})
                if needle in state['output'] and (previous_count is None or state['output'].count(needle)>previous_count):return state
                f.require(time.monotonic()<deadline,f'terminal did not display {needle!r}');time.sleep(.02)
        def write(identifier,text):return app.rpc('terminal/write',{'id':identifier,'data':text})
        try:
            app.initialize(profile);config=cwd/'.codex/environments/environment.toml';raw='version=1\nname="PTY fixture"\n[setup]\nscript="export MC_ENV_READY=ready"\n'
            app.rpc('workspace/environment/save',{'cwd':str(cwd),'configPath':str(config),'raw':raw,'expectedRevision':None})
            created=app.rpc('workspace/worktree/create',{'cwd':str(cwd),'localEnvironmentConfigPath':str(config)});target=Path(created['worktree']['cwd'])
            deadline=time.monotonic()+timeout
            while app.rpc('workspace/environment/status',{'id':created['setupRun']['id']})['run']['status']=='running':
                f.require(time.monotonic()<deadline,'setup timed out');time.sleep(.02)
            thread=app.rpc('thread/start',{'cwd':str(target),'mode':'codex'})['thread']['id'];other=app.rpc('thread/start',{'cwd':str(target),'mode':'codex'})['thread']['id']
            terminal=app.rpc('terminal/create',{'cwd':str(target),'threadId':thread,'title':'Interactive fixture','command':'printf "<ENV:%s>\\n" "$MC_ENV_READY"; test -t 0 && test -t 1 && printf "<TTY:%s>\\n" true'})['terminal'];identifier=terminal['id']
            check('<ENV:ready>' in wait_terminal(identifier,'<ENV:ready>')['output'],'setup environment did not reach the PTY');check('<TTY:true>' in wait_terminal(identifier,'<TTY:true>')['output'],'stdin/stdout are not terminals')
            check(write(identifier,'IFS= read -r answer; printf "<READ:%s>\\n" "$answer"\r中文\r')=={'ok':True},'input unexpectedly returned a full replay buffer');check('<READ:中文>' in wait_terminal(identifier,'<READ:中文>')['output'],'Unicode interactive input failed')
            app.rpc('terminal/resize',{'id':identifier,'cols':111,'rows':37});write(identifier,'printf "<SIZE:%s>\\n" "$(stty size)"; printf "\\033[31mCOLOR\\033[0m\\n"\r');check(wait_terminal(identifier,'<SIZE:37 111>')['cols']==111,'PTY dimensions did not change');check('\x1b[31mCOLOR\x1b[0m' in wait_terminal(identifier,'\x1b[31mCOLOR\x1b[0m')['output'],'ANSI sequences were stripped')
            second=app.rpc('terminal/create',{'cwd':str(target),'threadId':other})['terminal'];listing=app.rpc('terminal/list',{'cwd':str(target),'threadId':thread})['data'];check([s['id'] for s in listing]==[identifier],'terminal lists crossed chats')
            write(identifier,"/bin/bash -c 'printf started > started; exec sleep 30'\r");deadline=time.monotonic()+timeout
            while not (target/'started').exists():f.require(time.monotonic()<deadline,'foreground command did not start');time.sleep(.02)
            wait_terminal(identifier,'__MC_PTY_READY__ ')
            prompt_count=app.rpc('terminal/status',{'id':identifier})['terminal']['output'].count('__MC_PTY_READY__ ')
            write(identifier,'\x03');wait_terminal(identifier,'__MC_PTY_READY__ ',prompt_count);write(identifier,'printf "<AFTER:%s>\\n" ok\r');check('<AFTER:ok>' in wait_terminal(identifier,'<AFTER:ok>')['output'],'Ctrl+C did not leave the shell usable')
            try:app.rpc('workspace/worktree/archive',{'cwd':str(cwd),'id':created['worktree']['id']});raise AssertionError('an active terminal checkout was archived')
            except f.SmokeFailure as error:check('running responses' in str(error) and target.is_dir(),'archive did not respect the terminal checkout lease')
            write(identifier,"/bin/bash -c 'while :; do printf x >> live; sleep .02; done' & wait\r");deadline=time.monotonic()+timeout
            while not (target/'live').exists():f.require(time.monotonic()<deadline,'background writer did not start');time.sleep(.02)
            a=app.send('terminal/close',{'id':identifier});b=app.send('terminal/close',{'id':identifier})
            for request in [a,b]:
                value=app.wait(request)['terminal'];check(value is None or value['status']=='closed','a close returned before the terminal stopped')
            size=(target/'live').stat().st_size;time.sleep(.2);check((target/'live').stat().st_size==size,'terminal close left a shell job writing')
            app.rpc('terminal/close',{'id':second['id']});check(app.rpc('terminal/list',{'cwd':str(target)})['data']==[],'closed sessions remained listed')
            result=app.rpc('workspace/worktree/archive',{'cwd':str(cwd),'id':created['worktree']['id']});check(result['worktree']['state']=='archived' and not target.exists(),'terminal close did not release the checkout lease')
            print(f'terminal smoke: {checks} checks, {app.rpc_count} real RPCs; real PTYs, no model calls; temporary data removed')
        finally:app.close()

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=Path('target/debug/mcodex'));parser.add_argument('--timeout',type=float,default=30);args=parser.parse_args();run(args.binary.resolve(),args.timeout)
