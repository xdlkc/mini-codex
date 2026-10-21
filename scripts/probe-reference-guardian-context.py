#!/usr/bin/env python3
"""Check the installed Codex manual denial operation against an owned provider."""
import importlib.util,json,os,subprocess,sys,tempfile
from pathlib import Path
sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location('reference_guardian_fixture',Path(__file__).with_name('smoke-guardian.py'))
f=importlib.util.module_from_spec(spec);sys.modules[spec.name]=f;spec.loader.exec_module(f)
reference=Path('/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex')
with tempfile.TemporaryDirectory(prefix='.mcodex-reference-guardian-',dir=Path.cwd()) as directory:
    root=Path(directory).resolve();work=root/'workspace';work.mkdir();store=root/'source-home';store.mkdir();provider=f.Provider(45,item_events=True)
    for args in [('init','-b','main'),('config','core.hooksPath','/dev/null'),('config','core.fsmonitor','false')]:
        subprocess.run(['git','-C',str(work),*args],check=True,capture_output=True)
    (store/'config.toml').write_text('model="gpt-5.4"\nmodel_provider="fixture"\napproval_policy="on-request"\nsandbox_mode="workspace-write"\n[model_providers.fixture]\nname="fixture"\nbase_url='+json.dumps(provider.base_url)+'\nwire_api="responses"\nenv_key="OPENAI_API_KEY"\n')
    wrapper=root/'reference-wrapper.py';wrapper.write_text('#!/usr/bin/env python3\nimport os\ne=os.environ.copy()\ne["CODEX_HOME"]='+repr(str(store))+'\nos.execve('+repr(str(reference))+',['+repr(str(reference))+',"-C",'+repr(str(work))+',"app-server"],e)\n');wrapper.chmod(0o700)
    app=f.fixture.WorkspaceServer(wrapper,store,work,provider.base_url,45)
    marker=root/'never-auto-execute';event={'id':'reference-denial','status':'denied','action':{'type':'command','source':'shell','command':'touch '+str(marker),'cwd':str(work)}}
    try:
        app.rpc('initialize',{'clientInfo':{'name':'owned-guardian-context-probe','version':'1'},'capabilities':{'experimentalApi':True}})
        app.process.stdin.write(json.dumps({'method':'initialized'})+'\n');app.process.stdin.flush()
        thread=app.rpc('thread/start',{'cwd':str(work),'model':'gpt-5.4','approvalPolicy':'on-request','sandbox':'workspace-write'})['thread']['id']
        app.rpc('thread/approveGuardianDeniedAction',{'threadId':thread,'event':{**event,'status':'approved'}})
        provider.plan('No approval was added.');turn=app.turn(thread);first=provider.request();app.event('turn/completed',thread,turn)
        assert 'The user has manually approved' not in json.dumps(first['input'])
        count=provider.count;app.rpc('thread/approveGuardianDeniedAction',{'threadId':thread,'event':event})
        assert provider.count==count and not marker.exists()
        provider.plan('Only authorization context was added.');turn=app.turn(thread);request=provider.request();app.event('turn/completed',thread,turn)
        contexts=[item for item in request['input'] if item.get('role')=='developer' and 'The user has manually approved' in json.dumps(item)]
        assert len(contexts)==1 and str(marker) in json.dumps(contexts) and 'different payloads' in json.dumps(contexts)
        assert not marker.exists() and not provider.errors
        print('reference Guardian context: non-denials ignored; exact developer approval recorded; no automatic execution or new turn')
    finally:app.close();provider.close()
