#!/usr/bin/env python3
"""Generate and persist titles through the real Rust service and a loopback model."""
import argparse,importlib.util,json,tempfile,sys
from pathlib import Path
sys.dont_write_bytecode=True
ROOT=Path(__file__).resolve().parent.parent
spec=importlib.util.spec_from_file_location('title_turn_fixture',ROOT/'scripts/smoke-turns.py');f=importlib.util.module_from_spec(spec);sys.modules[spec.name]=f;spec.loader.exec_module(f)
def run(binary):
 with tempfile.TemporaryDirectory(prefix='mini-codex-title-smoke-') as directory:
  root=Path(directory);home=root/'profile';home.mkdir();cwd=root/'workspace';cwd.mkdir();mock=f.MockResponses(20);app=f.AppServer(binary,home,cwd,mock.base_url,20)
  try:
   app.rpc('initialize',{'clientInfo':{'name':'title-smoke','version':'1'}})
   thread=app.rpc('thread/start',{'mode':'codex'})['thread']['id'];prompt='请让终端在会话底部打开，并且不覆盖文件面板'
   mock.plan('success','你好，终端布局已处理。');turn=app.turn(thread,[{'type':'text','text':prompt},{'type':'attachment','name':'private-fixture.txt','text':'Attachment bytes must not enter the title request'}]);app.assert_chinese_completion(thread,turn,'你好，终端布局已处理。');mock.request()
   mock.plan('success',json.dumps({'title':'添加底部会话终端','note':'你好'},ensure_ascii=False));result=app.rpc('thread/title/generate',{'threadId':thread});assert result['status']=='generated' and result['title']=='添加底部会话终端',result
   wire=mock.request();assert not wire.get('tools'),wire;assert wire.get('max_output_tokens')==512;assert 'Attachment bytes' not in json.dumps(wire,ensure_ascii=False);assert prompt in json.dumps(wire,ensure_ascii=False)
   read=app.rpc('thread/read',{'threadId':thread,'includeTurns':True})['thread'];assert read['name']=='添加底部会话终端',read
   notice=app.event('thread/name/updated',thread);assert notice['threadName']=='添加底部会话终端' and notice['source']=='automatic',notice
   result=app.rpc('thread/title/generate',{'threadId':thread});assert result['status']=='alreadyNamed';assert mock.requests.empty()
   app.rpc('thread/name/set',{'threadId':thread,'name':'我的手动标题'});result=app.rpc('thread/title/generate',{'threadId':thread});assert result['status']=='alreadyNamed';assert mock.requests.empty()
   assert app.rpc('thread/read',{'threadId':thread})['thread']['name']=='我的手动标题'
   other=app.rpc('thread/start',{'mode':'codex'})['thread']['id'];mock.plan('success','你好，第二个会话。');turn=app.turn(other,[{'type':'text','text':'修复设置页面'}]);app.assert_chinese_completion(other,turn,'你好，第二个会话。');mock.request()
   mock.plan('success','你好\ninvalid multiline title')
   try:app.rpc('thread/title/generate',{'threadId':other});raise AssertionError('invalid title accepted')
   except f.SmokeFailure as error:assert 'valid title' in str(error),error
   mock.request();assert not app.rpc('thread/read',{'threadId':other})['thread'].get('name');assert not mock.errors,mock.errors
   print(f'Title smoke passed: semantic title, no tools or attachment bytes, persistence, notification, no duplicate model request, manual rename protection and invalid response handling; {app.rpc_count} real RPCs')
  finally:app.close();mock.close()
if __name__=='__main__':
 parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=ROOT/'target/debug/mcodex');args=parser.parse_args();run(args.binary.resolve())
