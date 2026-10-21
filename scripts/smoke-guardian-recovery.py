#!/usr/bin/env python3
"""Exercise manual Guardian context without silently replaying a denied action."""
from __future__ import annotations
import argparse, importlib.util, json, sys, tempfile
from pathlib import Path
sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location('recovery_fixture',Path(__file__).with_name('smoke-guardian.py'))
fixture=importlib.util.module_from_spec(spec);sys.modules[spec.name]=fixture;spec.loader.exec_module(fixture)
require=fixture.require

def run(binary,timeout):
    checks=0;rpcs=0
    def check(value,message):
        nonlocal checks
        require(value,message);checks+=1
    with tempfile.TemporaryDirectory(prefix='.mcodex-recovery-smoke-',dir=Path.cwd()) as directory:
        root=Path(directory).resolve();work=root/'workspace';work.mkdir();store=root/'profile';store.mkdir()
        provider=fixture.Provider(timeout);app=fixture.fixture.WorkspaceServer(binary,store,work,provider.base_url,timeout)
        marker=root/'exact-marker';command=json.dumps(str(marker))
        event={'id':'review-fixture','status':'denied','risk_level':'high','user_authorization':'low','action':{'type':'command','source':'shell','command':'/usr/bin/touch '+command,'cwd':str(work)}}
        def contexts(request):return [item for item in request['input'] if item.get('role')=='developer' and 'The user has manually approved' in str(item)]
        def approve(thread,value=event):return app.rpc('thread/approveGuardianDeniedAction',{'threadId':thread,'event':value})
        def start():return app.rpc('thread/start',{'mode':'codex','cwd':str(work),'approvalsReviewer':'guardian_subagent'})['thread']['id']
        def finished(thread,turn):return app.event('turn/completed',thread,turn)
        try:
            app.rpc('initialize',{});thread=start();count=provider.count
            app.expect_error('thread/approveGuardianDeniedAction',{'threadId':thread,'event':{'id':'bad','status':'denied','action':{'type':'unknown'}}},'invalid')
            non_denied={**event,'status':'approved'};approve(thread,non_denied)
            provider.plan('No manual approval was recorded.');turn=app.turn(thread);request=provider.request();finished(thread,turn)
            check(not contexts(request),'a non-denied event created approval context')
            count=provider.count;approve(thread)
            check(provider.count==count and not marker.exists(),'manual approval executed the proposal or started a model request')
            provider.call(['/usr/bin/touch',str(marker)]);provider.plan('{"outcome":"allow"}');provider.plan('Only the exact action ran.')
            turn=app.turn(thread);parent=provider.request();review=provider.request();provider.request();finished(thread,turn)
            check(len(contexts(parent))==1 and str(marker) in str(contexts(parent)[0]),'parent lost exact manual approval context')
            records=json.loads(review['input'][0]['content'][0]['text'])['transcript']
            check(any(row['author']=='developer' and str(marker) in row['text'] for row in records),'reviewer lost trusted manual authorization')
            check(marker.exists(),'a newly reviewed and allowed exact proposal did not execute')
            check('_mcodex_' not in json.dumps(review['input']),'reviewer received private rollout fields')
            fork=app.rpc('thread/fork',{'threadId':thread})['thread']['id'];other=root/'other'
            provider.call(['/usr/bin/touch',str(other)]);provider.plan('{"outcome":"deny","rationale":"Different action requires its own authorization"}');provider.plan('The inherited approval does not authorize this action.')
            turn=app.turn(fork);provider.request();review=provider.request();provider.request();finished(fork,turn)
            records=json.loads(review['input'][0]['content'][0]['text'])['transcript'];copied=[row for row in records if 'The user has manually approved' in row['text']]
            check(copied and all(row['author']=='assistant' for row in copied),'fork treated inherited manual approval as new trusted authorization')
            check(not other.exists(),'manual approval widened to a different action in the fork')
            active=start();slow=provider.plan('The active response finished.',slow=True);turn=app.turn(active);provider.request();require(slow['opened'].wait(timeout),'active response not opened')
            count=provider.count;approve(active);check(provider.count==count,'approval injected into a running chat started another turn')
            slow['release'].set();finished(active,turn)
            provider.plan('Context is available after completion.');turn=app.turn(active);request=provider.request();finished(active,turn)
            check(len(contexts(request))==1,'context queued during a final response was lost')
            cancelled=start();slow=provider.plan('This response will be cancelled.',slow=True);turn=app.turn(cancelled);provider.request();require(slow['opened'].wait(timeout),'cancel response not opened')
            approve(cancelled);app.rpc('turn/interrupt',{'threadId':cancelled});finished(cancelled,turn);slow['release'].set()
            rpcs+=app.rpc_count
        finally:app.close()
        app=fixture.fixture.WorkspaceServer(binary,store,work,provider.base_url,timeout)
        try:
            app.rpc('initialize',{});app.rpc('thread/resume',{'threadId':cancelled});provider.plan('The saved manual context survived interruption and restart.')
            turn=app.turn(cancelled);request=provider.request();finished(cancelled,turn)
            check(len(contexts(request))==1,'interrupt/restart lost an accepted manual action approval')
            check(not other.exists(),'restart executed an unapproved different action')
            check(not provider.errors,f'provider errors: {provider.errors}');rpcs+=app.rpc_count
        finally:app.close();provider.close()
        print(f'guardian recovery smoke: {checks} checks, {rpcs} RPCs, {provider.count} loopback requests; temporary data removed')

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=Path('target/debug/mcodex'));parser.add_argument('--timeout',type=float,default=20)
    args=parser.parse_args();run(args.binary.resolve(),args.timeout)
