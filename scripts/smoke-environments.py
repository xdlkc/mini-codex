#!/usr/bin/env python3
"""Real local-environment RPCs, Bash, worktrees, cleanup and durable recovery."""
from __future__ import annotations
import argparse,importlib.util,json,os,subprocess,sys,tempfile,time
from pathlib import Path
sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location('environment_smoke',Path(__file__).with_name('smoke-desktop.py'))
f=importlib.util.module_from_spec(spec);sys.modules[spec.name]=f;spec.loader.exec_module(f)

def run(binary,timeout):
    checks=0;rpcs=0
    def check(value,message):
        nonlocal checks
        f.require(value,message);checks+=1
    def git(cwd,*args):
        env=os.environ.copy();env.update(GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL='/dev/null')
        return subprocess.run(['git','-C',str(cwd),'-c','core.hooksPath=/dev/null','-c','core.fsmonitor=false',*args],check=True,capture_output=True,text=True,env=env).stdout.strip()
    def config(setup,cleanup=''):
        return 'version=1\nname="fixture"\n[setup]\nscript='+json.dumps(setup)+'\n[cleanup]\nscript='+json.dumps(cleanup)+'\n[[actions]]\nname="Read export"\ncommand='+json.dumps('printf %s "$MC_ENV_READY"')+'\n'
    with tempfile.TemporaryDirectory(prefix='.mcodex-environment-smoke-',dir=Path.cwd()) as directory:
        root=Path(directory).resolve();workspace=root/'workspace';workspace.mkdir();home=root/'profile';home.mkdir()
        git(workspace,'init','--template=','-b','main');git(workspace,'config','user.name','Environment fixture');git(workspace,'config','user.email','fixture@example.invalid');(workspace/'tracked').write_text('base');git(workspace,'add','tracked');git(workspace,'commit','-m','fixture')
        app=f.AppServer(binary,home,workspace,timeout)
        def wait_run(start):
            deadline=time.monotonic()+timeout;identifier=start['id']
            while True:
                value=app.rpc('workspace/environment/status',{'id':identifier})['run']
                if value['status']!='running':return value
                f.require(time.monotonic()<deadline,'environment run timed out');time.sleep(.03)
        def save(cwd,path,raw):
            current=app.rpc('workspace/environment/read',{'cwd':str(cwd),'configPath':str(path)})
            result=app.rpc('workspace/environment/save',{'cwd':str(cwd),'configPath':str(path),'raw':raw,'expectedRevision':current['revision']})
            f.require(result['type']=='success','environment save failed');return result
        try:
            app.initialize(home);listing=app.rpc('workspace/environment/list',{'cwd':str(workspace)});selected=Path(listing['defaultConfigPath'])
            check(listing['environments']==[],'fresh project invented an environment')
            setup='export MC_ENV_READY=ready; printf "%s|%s" "$CODEX_SOURCE_TREE_PATH" "$CODEX_WORKTREE_PATH" > setup-marker'
            cleanup='printf cleanup > cleanup-created; rm -f setup-marker'
            saved=save(workspace,selected,config(setup,cleanup))
            conflict=app.rpc('workspace/environment/save',{'cwd':str(workspace),'configPath':str(selected),'raw':'overwrite','expectedRevision':'sha256:wrong'})
            check(conflict['type']=='conflict' and selected.read_text()==config(setup,cleanup),'a revision conflict overwrote the environment')
            app.rpc('workspace/environment/selection/set',{'cwd':str(workspace),'configPath':str(selected)})
            thread=app.rpc('thread/start',{'mode':'codex','cwd':str(workspace)})['thread']['id']
            fork=app.rpc('thread/fork',{'threadId':thread,'destination':'newWorktree'})
            check(not (Path(fork['cwd'])/'setup-marker').exists() and app.rpc('workspace/environment/selection/read',{'cwd':fork['cwd']})['configPath'] is None,'fork unexpectedly selected or executed project setup')
            created=app.rpc('workspace/worktree/create',{'cwd':str(workspace),'localEnvironmentConfigPath':str(selected)});tree=created['worktree'];target=Path(tree['path']);run_result=wait_run(created['setupRun'])
            check(run_result['status']=='succeeded' and (target/'setup-marker').read_text()==f'{workspace}|{target}','worktree setup lost its source/target paths')
            check(not (workspace/'setup-marker').exists(),'worktree setup ran in the original source')
            mapped=Path(app.rpc('workspace/environment/selection/read',{'cwd':str(target)})['configPath']);check(mapped.is_relative_to(target) and mapped.read_text()==selected.read_text(),'selected untracked environment did not map to the target')
            action=app.rpc('workspace/environment/run',{'cwd':str(target),'configPath':str(mapped),'scriptType':'action','actionIndex':0})
            check(wait_run(action['run'])['status']=='succeeded','toolbar action did not execute')
            output=app.rpc('command/exec',{'cwd':str(target),'command':'printf %s "$MC_ENV_READY"'})['output'];check('ready' in output,'setup exports did not reach subsequent shell commands')
            (target/'user-data').write_text('preserve me')
            archived=app.rpc('workspace/worktree/archive',{'cwd':str(workspace),'id':tree['id']})['worktree']
            check(archived['state']=='archived' and not target.exists() and archived['cleanupResult']['status']=='succeeded','archive did not complete selected cleanup')
            check(git(workspace,'show',archived['cleanupSnapshot']['commit']+':cleanup-created')=='cleanup','cleanup output was not retained in recovery')
            restored=app.rpc('workspace/worktree/restore',{'cwd':str(workspace),'id':tree['id']})['worktree']
            check((target/'user-data').read_text()=='preserve me' and (target/'setup-marker').exists() and not (target/'cleanup-created').exists(),'restore did not recover the pre-cleanup files')
            output=app.rpc('command/exec',{'cwd':str(target),'command':'printf %s "$MC_ENV_READY"'})['output'];check('ready' in output,'restore lost the worktree shell environment')
            save(workspace,selected,config('exit 9'))
            failed=app.rpc('workspace/worktree/create',{'cwd':str(workspace),'localEnvironmentConfigPath':str(selected)});failed_target=Path(failed['worktree']['path']);failed_run=wait_run(failed['setupRun'])
            check(failed_run['status']=='failed' and failed_target.is_dir(),'failed setup removed the created checkout')
            mapped_failed=Path(app.rpc('workspace/environment/selection/read',{'cwd':str(failed_target)})['configPath'])
            save(failed_target,mapped_failed,config('export MC_ENV_READY=fixed; printf repaired > repaired','(while :; do printf x >> cleanup-loop; sleep .02; done) & wait'))
            retry=app.rpc('workspace/environment/run',{'cwd':str(failed_target),'configPath':str(mapped_failed),'sourceTreePath':str(workspace)})
            check(wait_run(retry['run'])['status']=='succeeded' and (failed_target/'repaired').read_text()=='repaired','repaired setup could not be retried in the same checkout')
            pending=app.send('workspace/worktree/archive',{'cwd':str(workspace),'id':failed['worktree']['id']});deadline=time.monotonic()+timeout
            while True:
                latest=app.rpc('workspace/environment/status',{'cwd':str(failed_target)})['run']
                if latest and latest['scriptType']=='cleanup' and latest['status']=='running':break
                f.require(time.monotonic()<deadline,'cleanup did not start');time.sleep(.03)
            listing=app.rpc('workspace/worktree/list',{'cwd':str(workspace)})
            check(any(item['id']==failed['worktree']['id'] and item['state']=='archiving' for item in listing['data']),'cleanup held the registry lock or lost its archive state')
            app.rpc('workspace/environment/cancel',{'id':latest['id']})
            try:app.wait(pending);raise AssertionError('cancelled cleanup still archived the checkout')
            except f.RpcFailure as error:check('cancel' in str(error).lower() and failed_target.is_dir(),'cancelled cleanup did not retain the checkout')
            writer=failed_target/'cleanup-loop';before=writer.stat().st_size if writer.exists() else 0;time.sleep(.15)
            check(not writer.exists() or writer.stat().st_size==before,'cancelled cleanup left a descendant writing')
            save(workspace,selected,'invalid = [')
            invalid=app.rpc('workspace/worktree/create',{'cwd':str(workspace),'localEnvironmentConfigPath':str(selected)})
            check(invalid.get('setupError') and invalid['setupRun']['status']=='failed' and Path(invalid['worktree']['path']).exists(),'parse failure did not retain a repairable worktree and failed setup record')
            persisted=invalid['setupRun']['id'];rpcs+=app.rpc_count
        finally:app.close()
        app=f.AppServer(binary,home,workspace,timeout)
        try:
            app.initialize(home);result=app.rpc('workspace/environment/status',{'id':persisted})['run'];check(result['status']=='failed' and result['error'],'restart lost the setup failure and repair context');rpcs+=app.rpc_count
        finally:app.close()
        print(f'environment smoke: {checks} checks, {rpcs} RPCs; real Git/Bash, no model calls; temporary data removed')

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=Path('target/debug/mcodex'));parser.add_argument('--timeout',type=float,default=30);args=parser.parse_args();run(args.binary.resolve(),args.timeout)
