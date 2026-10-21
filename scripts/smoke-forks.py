#!/usr/bin/env python3
"""Exercise fork history, real worktrees and rollback using isolated stores.

Only a loopback Responses mock is used. All Git operations and project data are
inside TemporaryDirectory; the current checkout and real credentials are unused.
"""
from __future__ import annotations
import argparse
import importlib.util
import json
from pathlib import Path
import sys
import tempfile

sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location('mcodex_fork_fixture',Path(__file__).with_name('smoke-workspaces.py'))
assert spec and spec.loader
fixture=importlib.util.module_from_spec(spec);sys.modules[spec.name]=fixture;spec.loader.exec_module(fixture)
require,git,MockResponses,AppServer=fixture.require,fixture.git,fixture.MockResponses,fixture.WorkspaceServer

def run(binary:Path,timeout:float)->None:
    checks=0;rpcs=0;requests=0
    def check(condition:bool,message:str)->None:
        nonlocal checks
        require(condition,message);checks+=1
    with tempfile.TemporaryDirectory(prefix='mcodex-forks-') as directory:
        root=Path(directory).resolve();source=root/'source';store=root/'state';store.mkdir()
        fixture.repository(source);fixture.populate_dirty_worktree(source)
        (store/'config.toml').write_text('model = "local-mock-model"\n')
        original=fixture.fingerprint(source);index=(source/'.git/index').read_bytes()
        with MockResponses(timeout) as mock:
            app=AppServer(binary,store,source/'nested',mock.base_url,timeout)
            try:
                app.rpc('initialize',{})
                project=app.rpc('desktop/project/create',{'mode':'codex','name':'Fork source','path':str(source)})['project']
                parent=app.rpc('thread/start',{'mode':'codex','projectId':project['id'],'cwd':str(source/'nested')})['thread']['id']
                response=mock.plan('success','你好，原聊天历史 🙂');turn=app.turn(parent);mock.request();requests+=1
                app.assert_chinese_completion(parent,turn,response.text)
                app.rpc('thread/name/set',{'threadId':parent,'name':'代码评审'})
                app.rpc('thread/goal/set',{'threadId':parent,'objective':'Source goal stays on its chat','status':'paused','tokenBudget':1000})
                app.rpc('desktop/threadMetadata/set',{'threadId':parent,'pinned':True,'webSearchMode':'disabled'})
                history=app.rpc('thread/read',{'threadId':parent,'includeTurns':True})['thread']['turns']
                rollout=(store/'sessions'/f'{parent}.jsonl').read_bytes()
                local=app.rpc('thread/fork',{'threadId':parent,'destination':'current','titleMode':'fork','reasoningEffort':'high'})['thread']
                check(local['id']!=parent and local['forkedFromId']==parent,'current fork lost its new identity or lineage')
                check(local['name']=='代码评审 (2)' and local['cwd']==str(source/'nested'),'current fork used the wrong title or folder')
                check(local['mode']=='codex' and local['projectId']==project['id'] and local['webSearchMode']=='disabled','current fork lost mode, project or search preference')
                check(local['turns']==history,'current fork did not preserve the completed history and UI snapshots')
                check(local.get('pinned') is False,'fork duplicated the source pin')
                check(app.rpc('thread/goal/get',{'threadId':local['id']}).get('goal') is None,'fork duplicated an active or paused goal')
                check(app.rpc('thread/goal/get',{'threadId':parent})['goal']['objective']=='Source goal stays on its chat','fork changed the source goal')
                prepared=app.rpc('thread/fork',{'threadId':parent,'destination':'newWorktree','titleMode':'fork','reasoningEffort':'high'})
                isolated=prepared['thread'];tree=prepared['worktree'];checkout=Path(tree['path'])
                check(isolated['name']=='代码评审 (3)' and isolated['forkedFromId']==parent,'new-worktree fork lost family numbering')
                check(Path(isolated['cwd'])==checkout/'nested','new-worktree fork lost the selected source subdirectory')
                check(tree['ownerThreadId']==isolated['id'] and tree['branch'] is None,'fork worktree is not detached or lost its owner')
                check(isolated['turns']==history,'new-worktree fork lost completed history')
                check(not git(checkout,'diff','--cached','--name-only'),'working-tree fork unexpectedly copied staging state')
                expected={name:value for name,value in fixture.files(source).items() if name!='ignored' and not name.startswith('ignored/')}
                # The reference stages tracked content in a private index and
                # applies its diff. Git attributes/execute bits apply; empty
                # directories and non-Git directory modes are not copied.
                expected.pop('empty directory',None)
                expected['private']=('directory',0o755)
                expected['run.sh']=('file',0o755,expected['run.sh'][2])
                expected['raw.crlf']=('file',expected['raw.crlf'][1],expected['raw.crlf'][2].replace(b'\r\n',b'\n'))
                actual=fixture.files(checkout)
                differences={name:(expected.get(name),actual.get(name)) for name in set(expected)|set(actual) if expected.get(name)!=actual.get(name)}
                check(not differences,f'fork did not match reference Git working-tree copy semantics: {differences!r}')
                check(fixture.fingerprint(source)==original and (source/'.git/index').read_bytes()==index,'fork changed source files, HEAD, branch or index bytes')
                check((store/'sessions'/f'{parent}.jsonl').read_bytes()==rollout,'fork appended to or rewrote the source rollout')
                app.rpc('thread/archive',{'threadId':local['id']})
                sibling=app.rpc('thread/fork',{'threadId':local['id'],'titleMode':'fork'})['thread']
                check(sibling['name']=='代码评审 (4)' and not sibling.get('archived'),'forking an archived descendant lost the family counter or copied its archived state')
                truncated=app.rpc('thread/fork',{'threadId':parent,'beforeUserMessage':0})['thread']
                check(not truncated['turns'],'prompt-edit fork retained messages after its cut point')
                defaults=app.rpc('thread/fork',{'threadId':parent,'destination':None,'model':None,'cwd':None,'reasoningEffort':None})['thread']
                check(defaults['cwd']==str(source/'nested') and defaults['turns']==history,'nullable fork options did not preserve their source defaults')
                # A foreign-key failure occurs after Git preparation. No rollout,
                # metadata entry or new worktree may survive this failed fork.
                state_path=store/'desktop.json';state=json.loads(state_path.read_text());broken=json.loads(json.dumps(state));broken['projects'].pop(project['id']);state_path.write_text(json.dumps(broken))
                sessions={p.name for p in (store/'sessions').iterdir()};worktrees=git(source,'worktree','list','--porcelain');registry=json.loads((store/'workspaces.json').read_text())
                notifications=len(app.notifications)
                app.expect_error('thread/fork',{'threadId':parent,'destination':'newWorktree','titleMode':'fork'},'projects item not found')
                check(any(e['method']=='thread/fork/progress' and e['params']['stage']=='copyingHistory' for e in app.notifications[notifications:]),'rollback fixture did not reach rollout copying')
                check({p.name for p in (store/'sessions').iterdir()}==sessions,'failed fork left a partial rollout')
                check(git(source,'worktree','list','--porcelain')==worktrees and json.loads((store/'workspaces.json').read_text())['worktrees']==registry['worktrees'],'failed fork left a Git worktree or ownership registration')
                check(json.loads(state_path.read_text())['threadMetadata']==broken['threadMetadata'],'failed fork left partial desktop metadata')
                check(fixture.fingerprint(source)==original and (source/'.git/index').read_bytes()==index,'failed fork changed the source workspace')
                state_path.write_text(json.dumps(state))
                plain=root/'not-git';plain.mkdir()
                non_git=app.rpc('thread/start',{'mode':'codex','cwd':str(plain)})['thread']['id']
                app.expect_error('thread/fork',{'threadId':non_git,'destination':'newWorktree'},'Git repository')
                check(git(source,'worktree','list','--porcelain')==worktrees,'non-Git fork created a worktree before validation')
                app.expect_error('thread/fork',{'threadId':parent,'destination':'elsewhere'},'destination')
                app.expect_error('thread/fork',{'threadId':parent,'reasoningEffort':'invented'},'reasoning')
                slow=mock.plan('slow');active_turn=app.turn(parent);mock.request();requests+=1
                check(slow.opened.wait(timeout),'source response did not enter its live stream')
                active_copy=app.rpc('thread/fork',{'threadId':parent,'titleMode':'fork'})['thread']
                check(app.rpc('thread/read',{'threadId':parent})['thread']['status']['type']=='active','fork interrupted the original response')
                check(len(active_copy['turns'])>=len(history),'active fork lost the completed history prefix')
                app.rpc('turn/interrupt',{'threadId':parent});slow.release.set()
                check(app.event('turn/completed',parent,active_turn)['turn']['status']=='interrupted','source response could not stop after a parallel fork')
                work=app.rpc('thread/start',{'mode':'work','cwd':str(source)})['thread']['id']
                check(app.rpc('thread/fork',{'threadId':work})['thread']['mode']=='work','ordinary Work forks changed mode')
                app.expect_error('thread/fork',{'threadId':work,'destination':'newWorktree'},'Codex')
                rpcs+=app.rpc_count
            finally:app.close()
            restarted=AppServer(binary,store,source/'nested',mock.base_url,timeout)
            try:
                restarted.rpc('initialize',{})
                resumed=restarted.rpc('thread/resume',{'threadId':isolated['id']})['thread']
                check(resumed['cwd']==isolated['cwd'] and resumed['name']==isolated['name'] and resumed['forkedFromId']==parent,'fork execution identity did not survive restart')
                check(resumed['turns']==history and resumed['reasoningEffort']=='high','fork history or reasoning setting did not survive restart')
                response=mock.plan('success','你好，工作树聊天可独立继续 🙂')
                turn=restarted.rpc('turn/start',{'threadId':isolated['id'],'input':[{'type':'text','text':'Continue the fork'}]})['turn']['id']
                request=mock.request();requests+=1
                check(request.get('reasoning',{}).get('effort')=='high','fork continuation did not use its saved reasoning effort')
                restarted.assert_chinese_completion(isolated['id'],turn,response.text)
                check(len(restarted.rpc('thread/read',{'threadId':isolated['id'],'includeTurns':True})['thread']['turns'])==len(history)+1,'fork continuation did not append exactly one independent turn')
                check(fixture.fingerprint(source)==original,'fork continuation changed the source checkout')
                rpcs+=restarted.rpc_count
            finally:restarted.close()
            check(not mock.errors,f'loopback mock errors: {mock.errors}')
    print(f'fork smoke: {checks} checks, {rpcs} real JSONL RPCs, {requests} loopback model requests; temporary data removed')

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=Path('target/debug/mcodex'));parser.add_argument('--timeout',type=float,default=30)
    options=parser.parse_args();run(options.binary.resolve(),options.timeout)
