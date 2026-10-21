#!/usr/bin/env python3
"""Verify the installed Codex rule workflow using an isolated profile and local provider."""
import importlib.util,json,os,shlex,subprocess,sys,tempfile,time
from pathlib import Path
sys.dont_write_bytecode=True
path=Path(__file__).with_name('smoke-guardian.py')
spec=importlib.util.spec_from_file_location('source_rule_fixture',path);f=importlib.util.module_from_spec(spec);sys.modules[spec.name]=f;spec.loader.exec_module(f)
reference=Path('/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex')
with tempfile.TemporaryDirectory(prefix='.mcodex-reference-rules-',dir=path.parent.parent) as directory:
    root=Path(directory).resolve();work=root/'workspace';work.mkdir();store=root/'source-home';store.mkdir();provider=f.Provider(45,item_events=True)
    for args in [('init','-b','main'),('config','core.hooksPath','/dev/null'),('config','core.fsmonitor','false')]:
        subprocess.run(['git','-C',str(work),*args],check=True,capture_output=True)
    (store/'config.toml').write_text('model="gpt-5.4"\nmodel_provider="fixture"\napproval_policy="on-request"\nsandbox_mode="workspace-write"\n[model_providers.fixture]\nname="fixture"\nbase_url='+json.dumps(provider.base_url)+'\nwire_api="responses"\nenv_key="OPENAI_API_KEY"\n')
    wrapper=root/'reference-wrapper.py'
    wrapper.write_text('#!/usr/bin/env python3\nimport os\ne=os.environ.copy()\ne["CODEX_HOME"]='+repr(str(store))+'\nos.execve('+repr(str(reference))+',['+repr(str(reference))+',"-C",'+repr(str(work))+',"app-server"],e)\n');wrapper.chmod(0o700)
    app=f.fixture.WorkspaceServer(wrapper,store,work,provider.base_url,45)
    try:
        app.rpc('initialize',{'clientInfo':{'name':'owned-rule-probe','version':'1'},'capabilities':{'experimentalApi':True}})
        app.process.stdin.write(json.dumps({'method':'initialized'})+'\n');app.process.stdin.flush()
        def start():return app.rpc('thread/start',{'cwd':str(work),'model':'gpt-5.4','approvalPolicy':'on-request','sandbox':'workspace-write','approvalsReviewer':'user'})['thread']['id']
        def plan(target,escalate):
            args={'cmd':'/usr/bin/touch '+shlex.quote(str(target)),'shell':'/bin/sh','login':False}
            if escalate:args.update(sandbox_permissions='require_escalated',prefix_rule=['/usr/bin/touch'],justification='Create only the owned probe marker')
            provider.plan(output=[{'type':'function_call','id':'source-call-'+target.name,'call_id':'source-call-'+target.name,'name':'exec_command','arguments':json.dumps(args)}]);provider.plan('Owned probe completed.')
        first=start();one=root/'one';plan(one,True);turn=app.turn(first);provider.request();approval=app.approval(first)
        prefix=approval['params'].get('proposedExecpolicyAmendment');print('reference offered prefix:',json.dumps(prefix),flush=True)
        assert prefix==['/usr/bin/touch'] and not one.exists()
        app.rpc('thread/read',{'threadId':first,'includeTurns':True})
        print('command items started before approval:',[row['params'].get('item',{}).get('type') for row in app.notifications if row.get('method')=='item/started' and row['params'].get('item',{}).get('type')=='commandExecution'],flush=True)
        time.sleep(.5)
        app.respond_approval(approval['id'],{'acceptWithExecpolicyAmendment':{'execpolicy_amendment':prefix}});provider.request();app.event('turn/completed',first,turn)
        assert one.exists() and (store/'rules/default.rules').is_file()
        print('reference execution durations:',[row['params']['item'].get('durationMs') for row in app.notifications if row.get('method')=='item/completed' and row['params'].get('item',{}).get('type')=='commandExecution'],flush=True)
        second=start();two=root/'two';plan(two,False);turn=app.turn(second);provider.request();provider.request();app.event('turn/completed',second,turn)
        assert two.exists();print('reference persisted rule and used it in a new chat without another approval',flush=True)
        assert not provider.errors,provider.errors
    except Exception:
        print('provider diagnostics:',provider.count,provider.errors,flush=True)
        print('reference notifications:',json.dumps([{'method':row.get('method'),'threadId':row.get('params',{}).get('threadId'),'turnId':row.get('params',{}).get('turnId'),'turn':row.get('params',{}).get('turn'),'error':row.get('params',{}).get('error')} for row in app.notifications[-12:]],ensure_ascii=False)[:6500],flush=True)
        print('marker and rule persisted:',(root/'one').exists(),(store/'rules/default.rules').exists(),flush=True)
        raise
    finally:app.close();provider.close()
