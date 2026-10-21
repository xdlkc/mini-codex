#!/usr/bin/env python3
"""Force a real source metadata rename failure after Git and chat handoff.

Only disposable profile files change. No model, account or remote service is used.
"""
from __future__ import annotations
import argparse,importlib.util,json,sys,tempfile
from pathlib import Path
sys.dont_write_bytecode=True
def module(name,file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file));value=importlib.util.module_from_spec(spec);sys.modules[name]=value;spec.loader.exec_module(value);return value
f=module('handoff_failure_transport','smoke-desktop.py');git_fixture=module('handoff_failure_git','smoke-workspaces.py')

def run(binary,timeout):
    checks=0;rpcs=0
    def check(value,message):
        nonlocal checks
        f.require(value,message);checks+=1
    with tempfile.TemporaryDirectory(prefix='.mcodex-handoff-failure-',dir=Path.cwd()) as temporary:
        root=Path(temporary).resolve();repo=root/'repo';profile=root/'profile';profile.mkdir();git_fixture.repository(repo);git_fixture.git(repo,'switch','-c','feat/archive-failure');(repo/'mixed.txt').write_bytes(b'preserve after archive failure\n')
        app=f.AppServer(binary,profile,repo,timeout)
        try:
            app.initialize(profile);thread=app.rpc('thread/start',{'cwd':str(repo),'mode':'codex'})['thread']['id'];app.rpc('thread/name/set',{'threadId':thread,'name':'Archive failure fixture'})
            metadata=profile/'session-metadata'/(thread.encode().hex()+'.json');original=metadata.read_bytes();metadata.unlink();metadata.mkdir()
            try:
                result=app.rpc('thread/handoff',{'threadId':thread,'direction':'toWorktree','worktreeBranch':'feat/archive-failure','localBranch':'main','operationId':'archive-failure-fixture'})
            finally:
                metadata.rmdir();metadata.write_bytes(original)
            target=result['thread']['id'];path=Path(result['handoff']['targetPath']);warnings=result['handoff']['warnings']
            check(target!=thread and result['handoff']['ownerThreadId']==target,'archive failure lost the new chat or committed owner')
            check(any('original chat could not be archived' in item for item in warnings),'the real archive error did not produce a warning')
            check((path/'mixed.txt').read_bytes()==b'preserve after archive failure\n' and git_fixture.git(repo,'branch','--show-current').strip()==b'main','archive failure reverted the successful Git move')
            check(not app.rpc('thread/read',{'threadId':thread})['thread']['archived'],'the failed source archive was reported as successful')
            check(app.rpc('thread/handoff/status',{'threadId':thread,'operationId':'archive-failure-fixture'})['operation']['warnings']==warnings,'status recovery lost the archive warning');rpcs+=app.rpc_count
        finally:app.close()
        app=f.AppServer(binary,profile,repo,timeout)
        try:
            app.initialize(profile);record=app.rpc('thread/handoff/status',{'threadId':thread,'operationId':'archive-failure-fixture'})['operation']
            check(record['status']=='committed' and record['ownerThreadId']==target and record['warnings']==warnings,'restart lost the committed result or warning')
            check(app.rpc('thread/read',{'threadId':target})['thread']['cwd']==str(path),'the destination chat was unavailable after restart');rpcs+=app.rpc_count
        finally:app.close()
        print(f'handoff-failure smoke: {checks} checks, {rpcs} real RPCs; filesystem archive failure and restart; no model calls; temporary data removed')

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=Path('target/debug/mcodex'));parser.add_argument('--timeout',type=float,default=30);args=parser.parse_args();run(args.binary.resolve(),args.timeout)
