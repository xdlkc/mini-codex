#!/usr/bin/env python3
"""Check persistent command approvals through real RPCs and the installed CLI oracle."""
from __future__ import annotations
import argparse, importlib.util, json, os, subprocess, sys, tempfile
from pathlib import Path
sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location('exec_policy_fixture',Path(__file__).with_name('smoke-guardian.py'))
fixture=importlib.util.module_from_spec(spec);sys.modules[spec.name]=fixture;spec.loader.exec_module(fixture)
require=fixture.require

def run(binary,evaluator,reference,timeout):
    checks=0;sequence=0;rpcs=0
    def check(value,message):
        nonlocal checks
        require(value,message);checks+=1
    with tempfile.TemporaryDirectory(prefix='.mcodex-policy-smoke-',dir=Path.cwd()) as temporary:
        root=Path(temporary).resolve();workspace=root/'workspace';workspace.mkdir();store=root/'profile';store.mkdir()
        provider=fixture.Provider(timeout);app=None
        def connect():
            value=fixture.fixture.WorkspaceServer(binary,store,workspace,provider.base_url,timeout);value.rpc('initialize',{});return value
        def thread(**settings):return app.rpc('thread/start',{'mode':'codex','sandbox':'workspace-write','approvalsReviewer':'user',**settings})['thread']['id']
        def plan(args):
            nonlocal sequence
            sequence+=1;provider.plan(output=[{'type':'function_call','id':f'item-{sequence}','call_id':f'call-{sequence}','name':'shell','arguments':json.dumps(args)}]);provider.plan('本机规则测试已结束。')
        def finish(tid,turn):provider.request();return app.event('turn/completed',tid,turn)
        def request(tid,args):
            plan(args);turn=app.turn(tid);provider.request();return turn
        try:
            app=connect();first=thread();outside=root/'first'
            command={'command':['/usr/bin/touch',str(outside)],'sandbox_permissions':'require_escalated','prefix_rule':['/usr/bin/touch']}
            turn=request(first,command);approval=app.approval(first);prefix=approval['params']['proposedExecpolicyAmendment']
            check(prefix==['/usr/bin/touch'] and not outside.exists(),'request did not capture the offered prefix before execution')
            app.respond_approval(approval['id'],{'acceptWithExecpolicyAmendment':{'execpolicy_amendment':prefix}});finish(first,turn);app.approval_requests.pop(approval['id'],None)
            rules=store/'rules/default.rules'
            check(outside.exists() and rules.is_file(),'accepted command rule was not saved before execution')
            text=rules.read_text();check('prefix_rule(' in text and 'decision="allow"' in text,'saved rule is not in the reference Starlark format')
            second=thread();path=root/'second';turn=request(second,{'command':['/usr/bin/touch',str(path)]});finish(second,turn)
            check(path.exists() and not app.approval_requests,'saved rule was not used by another chat or did not bypass only its matched command')
            path=root/'other-command';turn=request(second,{'command':['/bin/sh','-c',f'printf changed > "{path}"'],'sandbox_permissions':'require_escalated'});approval=app.approval(second)
            app.respond_approval(approval['id'],{'acceptWithExecpolicyAmendment':{'execpolicy_amendment':['/bin/sh']}});finish(second,turn);app.approval_requests.pop(approval['id'],None)
            check(not path.exists() and rules.read_text()==text,'a changed prefix executed the action or broadened the saved policy')
            never=thread(approvalPolicy='never');path=root/'never';turn=request(never,{'command':['/usr/bin/touch',str(path)],'sandbox_permissions':'require_escalated'});finish(never,turn)
            check(not path.exists() and not app.approval_requests,'a saved rule bypassed Never explicit-escalation validation')
            prompted=root/'policy-prompt';(store/'rules/prompt.rules').write_text(f'prefix_rule(pattern={json.dumps(["/usr/bin/touch",str(prompted)])}, decision="prompt")\n')
            turn=request(never,{'command':['/usr/bin/touch',str(prompted)]});final=provider.request();app.event('turn/completed',never,turn)
            check(not prompted.exists() and 'approval required by policy, but AskForApproval is set to Never' in json.dumps(final['input']),'Never lost the exact policy rejection or executed a prompted action')
            protected=root/'never-delete';protected.write_text('keep')
            turn=request(never,{'command':['/bin/rm','-f',str(protected)]});final=provider.request();app.event('turn/completed',never,turn)
            check(protected.read_text()=='keep' and 'rm -f style commands' in json.dumps(final['input']),'dangerous-command rejection differed from the reference or changed a file')
            forbidden=root/'forbidden';(store/'rules/deny.rules').write_text(f'prefix_rule(pattern={json.dumps(["/usr/bin/touch",str(forbidden)])}, decision="forbidden", justification="Owned fixture is denied")\n')
            turn=request(second,{'command':['/usr/bin/touch',str(forbidden)]});final=provider.request();app.event('turn/completed',second,turn)
            check(not forbidden.exists() and 'Owned fixture is denied' in json.dumps(final['input']),'an allow rule overrode a more restrictive rule')
            broken=store/'rules/broken.rules';broken.write_text('prefix_rule(pattern=[')
            path=root/'invalid-policy';turn=request(second,{'command':['/usr/bin/touch',str(path)]});final=provider.request();app.event('turn/completed',second,turn)
            warnings=[row for row in app.notifications if row.get('method')=='configWarning' and row['params'].get('threadId')==second]
            check(not path.exists() and warnings and 'broken.rules' in warnings[-1]['params']['summary'],'malformed rules failed to report their warning or removed sandbox protection')
            check('Command rules were not loaded' not in json.dumps(final['input']),'configuration diagnostics entered model history as user or assistant content')
            broken.unlink();rpcs+=app.rpc_count;app.close();app=connect();third=thread();path=root/'after-restart'
            turn=request(third,{'command':['/usr/bin/touch',str(path)]});finish(third,turn)
            check(path.exists() and not app.approval_requests,'restarting the backend lost the approved command rule')
            check(not provider.errors,f'loopback errors: {provider.errors}');rpcs+=app.rpc_count
        finally:
            if app:app.close()
            provider.close()
        policy=root/'oracle.rules';policy.write_text('''def permit(program):
    prefix_rule(pattern=[program, ["status", "diff"]], decision="allow")
permit("git")
prefix_rule(pattern=["git", "push"], decision="prompt")
prefix_rule(pattern=["git", "diff", "--no-index"], decision="forbidden", justification="Owned fixture is denied")
host_executable(name="git", paths=["/usr/bin/git"])
''')
        oracle_home=root/'oracle-home';oracle_home.mkdir()
        environment=os.environ.copy();environment['CODEX_HOME']=str(oracle_home)
        for name in ['OPENAI_API_KEY','CODEX_API_KEY']:environment.pop(name,None)
        cases=[['git','status'],['git','diff','--stat'],['git','push','origin'],['git','diff','--no-index','a','b'],['other','status'],['/usr/bin/git','status'],['/unapproved/git','status']]
        for resolve in [False,True]:
            for argv in cases:
                options=['--rules',str(policy)]+(['--resolve-host-executables'] if resolve else [])+['--',*argv]
                expected=subprocess.run([str(reference),'execpolicy','check',*options],env=environment,capture_output=True,text=True,timeout=timeout)
                actual=subprocess.run([str(evaluator),'check',*options],env=environment,capture_output=True,text=True,timeout=timeout)
                require(expected.returncode==actual.returncode==0,f'oracle check failed: {expected.stderr} {actual.stderr}')
                check(json.loads(expected.stdout)==json.loads(actual.stdout),f'policy result differs from installed CLI for {argv}, resolve={resolve}')
        print(f'exec-policy smoke: {checks} checks, {rpcs} RPCs, {provider.count} loopback requests; installed CLI oracle matched; temporary data removed')

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=Path('target/debug/mcodex'));parser.add_argument('--evaluator',type=Path,default=Path('target/debug/codex-execpolicy'));parser.add_argument('--reference',type=Path,default=Path('/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex'));parser.add_argument('--timeout',type=float,default=30);args=parser.parse_args();run(args.binary.resolve(),args.evaluator.resolve(),args.reference.resolve(),args.timeout)
