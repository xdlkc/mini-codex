#!/usr/bin/env python3
"""Fail-closed distribution tests. All Apple/network commands use local test doubles."""
import json,os,subprocess,sys,tempfile,unittest
from pathlib import Path
ROOT=Path(__file__).resolve().parent.parent
APP='Developer ID Application: Fixture (TESTTEAM)'
INSTALLER='Developer ID Installer: Fixture (TESTTEAM)'
MOCK=r'''
import json,os,pathlib,sys
name=pathlib.Path(sys.argv[0]).name;args=sys.argv[1:]
with open(os.environ['COMMAND_LOG'],'a') as f:f.write(json.dumps([name,*args])+'\n')
if name=='security':
 if os.environ.get('IDENTITIES')=='present':print('1) AAA "'+os.environ['MACOS_APP_SIGN_IDENTITY']+'"\n2) BBB "'+os.environ['MACOS_INSTALLER_SIGN_IDENTITY']+'"')
 else:print('0 valid identities found')
elif name=='codesign' and '--display' in args:
 print('Authority=Developer ID Application: Fixture (TESTTEAM)\nflags=0x10000(runtime)',file=sys.stderr)
elif name=='xcrun' and args[:2]==['notarytool','submit']:
 print(json.dumps({'status':os.environ.get('NOTARY_STATUS','Accepted'),'id':'fixture-submission','message':'Accepted'}))
 if os.environ.get('SUBMIT_FAIL'):sys.exit(1)
elif name=='xcrun' and args[:2]==['stapler','validate'] and os.environ.get('STAPLER_FAIL'):sys.exit(1)
elif name=='spctl' and os.environ.get('GATEKEEPER_FAIL'):sys.exit(1)
'''
class Signing(unittest.TestCase):
 def setUp(self):
  self.temp=tempfile.TemporaryDirectory(prefix='mini-codex-signing-test-');self.addCleanup(self.temp.cleanup);self.root=Path(self.temp.name);self.bin=self.root/'bin';self.bin.mkdir();self.log=self.root/'commands.jsonl'
  for name in ['security','codesign','xcrun','ditto','pkgutil','spctl']:
   p=self.bin/name;p.write_text('#!'+sys.executable+'\n'+MOCK);p.chmod(0o755)
  self.env={k:v for k,v in os.environ.items() if not k.startswith('MACOS_')};self.env.update(PATH=str(self.bin)+':/usr/bin:/bin',COMMAND_LOG=str(self.log),MACOS_SIGNING_MODE='developer-id',MACOS_APP_SIGN_IDENTITY=APP,MACOS_INSTALLER_SIGN_IDENTITY=INSTALLER,MACOS_NOTARY_PROFILE='fixture-profile',IDENTITIES='present')
 def call(self,expression):return subprocess.run(['/bin/bash','-c','set -euo pipefail; source "$1"; '+expression,'test',str(ROOT/'scripts/lib/macos-signing.sh')],env=self.env,text=True,capture_output=True)
 def commands(self):return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []
 def test_missing_identity_and_private_key_stop_preflight(self):
  self.env.pop('MACOS_APP_SIGN_IDENTITY');r=self.call('macos_distribution_preflight 0');self.assertNotEqual(r.returncode,0);self.assertEqual(self.commands(),[])
  self.env['MACOS_APP_SIGN_IDENTITY']=APP;self.env['IDENTITIES']='absent';r=self.call('macos_distribution_preflight 0');self.assertNotEqual(r.returncode,0);self.assertFalse(any(c[:2]==['xcrun','notarytool'] for c in self.commands()))
 def test_installer_requires_installer_certificate(self):
  self.env['MACOS_INSTALLER_SIGN_IDENTITY']=APP;r=self.call('macos_distribution_preflight 0');self.assertNotEqual(r.returncode,0);self.assertIn('Developer ID Installer',r.stderr)
 def test_valid_preflight_and_hardened_signing(self):
  r=self.call('macos_distribution_preflight 0; macos_sign_app fixture.app');self.assertEqual(r.returncode,0,r.stderr)
  signing=next(c for c in self.commands() if c[0]=='codesign' and '--force' in c);self.assertIn('--timestamp',signing);self.assertIn('runtime',signing);self.assertNotIn('--deep',signing)
 def test_only_accepted_submissions_are_stapled(self):
  for status in ['Invalid','In Progress','Rejected','']:
   self.log.unlink(missing_ok=True);self.env['NOTARY_STATUS']=status;r=self.call('macos_notarize fixture.dmg "'+str(self.root/'reports')+'"');self.assertNotEqual(r.returncode,0,status);self.assertFalse(any(c[:2]==['xcrun','stapler'] for c in self.commands()),status)
 def test_timeout_does_not_staple_or_automatically_resubmit(self):
  self.env['SUBMIT_FAIL']='1';r=self.call('macos_notarize fixture.dmg "'+str(self.root/'reports')+'"');self.assertNotEqual(r.returncode,0);self.assertEqual(sum(c[:3]==['xcrun','notarytool','submit'] for c in self.commands()),1);self.assertFalse(any(c[:2]==['xcrun','stapler'] for c in self.commands()))
 def test_accepted_then_staple_then_validate(self):
  r=self.call('macos_notarize fixture.dmg "'+str(self.root/'reports')+'"');self.assertEqual(r.returncode,0,r.stderr)
  calls=self.commands();self.assertEqual([c[1:3] for c in calls if c[0]=='xcrun'],[['notarytool','submit'],['stapler','staple'],['stapler','validate']])
 def test_staple_and_gatekeeper_failure_stop_distribution(self):
  self.env['STAPLER_FAIL']='1';r=self.call('macos_notarize fixture.dmg "'+str(self.root/'reports')+'"; macos_assess_app fixture.app');self.assertNotEqual(r.returncode,0);self.assertFalse(any(c[0]=='spctl' for c in self.commands()))
  self.env.pop('STAPLER_FAIL');self.env['GATEKEEPER_FAIL']='1';r=self.call('macos_assess_installers fixture.dmg fixture.pkg');self.assertNotEqual(r.returncode,0)
 def test_ad_hoc_cannot_enter_notarization(self):
  self.env['MACOS_SIGNING_MODE']='ad-hoc';r=self.call('macos_notarize fixture.dmg "'+str(self.root/'reports')+'"');self.assertNotEqual(r.returncode,0);self.assertEqual(self.commands(),[])
 def test_unverified_manifest_cannot_publish(self):
  dist=self.root/'dist';dist.mkdir();(dist/'distribution-manifest.json').write_text(json.dumps({'signingMode':'ad-hoc','notarized':False,'gatekeeperVerified':False}))
  r=subprocess.run(['/bin/bash',str(ROOT/'scripts/publish-macos-release.sh'),str(dist)],env=self.env,text=True,capture_output=True);self.assertNotEqual(r.returncode,0);self.assertIn('Refusing publication',r.stderr);self.assertEqual(self.commands(),[])
if __name__=='__main__':unittest.main()
