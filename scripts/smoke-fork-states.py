#!/usr/bin/env python3
"""Fork real chats in the Git states verified by the reference-sequence probe."""
from __future__ import annotations
import argparse,importlib.util,json,sys,tempfile
from pathlib import Path
sys.dont_write_bytecode=True
def module(name,file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file));value=importlib.util.module_from_spec(spec);sys.modules[name]=value;spec.loader.exec_module(value);return value
f=module('fork_state_transport','smoke-desktop.py');reference=module('fork_state_reference','probe-reference-worktree-copy.py')

def run(binary,timeout):
    checks=0;rpcs=0
    def check(value,message):
        nonlocal checks
        f.require(value,message);checks+=1
    with tempfile.TemporaryDirectory(prefix='.mcodex-fork-states-',dir=Path.cwd()) as temporary:
        root=Path(temporary).resolve()
        for case in ['intent-to-add','assume-unchanged','skip-worktree','sparse-index','unmerged','split-index','uninitialized-submodule','initialized-submodule','changed-submodule-commit','embedded-unborn','embedded-committed','external-attributes','untracked-cwd','normalization']:
            folder=root/case;repo=folder/'repo';reference.prepare(repo,case);
            chat_cwd=repo
            if case=='untracked-cwd':
                chat_cwd=repo/'untracked folder';chat_cwd.mkdir();(chat_cwd/'working.txt').write_bytes(b'local contents\n')
            profile=folder/'profile';profile.mkdir();index=(repo/'.git/index').read_bytes();head=reference.git(repo,'rev-parse','HEAD').stdout
            app=f.AppServer(binary,profile,repo,timeout)
            try:
                app.initialize(profile);thread=app.rpc('thread/start',{'cwd':str(chat_cwd),'mode':'codex'})['thread']['id'];
                if case=='embedded-committed':
                    app.expect_error('thread/fork',{'threadId':thread,'destination':'newWorktree'});check((repo/'.git/index').read_bytes()==index and (repo/'embedded/child.txt').read_bytes()==b'embedded bytes\n',case+': failed non-recursive copy changed source');rpcs+=app.rpc_count;continue
                result=app.rpc('thread/fork',{'threadId':thread,'destination':'newWorktree'});target=Path(result['cwd'])
                check(result['worktree']['ownerThreadId']==result['thread']['id'] and target.is_dir(),case+': worktree or chat ownership missing')
                check((repo/'.git/index').read_bytes()==index and reference.git(repo,'rev-parse','HEAD').stdout==head,case+': source index/HEAD changed')
                check(reference.git(target,'diff','--cached','--quiet',check=False).returncode==0,case+': copied staging flags')
                if case=='intent-to-add':check((target/'intent.txt').read_text()=='intent contents\n',case+': contents missing')
                elif case in ['assume-unchanged','skip-worktree']:check((target/'tracked.txt').read_text()=='base\n',case+': ignored index flag semantics differ')
                elif case=='sparse-index':check((target/'src/included.txt').read_text()=='sparse working change\n',case+': working change missing')
                elif case=='unmerged':check((target/'tracked.txt').read_bytes()==(repo/'tracked.txt').read_bytes(),case+': conflict markers differ')
                elif case=='split-index':check((target/'tracked.txt').read_text()=='split working change\n',case+': working change missing')
                elif case in ['uninitialized-submodule','initialized-submodule','changed-submodule-commit']:check((target/'module').is_dir() and not list((target/'module').iterdir()),case+': initialized a submodule')
                elif case=='embedded-unborn':check(not (target/'embedded').exists(),case+': unborn embedded repository was copied')
                elif case=='untracked-cwd':check(target.name=='untracked folder' and (target/'working.txt').read_bytes()==b'local contents\n',case+': chat directory was not copied')
                elif case=='external-attributes':check((target/'raw.crlf').read_bytes()==b'working\r\n',case+': external attributes changed copied bytes')
                elif case=='normalization':check((target/'raw.crlf').read_bytes()==b'working\n' and (target/'run.sh').stat().st_mode&0o777==0o755 and not (target/'empty directory').exists(),case+': Git attributes/mode/empty-directory rules differ')
                check(not app.rpc('thread/read',{'threadId':result['thread']['id'],'includeTurns':True})['thread']['turns'],case+': empty history was not preserved');rpcs+=app.rpc_count
            finally:app.close()
        print(f'fork-state smoke: {checks} checks, {rpcs} RPCs, 14 Git cases (13 reference sequences and an untracked chat directory); no model calls; temporary data removed')

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=Path('target/debug/mcodex'));parser.add_argument('--timeout',type=float,default=30);args=parser.parse_args();run(args.binary.resolve(),args.timeout)
