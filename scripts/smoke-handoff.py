#!/usr/bin/env python3
"""Exercise Git + chat handoff with real JSONL RPCs and isolated Git fixtures."""
from __future__ import annotations
import argparse
import importlib.util
import json
import fcntl
import hashlib
import os
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path
import sys
import tempfile

sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location('mcodex_handoff_fixture',Path(__file__).with_name('smoke-workspaces.py'))
assert spec and spec.loader
fixture=importlib.util.module_from_spec(spec);sys.modules[spec.name]=fixture;spec.loader.exec_module(fixture)
require,git,MockResponses,AppServer=fixture.require,fixture.git,fixture.MockResponses,fixture.WorkspaceServer

def run(binary:Path,timeout:float)->None:
    checks=0;rpcs=0;requests=0
    def check(condition:bool,message:str)->None:
        nonlocal checks
        require(condition,message);checks+=1
    with tempfile.TemporaryDirectory(prefix='mcodex-handoff-') as directory:
        root=Path(directory).resolve();source=root/'source';store=root/'state';store.mkdir()
        fixture.repository(source);git(source,'switch','-c','feat/handoff');fixture.populate_dirty_worktree(source)
        (store/'config.toml').write_text('model = "local-mock-model"\n')
        before=fixture.fingerprint(source);index=(source/'.git/index').read_bytes()
        with MockResponses(timeout) as mock:
            app=AppServer(binary,store,source/'nested',mock.base_url,timeout)
            try:
                app.rpc('initialize',{})
                project=app.rpc('desktop/project/create',{'mode':'codex','name':'Handoff source','path':str(source)})['project']
                parent=app.rpc('thread/start',{'mode':'codex','projectId':project['id'],'cwd':str(source/'nested')})['thread']['id']
                response=mock.plan('success','你好，迁移前的聊天历史 🙂');turn=app.turn(parent,[{'type':'text','text':'Inspect this file before handing off'},{'type':'mention','path':'keep.txt'}]);mock.request();requests+=1
                app.assert_chinese_completion(parent,turn,response.text)
                app.rpc('thread/name/set',{'threadId':parent,'name':'迁移验收'})
                app.rpc('desktop/threadMetadata/set',{'threadId':parent,'pinned':True,'webSearchMode':'disabled'})
                history=app.rpc('thread/read',{'threadId':parent,'includeTurns':True})['thread']['turns']
                rollout=(store/'sessions'/f'{parent}.jsonl').read_bytes()
                params={'threadId':parent,'direction':'toWorktree','worktreeBranch':'feat/handoff','localBranch':'main','reasoningEffort':'high'}
                app.expect_error('thread/handoff',{**params,'threadId':''},'threadId is required')
                check(fixture.fingerprint(source)==before,'empty handoff ID selected the only saved chat')
                app.rpc('thread/goal/set',{'threadId':parent,'objective':'Do not orphan this goal','status':'paused'})
                source_goal=app.rpc('thread/goal/get',{'threadId':parent})['goal']
                task_params={'kind':'heartbeat','mode':'codex','state':'paused','name':'Handoff heartbeat','prompt':'Check this chat','cwd':str(source),'targetThreadId':parent,'model':'local-mock-model','schedule':{'dtstart':(datetime.now(timezone.utc)+timedelta(days=1)).strftime('DTSTART:%Y%m%dT%H%M%SZ'),'rrule':'FREQ=DAILY;COUNT=1','timezone':'UTC'}}
                heartbeat=app.rpc('scheduler/create',task_params)['task']
                eligibility=app.rpc('thread/handoff/inspect',{'threadId':parent})
                check(eligibility['blockedReason'] is None and eligibility['workspace']['sourceBranch']=='feat/handoff','Goal or heartbeat incorrectly prevented handoff')
                # A separate process holding the real reservation must exclude
                # all source writes and both scheduled/manual execution paths.
                lock_path=store/'thread-lifecycle'/f'{parent.encode().hex()}.handoff.lock'
                with lock_path.open('r+b') as held:
                    fcntl.flock(held,fcntl.LOCK_EX|fcntl.LOCK_NB)
                    app.expect_error('thread/goal/set',{'threadId':parent,'status':'active'},'handed off')
                    app.expect_error('thread/goal/clear',{'threadId':parent},'handed off')
                    app.expect_error('scheduler/create',task_params,'handed off')
                    app.expect_error('scheduler/update',{'id':heartbeat['id'],'prompt':'Changed'},'handed off')
                    app.expect_error('scheduler/delete',{'id':heartbeat['id']},'handed off')
                    app.expect_error('scheduler/run',{'id':heartbeat['id']},'handed off')
                    app.expect_error('turn/start',{'threadId':parent,'input':[{'type':'text','text':'Concurrent source response'}]},'handed off')
                check(app.rpc('thread/goal/get',{'threadId':parent})['goal']==source_goal,'rejected concurrent writes changed Goal counters')
                check(fixture.fingerprint(source)==before and (source/'.git/index').read_bytes()==index,'concurrent rejection changed Git')
                execution_path=store/'thread-lifecycle'/f'{parent.encode().hex()}.execution.lock'
                with execution_path.open('a+b') as held:
                    fcntl.flock(held,fcntl.LOCK_EX|fcntl.LOCK_NB)
                    app.expect_error('turn/start',{'threadId':parent,'input':[{'type':'text','text':'Concurrent execution'}]},'another window')
                    app.expect_error('thread/handoff',params,'another window')
                check(fixture.fingerprint(source)==before,'external execution exclusion changed Git')
                checkout_key=hashlib.sha256(os.fsencode(source.resolve())).hexdigest()
                checkout_lock=store/'thread-lifecycle'/'checkouts'/f'{checkout_key}.lock'
                with checkout_lock.open('r+b') as held:
                    fcntl.flock(held,fcntl.LOCK_SH|fcntl.LOCK_NB)
                    app.expect_error('thread/handoff',params,'running responses')
                check(fixture.fingerprint(source)==before,'another process execution lease did not protect the checkout')
                with checkout_lock.open('r+b') as held:
                    fcntl.flock(held,fcntl.LOCK_EX|fcntl.LOCK_NB)
                    app.expect_error('turn/start',{'threadId':parent,'input':[{'type':'text','text':'Concurrent checkout execution'}]},'checkout is being handed off')
                check(app.rpc('thread/goal/get',{'threadId':parent})['goal']==source_goal,'checkout exclusion changed the source Goal')
                params['operationId']='handoff-smoke-primary'
                app.next_id+=1;pending_id=app.next_id;app.rpc_count+=1
                app.process.stdin.write(json.dumps({'id':pending_id,'method':'thread/handoff','params':params})+'\n');app.process.stdin.flush()
                app.pump_until(lambda:any(item.get('method')=='thread/handoff/progress' and item['params'].get('operationId')==params['operationId'] for item in app.notifications),'handoff progress before completion')
                live=app.rpc('thread/handoff/status',{'threadId':parent,'operationId':params['operationId']})
                check(live['active'] and live['threadId']==parent,'live handoff status did not observe its actual coordinator')
                app.pump_until(lambda:pending_id in app.replies,'handoff completion');reply=app.replies.pop(pending_id);require('error' not in reply,f'handoff failed: {reply.get("error")}')
                result=reply['result'];target=result['thread'];checkout=Path(result['handoff']['targetPath'])
                status=app.rpc('thread/handoff/status',{'threadId':parent,'operationId':params['operationId']})
                check(not status['active'] and status['operation']['status']=='committed' and status['operation']['ownerThreadId']==target['id'],'completed handoff could not be recovered by its request ID')
                check('sourceBefore' not in status['operation'] and 'targetBefore' not in status['operation'],'status response exposed full file inventories')
                app.expect_error('thread/handoff/status',{'threadId':target['id'],'operationId':params['operationId']},'another chat')
                check(target['id']!=parent and target['forkedFromId']==parent,'handoff did not create a distinct destination chat')
                check(target['name']=='迁移验收' and target['cwd']==str(checkout/'nested'),'handoff lost the title or chat subdirectory')
                check(target['pinned'] is True and target['projectId']==project['id'] and target['webSearchMode']=='disabled','handoff lost pin/project/search settings')
                check(target['turns']==history and target['reasoningEffort']=='high','handoff lost history/UI snapshots or reasoning effort')
                source_meta=app.rpc('desktop/threadMetadata/read',{'threadId':parent})['metadata']
                check(source_meta['archived'] is True and source_meta['pinned'] is False,'source chat was not archived and unpinned')
                check(app.rpc('thread/goal/get',{'threadId':parent})['goal']==source_goal,'handoff moved or changed the paused source Goal')
                check(app.rpc('thread/goal/get',{'threadId':target['id']})['goal'] is None,'handoff copied the source Goal to its destination')
                app.expect_error('scheduler/read',{'id':heartbeat['id']},'not found')
                app.expect_error('thread/resume',{'threadId':parent},'archived')
                app.expect_error('thread/goal/set',{'threadId':parent,'status':'active'},'archived')
                check((store/'sessions'/f'{parent}.jsonl').read_bytes()==rollout,'handoff rewrote source history')
                check(git(source,'branch','--show-current').strip()==b'main' and git(checkout,'branch','--show-current').strip()==b'feat/handoff','handoff did not move branch ownership')
                check(not git(source,'status','--porcelain','--untracked-files=all'),'source retained moved working changes')
                check((checkout/'mixed.txt').read_bytes()==before[3]['mixed.txt'][-1],'destination working bytes differ')
                cached=git(checkout,'diff','--cached','--name-only').splitlines()
                check(b'mixed.txt' not in cached and b'raw.crlf' not in cached,'handoff restored an existing file\'s source staging despite stash apply without --index')
                check(not (checkout/'ignored').exists() and (source/'ignored').exists(),'handoff moved ignored files')
                record=json.loads((store/'workspaces.json').read_text())
                check(any(tree.get('ownerThreadId')==target['id'] for tree in record['worktrees'].values()),'destination worktree has no chat owner')
                app.expect_error('turn/start',{'threadId':parent,'input':[{'type':'text','text':'stale source'}]},'thread not loaded')
                check(not result['handoff']['warnings'],'ordinary handoff unexpectedly warned')
                response=mock.plan('success','你好，在工作树中继续 🙂');turn=app.rpc('turn/start',{'threadId':target['id'],'input':[{'type':'text','text':'Continue in the handed-off checkout'}]})['turn']['id'];request=mock.request();requests+=1
                check(request.get('reasoning',{}).get('effort')=='high','destination continuation did not restore its reasoning effort')
                app.assert_chinese_completion(target['id'],turn,response.text)
                complete_history=app.rpc('thread/read',{'threadId':target['id'],'includeTurns':True})['thread']['turns']
                returned=app.rpc('thread/handoff',{'threadId':target['id'],'direction':'toLocal'})
                local=returned['thread']
                check(local['cwd']==str(source/'nested') and local['name']=='迁移验收' and local['pinned'],'return handoff lost destination context')
                check(local['turns']==complete_history and local['forkedFromId']==target['id'],'return handoff lost independent continuation')
                check(not git(checkout,'branch','--show-current').strip() and git(source,'branch','--show-current').strip()==b'feat/handoff','return handoff did not detach source and check out local branch')
                check(not git(checkout,'status','--porcelain','--untracked-files=all'),'return handoff left moved files in the worktree')
                check(app.rpc('desktop/threadMetadata/read',{'threadId':target['id']})['metadata']['archived'],'return handoff did not archive its worktree chat')
                check(not git(source,'stash','list'),'completed handoffs leaked source stashes')
                stages=[item['params']['stage'] for item in app.notifications if item.get('method')=='thread/handoff/progress']
                check(all(stage in stages for stage in ['stash-source-changes','checkout-local-branch','checkout-worktree-branch','apply-changes-to-worktree','detach-worktree-branch','apply-changes-to-local','switching-thread','ready']),'handoff missed progress stages')
                # The reference also supports continuing in an existing tree
                # without moving its branch or either checkout's working files.
                source_before_continue=fixture.fingerprint(source);target_before_continue=fixture.fingerprint(checkout)
                continued=app.rpc('thread/handoff',{'threadId':local['id'],'direction':'toWorktree','targetPath':str(checkout),'continueExisting':True})['thread']
                check(continued['cwd']==str(checkout/'nested') and continued['turns']==complete_history,'continue-in-existing lost folder or history')
                check(continued['pinned'] and continued['name']=='迁移验收' and continued['forkedFromId']==local['id'],'continue-in-existing lost chat identity')
                check(fixture.fingerprint(source)==source_before_continue and fixture.fingerprint(checkout)==target_before_continue,'continue-in-existing changed a branch, index or working file')
                check(app.rpc('desktop/threadMetadata/read',{'threadId':local['id']})['metadata']['archived'],'continue-in-existing left its original chat unarchived')
                local_id=continued['id'];local_history=continued['turns'];resumed_cwd=continued['cwd']
                # Invalid requests must not interrupt a live response. An
                # explicit valid handoff must stop it and copy its saved state.
                active=app.rpc('thread/start',{'mode':'codex','cwd':str(source/'nested')})['thread']['id']
                slow=mock.plan('slow')
                app.rpc('thread/goal/set',{'threadId':active,'objective':'Preserve running Goal accounting','tokenBudget':100000})
                active_turn=app.event('turn/started',active)['turn']['id'];mock.request();requests+=1
                check(slow.opened.wait(timeout),'active handoff fixture did not start streaming')
                app.expect_error('thread/handoff',{'threadId':active,'direction':'invalid','interrupt':True},'direction')
                check(app.rpc('thread/read',{'threadId':active})['thread']['status']['type']=='active','invalid handoff interrupted a live response')
                active_result=app.rpc('thread/handoff',{'threadId':active,'direction':'toWorktree','worktreeBranch':'feat/active-handoff','interrupt':True})
                slow.release.set()
                check(app.event('turn/completed',active,active_turn)['turn']['status']=='interrupted','explicit handoff did not stop the source response')
                check(any(turn['status']=='interrupted' for turn in active_result['thread']['turns']),'handoff lost the interrupted response snapshot')
                check(app.rpc('desktop/threadMetadata/read',{'threadId':active})['metadata']['archived'],'explicit handoff did not archive the stopped source')
                stopped_goal=app.rpc('thread/goal/get',{'threadId':active})['goal']
                check(stopped_goal['status']=='paused' and stopped_goal['turnsStarted']==1 and stopped_goal['turnsCompleted']==1,'system handoff interruption did not pause and drain its source Goal')
                check(stopped_goal['tokenBudget']==100000 and app.rpc('thread/goal/get',{'threadId':active_result['thread']['id']})['goal'] is None,'interrupted handoff changed the budget or copied the Goal')
                # A failed copy follows a successful Git move in the reference.
                # Keep that move, the original chat, and its heartbeat definition.
                failed_repo=root/'failed-copy';fixture.repository(failed_repo);git(failed_repo,'switch','-c','feat/failed-copy');fixture.populate_dirty_worktree(failed_repo)
                failed_before=fixture.fingerprint(failed_repo)
                failed_project=app.rpc('desktop/project/create',{'mode':'codex','name':'Failed copy','path':str(failed_repo)})['project']
                failed_thread=app.rpc('thread/start',{'mode':'codex','projectId':failed_project['id'],'cwd':str(failed_repo)})['thread']['id']
                failed_heartbeat=app.rpc('scheduler/create',{**task_params,'targetThreadId':failed_thread})['task']
                state_path=store/'desktop.json';state=json.loads(state_path.read_text());broken=json.loads(json.dumps(state));broken['projects'].pop(failed_project['id']);state_path.write_text(json.dumps(broken))
                failed=app.rpc('thread/handoff',{'threadId':failed_thread,'direction':'toWorktree','worktreeBranch':'feat/failed-copy','localBranch':'main','operationId':'failed-copy-keeps-git'})
                check(failed['thread']['id']==failed_thread and failed['handoff']['warnings'],'failed copy did not retain the original chat with a warning')
                check(failed['handoff']['status']=='committed' and failed['handoff']['ownerThreadId']==failed_thread,'failed copy lost the committed Git owner')
                check(git(failed_repo,'branch','--show-current').strip()==b'main' and (Path(failed['handoff']['targetPath'])/'mixed.txt').read_bytes()==failed_before[3]['mixed.txt'][-1],'failed copy incorrectly undid the successful Git move')
                check(not app.rpc('desktop/threadMetadata/read',{'threadId':failed_thread})['metadata']['archived'] and app.rpc('scheduler/read',{'id':failed_heartbeat['id']})['task']['id']==failed_heartbeat['id'],'failed copy archived its source or removed its heartbeat')
                recovered=app.rpc('thread/handoff/status',{'threadId':failed_thread,'operationId':'failed-copy-keeps-git'})['operation']
                check(recovered['warnings']==failed['handoff']['warnings'],'timeout recovery lost the failed-copy warning')
                restored=json.loads(state_path.read_text());restored['projects'][failed_project['id']]=state['projects'][failed_project['id']];state_path.write_text(json.dumps(restored))
                rpcs+=app.rpc_count
            finally:app.close()
            restarted=AppServer(binary,store,source/'nested',mock.base_url,timeout)
            try:
                restarted.rpc('initialize',{});resumed=restarted.rpc('thread/resume',{'threadId':local_id})['thread']
                check(resumed['cwd']==resumed_cwd and resumed['turns']==local_history and resumed['pinned'],'completed handoff did not survive restart')
                status=restarted.rpc('thread/handoff/status',{'threadId':parent,'operationId':'handoff-smoke-primary'})
                check(status['operation']['ownerThreadId']==target['id'] and not status['active'],'request-linked handoff result did not survive restart')
                rpcs+=restarted.rpc_count
            finally:restarted.close()
            # An idle active Goal stays active on its archived original. Disable
            # dispatch with a real external lease while setting up that state.
            idle_repo=root/'idle-goal';fixture.repository(idle_repo);idle_store=root/'idle-state';idle_store.mkdir()
            with (idle_store/'schedules.dispatch.lock').open('a+b') as dispatch:
                fcntl.flock(dispatch,fcntl.LOCK_EX|fcntl.LOCK_NB)
                idle=AppServer(binary,idle_store,idle_repo,mock.base_url,timeout)
                try:
                    idle.rpc('initialize',{})
                    original=idle.rpc('thread/start',{'mode':'codex','cwd':str(idle_repo)})['thread']['id']
                    idle.rpc('thread/goal/set',{'threadId':original,'objective':'Idle active goal stays on its source','tokenBudget':1234})
                    moved=idle.rpc('thread/handoff',{'threadId':original,'direction':'toWorktree','worktreeBranch':'feat/idle-goal'})
                    retained=idle.rpc('thread/goal/get',{'threadId':original})['goal']
                    check(retained['status']=='active' and retained['turnsStarted']==0 and retained['tokenBudget']==1234,'idle handoff paused, ran or altered its source Goal')
                    check(idle.rpc('thread/goal/get',{'threadId':moved['thread']['id']})['goal'] is None,'idle handoff copied its Goal')
                    ordinary=idle.rpc('thread/start',{'mode':'codex','cwd':str(idle_repo)})['thread']['id']
                    idle.rpc('thread/goal/set',{'threadId':ordinary,'objective':'Ordinary archive retains idle goal'})
                    removed=idle.rpc('scheduler/create',{**task_params,'cwd':str(idle_repo),'targetThreadId':ordinary})['task']
                    idle.rpc('thread/archive',{'threadId':ordinary})
                    check(idle.rpc('thread/goal/get',{'threadId':ordinary})['goal']['status']=='active','ordinary archive paused an idle Goal')
                    idle.expect_error('scheduler/read',{'id':removed['id']},'not found')
                    idle.expect_error('thread/resume',{'threadId':ordinary},'archived')
                    rpcs+=idle.rpc_count
                finally:idle.close()
            # Restart with a live dispatcher. Neither archived Goal may execute.
            mock.plan('success','你好，取消归档后目标恢复执行 🙂')
            idle=AppServer(binary,idle_store,idle_repo,mock.base_url,timeout)
            try:
                idle.rpc('initialize',{})
                deadline=time.monotonic()+1.5
                while time.monotonic()<deadline:
                    retained=idle.rpc('thread/goal/get',{'threadId':original})['goal']
                    require(retained['status']=='active' and retained['turnsStarted']==0,'archived source Goal resumed after restart')
                    time.sleep(.1)
                check(idle.rpc('thread/goal/get',{'threadId':ordinary})['goal']['turnsStarted']==0,'ordinary archived Goal ran after restart')
                check(not any(item.get('method')=='turn/started' for item in idle.notifications),'archived Goal started a model turn')
                idle.rpc('thread/unarchive',{'threadId':original})
                resumed_turn=idle.event('turn/started',original)['turn']['id'];mock.request();requests+=1
                idle.event('turn/completed',original,resumed_turn)
                idle.rpc('thread/goal/set',{'threadId':original,'status':'paused'})
                check(idle.rpc('thread/resume',{'threadId':original})['thread']['id']==original,'unarchive did not restore source availability')
                rpcs+=idle.rpc_count
            finally:idle.close()
            check(not mock.errors,f'loopback mock errors: {mock.errors}')
    print(f'handoff smoke: {checks} checks, {rpcs} real JSONL RPCs, {requests} loopback model requests; temporary data removed')

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=Path('target/debug/mcodex'));parser.add_argument('--timeout',type=float,default=180)
    options=parser.parse_args();run(options.binary.resolve(),options.timeout)
