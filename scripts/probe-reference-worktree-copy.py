#!/usr/bin/env python3
"""Replay the installed Codex worker's isolated-index Git sequence in disposable repos.

Contract source: Codex 26.1013.40934 worker.js o0/NTe/o2. This runs the observed
Git sequence; it does not claim to exercise the original desktop UI or its gates.
"""
from __future__ import annotations
import json,os,shutil,subprocess,tempfile
from pathlib import Path

def git(cwd,*args,index=None,check=True):
    env={key:value for key,value in os.environ.items() if not key.startswith('GIT_')}
    env.update(GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL='/dev/null',GIT_OPTIONAL_LOCKS='0')
    if index is not None:env['GIT_INDEX_FILE']=str(index)
    command=['git','-C',str(cwd),'-c','core.hooksPath=/dev/null','-c','core.fsmonitor=false','-c','commit.gpgSign=false','-c','user.name=Reference fixture','-c','user.email=fixture@example.invalid','-c','attr.tree=','-c','core.attributesFile=',*args]
    result=subprocess.run(command,env=env,capture_output=True,timeout=20)
    if check and result.returncode:raise RuntimeError(result.stderr.decode(errors='replace'))
    return result

def index_copy(repo,folder):
    folder.mkdir();index=Path(git(repo,'rev-parse','--git-path','index').stdout.decode().strip());index=index if index.is_absolute() else repo/index
    shutil.copyfile(index,folder/'index')
    shared=git(repo,'rev-parse','--shared-index-path').stdout.decode().strip()
    if shared:
        shared=Path(shared);shared=shared if shared.is_absolute() else repo/shared;shutil.copyfile(shared,folder/shared.name)
    return folder/'index'

def prepare(repo,case):
    repo.mkdir(parents=True);git(repo,'init','--template=','-b','main');(repo/'src').mkdir();(repo/'other').mkdir()
    for name in ['tracked.txt','src/included.txt','other/excluded.txt']:(repo/name).write_text('base\n')
    git(repo,'add','.');git(repo,'commit','-m','fixture')
    if case=='intent-to-add':(repo/'intent.txt').write_text('intent contents\n');git(repo,'add','-N','intent.txt')
    elif case in ['assume-unchanged','skip-worktree']:git(repo,'update-index','--'+case,'tracked.txt');(repo/'tracked.txt').write_text('hidden working change\n')
    elif case=='sparse-index':git(repo,'sparse-checkout','init','--cone','--sparse-index');git(repo,'sparse-checkout','set','src');(repo/'src/included.txt').write_text('sparse working change\n')
    elif case=='unmerged':
        git(repo,'switch','-c','other-branch');(repo/'tracked.txt').write_text('right\n');git(repo,'commit','-am','right');git(repo,'switch','main');(repo/'tracked.txt').write_text('left\n');git(repo,'commit','-am','left');assert git(repo,'merge','other-branch',check=False).returncode
    elif case=='split-index':git(repo,'update-index','--split-index');(repo/'tracked.txt').write_text('split working change\n')
    elif case=='uninitialized-submodule':
        commit=git(repo,'rev-parse','HEAD').stdout.decode().strip();git(repo,'update-index','--add','--cacheinfo','160000,'+commit+',module');git(repo,'commit','-m','gitlink');(repo/'module').mkdir()
    elif case in ['embedded-unborn','embedded-committed']:
        child=repo/'embedded';child.mkdir();git(child,'init','--template=','-b','main');(child/'child.txt').write_text('embedded bytes\n')
        if case=='embedded-committed':git(child,'add','.');git(child,'commit','-m','embedded')
    elif case=='external-attributes':
        attributes=repo.parent/'external-attributes';attributes.write_text('*.crlf text eol=lf\n');git(repo,'config','core.attributesFile',str(attributes));(repo/'raw.crlf').write_bytes(b'working\r\n');git(repo,'add','raw.crlf');(repo/'raw.crlf').write_bytes(b'working\r\n')
    elif case=='normalization':
        (repo/'.gitattributes').write_text('*.crlf text eol=lf\n');(repo/'raw.crlf').write_bytes(b'base\n');(repo/'run.sh').write_text('base\n');(repo/'run.sh').chmod(0o755);git(repo,'add','.');git(repo,'commit','-m','attributes');(repo/'raw.crlf').write_bytes(b'working\r\n');(repo/'run.sh').write_text('working\n');(repo/'run.sh').chmod(0o750);(repo/'empty directory').mkdir()
    elif case in ['initialized-submodule','changed-submodule-commit']:
        child=repo.parent/'module-source';child.mkdir();git(child,'init','--template=','-b','main');(child/'child.txt').write_text('base\n');git(child,'add','.');git(child,'commit','-m','module');git(repo,'-c','protocol.file.allow=always','submodule','add',str(child),'module');git(repo,'commit','-am','module');(repo/'module/child.txt').write_text('module working change\n')
        if case=='changed-submodule-commit':git(repo/'module','commit','-am','module change')

def run():
    results=[]
    with tempfile.TemporaryDirectory(prefix='mini-codex-reference-worktrees-') as temporary:
        root=Path(temporary).resolve()
        for case in ['intent-to-add','assume-unchanged','skip-worktree','sparse-index','unmerged','split-index','uninitialized-submodule','initialized-submodule','changed-submodule-commit','embedded-unborn','embedded-committed','external-attributes','normalization']:
            folder=root/case;repo=folder/'repo';prepare(repo,case)
            original_index=repo/'.git/index';before=original_index.read_bytes();private=index_copy(repo,folder/'source-index');git(repo,'add','-u',index=private)
            patch=git(repo,'diff','--no-ext-diff','--no-textconv','--color=never','--src-prefix=a/','--dst-prefix=b/','--binary','--cached',index=private).stdout
            target=folder/'target';git(repo,'worktree','add','--detach',str(target),'HEAD');applied=True;error=None
            if patch:
                patch_file=folder/'patch.diff';patch_file.write_bytes(patch);target_index=index_copy(target,folder/'target-index');result=git(target,'apply','--3way',str(patch_file),index=target_index,check=False);applied=result.returncode==0;error=result.stderr.decode(errors='replace') if not applied else None
            assert original_index.read_bytes()==before,'reference sequence changed the original index'
            entry={'case':case,'applied':applied,'sourceIndexUnchanged':True,'targetIndexClean':git(target,'diff','--cached','--quiet',check=False).returncode==0}
            if error:entry['error']=error
            if case.startswith('embedded-'):
                paths=git(repo,'ls-files','--others','--exclude-standard','-z').stdout.decode().strip('\0').split('\0');errors=[]
                for path in paths:
                    if not path:continue
                    if path.endswith('/'):
                        if git(repo/path,'rev-parse','--verify','--quiet','HEAD^{commit}',check=False).returncode==1:continue
                        path=path[:-1]
                    (target/path).parent.mkdir(parents=True,exist_ok=True)
                    result=subprocess.run(['node','-e',"require('node:fs/promises').cp(process.argv[1],process.argv[2],{recursive:false,verbatimSymlinks:true}).catch(e=>{console.error(e.code);process.exitCode=1})",str(repo/path),str(target/path)],capture_output=True)
                    if result.returncode:errors.append(result.stderr.decode().strip())
                entry.update(copyErrors=errors,embeddedCopied=(target/'embedded').exists())
            if case=='intent-to-add':entry['copiedIntent']=(target/'intent.txt').read_text() if (target/'intent.txt').exists() else None
            if case in ['assume-unchanged','skip-worktree']:entry['targetTracked']=(target/'tracked.txt').read_text()
            if case=='sparse-index':entry['copiedSparse']=(target/'src/included.txt').read_text();entry['sparseEnabled']=git(target,'config','--get','core.sparseCheckout',check=False).stdout.decode().strip()
            if case=='unmerged':entry['copiedConflictMarkers']='<<<<<<<' in (target/'tracked.txt').read_text()
            if case=='split-index':entry['copiedSplit']=(target/'tracked.txt').read_text()
            if case=='external-attributes':entry['rawFile']=list((target/'raw.crlf').read_bytes())
            if case=='normalization':entry.update(rawFile=list((target/'raw.crlf').read_bytes()),executableMode=oct((target/'run.sh').stat().st_mode&0o777),emptyDirectoryCopied=(target/'empty directory').exists())
            results.append(entry)
    print(json.dumps(results,ensure_ascii=False,indent=2))

if __name__=='__main__':run()
