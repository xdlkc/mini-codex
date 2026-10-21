#!/usr/bin/env python3
"""Sandbox execution and per-action grants using real RPCs and owned filesystem fixtures."""
from __future__ import annotations
import argparse,importlib.util,json,sys,tempfile,time
from pathlib import Path
sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location('permission_fixture',Path(__file__).with_name('smoke-guardian.py'));fixture=importlib.util.module_from_spec(spec);sys.modules[spec.name]=fixture;spec.loader.exec_module(fixture)
require=fixture.require

def run(binary,timeout):
    checks=0;sequence=0
    def check(value,message):
        nonlocal checks
        require(value,message);checks+=1
    with tempfile.TemporaryDirectory(prefix='.mcodex-permission-smoke-',dir=Path.cwd()) as tmp:
        root=Path(tmp).resolve();workspace=root/'workspace';workspace.mkdir();store=root/'profile';store.mkdir();alternate=root/'alternate';alternate.mkdir()
        provider=fixture.Provider(timeout);app=fixture.fixture.WorkspaceServer(binary,store,workspace,provider.base_url,timeout)
        def start(**overrides):return app.rpc('thread/start',{'mode':'codex','cwd':str(workspace),'sandbox':'workspace-write','approvalsReviewer':'user',**overrides})['thread']['id']
        def plan(name,args,answer=True):
            nonlocal sequence
            sequence+=1;provider.plan(output=[{'type':'function_call','id':f'item-{sequence}','call_id':f'call-{sequence}','name':name,'arguments':json.dumps(args)}])
            if answer:provider.plan('你好，测试步骤已结束。')
        def finish(thread,turn):
            provider.request();return app.event('turn/completed',thread,turn)
        def patch(input):return {'input':'*** Begin Patch\n'+input+'\n*** End Patch'}
        try:
            app.rpc('initialize',{});thread=start()
            plan('shell',{'command':['/usr/bin/touch','local.txt']});turn=app.turn(thread);provider.request();finish(thread,turn)
            check((workspace/'local.txt').exists() and not app.approval_requests,'ordinary workspace command incorrectly requested approval')
            outside=root/'outside.txt';plan('shell',{'command':['/usr/bin/touch',str(outside)]});turn=app.turn(thread);provider.request();finish(thread,turn)
            check(not outside.exists() and not app.approval_requests,'ordinary command escaped the sandbox or was automatically escalated')
            requested={'command':['/bin/sh','-c','printf granted > outside.txt'],'workdir':str(alternate),'sandbox_permissions':'require_escalated','justification':'Write the owned fixture in the selected directory'}
            plan('shell',requested);turn=app.turn(thread);provider.request();request=app.approval(thread)
            check(request['params']['cwd']==str(alternate) and request['params']['reason']==requested['justification'],'approval lost the actual directory or reason')
            check('acceptForSession' not in request['params']['availableDecisions'] and request['params']['proposedExecpolicyAmendment']==requested['command'] and not (alternate/'outside.txt').exists(),'approval lost its exact command prefix, offered an unsafe session grant or executed before acceptance')
            time.sleep(.5);approved_at=time.monotonic();app.respond_approval(request['id'],'accept');finish(thread,turn);app.approval_requests.pop(request['id'],None)
            approved_elapsed=(time.monotonic()-approved_at)*1000
            accepted_item=[row['params']['item'] for row in app.notifications if row.get('method')=='item/completed' and row['params'].get('threadId')==thread and row['params'].get('turnId')==turn and row['params'].get('item',{}).get('type')=='commandExecution'][-1]
            check(accepted_item['durationMs']<=approved_elapsed+50,'command execution duration included time waiting for user approval')
            check((alternate/'outside.txt').read_text()=='granted' and not (store/'rules/default.rules').exists(),'single-command approval did not execute with additional access or persisted an unapproved rule')
            plan('shell',{**requested,'command':['/bin/sh','-c','printf leaked > outside.txt'],'sandbox_permissions':'use_default'});turn=app.turn(thread);provider.request();finish(thread,turn)
            check((alternate/'outside.txt').read_text()=='granted','single-command grant widened a later ordinary command')
            never=start(approvalPolicy='never');plan('shell',{'command':['/usr/bin/touch',str(root/'never')],'sandbox_permissions':'require_escalated'});turn=app.turn(never);provider.request();finish(never,turn)
            check(not (root/'never').exists() and not any(r['params']['threadId']==never for r in app.approval_requests.values()),'Never policy requested or received additional access')
            plan('apply_patch',patch('*** Add File: direct.txt\n+direct'));turn=app.turn(thread);provider.request();finish(thread,turn)
            check((workspace/'direct.txt').read_text()=='direct\n','ordinary in-workspace patch did not apply directly')
            readonly=start(sandbox='read-only');plan('apply_patch',patch('*** Add File: approved.txt\n+first'));turn=app.turn(readonly);provider.request();request=app.approval(readonly)
            check(request['params']['files']==[str(workspace/'approved.txt')] and not (workspace/'approved.txt').exists(),'file approval did not bind the proposed file before mutation')
            app.respond_approval(request['id'],'acceptForSession');finish(readonly,turn);app.approval_requests.pop(request['id'],None)
            plan('apply_patch',patch('*** Update File: approved.txt\n@@\n-first\n+second'));turn=app.turn(readonly);provider.request();finish(readonly,turn)
            check((workspace/'approved.txt').read_text()=='second\n' and not app.approval_requests,'same-file session approval was not reused')
            plan('apply_patch',patch('*** Add File: another.txt\n+another'));turn=app.turn(readonly);provider.request();request=app.approval(readonly)
            check(not (workspace/'another.txt').exists(),'a file grant also approved another file')
            app.respond_approval(request['id'],'decline');finish(readonly,turn);app.approval_requests.pop(request['id'],None)
            check(not (workspace/'another.txt').exists(),'declined patch changed a file')
            outside_patch=root/'patch-outside.txt';plan('apply_patch',patch(f'*** Add File: {outside_patch}\n+outside'));turn=app.turn(thread);provider.request();request=app.approval(thread)
            app.respond_approval(request['id'],'accept');finish(thread,turn);app.approval_requests.pop(request['id'],None)
            check(outside_patch.read_text()=='outside\n','approved external patch did not apply')
            plan('shell',{'command':['/usr/bin/touch',str(root/'cancelled')],'sandbox_permissions':'require_escalated'},answer=False);turn=app.turn(thread);provider.request();request=app.approval(thread);count=provider.count;app.respond_approval(request['id'],'cancel');ended=app.event('turn/completed',thread,turn);app.approval_requests.pop(request['id'],None)
            check(ended['turn']['status']=='interrupted' and provider.count==count and not (root/'cancelled').exists(),'Cancel ran the action or continued the model turn')
            marker=workspace/'writer.txt';plan('shell',{'command':['/bin/sh','-c','(while :; do printf x >> "$1"; sleep 0.03; done) & wait','fixture',str(marker)],'sandbox_permissions':'require_escalated','timeout_ms':20000},answer=False);turn=app.turn(thread);provider.request();request=app.approval(thread);app.respond_approval(request['id'],'accept');app.approval_requests.pop(request['id'],None)
            deadline=time.monotonic()+timeout
            while not marker.exists():
                require(time.monotonic()<deadline,'writer did not start');time.sleep(.02)
            app.rpc('turn/interrupt',{'threadId':thread});ended=app.event('turn/completed',thread,turn);time.sleep(.1);size=marker.stat().st_size;time.sleep(.2)
            check(ended['turn']['status']=='interrupted' and marker.stat().st_size==size,'Stop left a descendant writing after completion')
            plan('shell',{'command':['/usr/bin/yes'],'timeout_ms':200});turn=app.turn(thread);provider.request();finish(thread,turn)
            history=app.rpc('thread/read',{'threadId':thread,'includeTurns':True})['thread']['turns'];items=[item for row in history for item in row['items'] if item.get('type')=='commandExecution']
            check(any(item.get('status')=='failed' and 'timed out' in item.get('aggregatedOutput','') for item in items),'command timeout was reported as successful')
            check(not provider.errors,f'loopback errors: {provider.errors}')
            print(f'permission smoke: {checks} checks, {app.rpc_count} RPCs, {provider.count} loopback model requests; temporary data removed')
        finally:app.close();provider.close()

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=Path('target/debug/mcodex'));parser.add_argument('--timeout',type=float,default=20);args=parser.parse_args();run(args.binary.resolve(),args.timeout)
