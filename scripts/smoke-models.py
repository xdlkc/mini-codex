#!/usr/bin/env python3
"""Exercise real model catalog RPCs against a loopback provider; no real credentials."""
import argparse
import base64
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('model_account_helpers', Path(__file__).with_name('smoke-account.py'))
helpers = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = helpers
spec.loader.exec_module(helpers)
require = helpers.require


def run(binary):
    requests = []
    state = {'mode': 'api'}
    slow_opened, slow_release = threading.Event(), threading.Event()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            requests.append({'path': self.path, 'authorization': self.headers.get('Authorization'), 'account': self.headers.get('chatgpt-account-id')})
            require(urlparse(self.path).path == '/v1/models', 'unexpected model endpoint')
            mode = state['mode']
            if mode == 'slow':
                slow_opened.set()
                require(slow_release.wait(6), 'slow model fixture was not released')
            status = 200
            if mode == 'http-error':
                status, body = 403, {'error': {'message': 'never reflect sk-local-fixture-secret'}}
            elif mode == 'invalid':
                body = {'unrelated': []}
            elif mode == 'codex':
                body = {'models': [{'slug': 'reported-model', 'display_name': 'Reported model', 'visibility': 'list', 'default_reasoning_level': 'high', 'supported_reasoning_levels': [{'effort': 'low', 'description': 'Reported low'}, {'effort': 'high', 'description': 'Reported high'}, {'effort':'ultra','description':'Reported Ultra'}], 'context_window': 100000, 'service_tiers': [{'id': 'priority', 'name': 'Fast'}]}]}
            else:
                owner = 'second-account' if self.headers.get('Authorization') == 'Bearer sk-local-fixture-two' else 'first-account'
                body = {'data': [{'id': owner + '-model', 'owned_by': owner}, {'id': 'text-embedding-3-small', 'owned_by': 'openai'}]}
            raw = json.dumps(body).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(4 * 1024 * 1024 + 1 if mode == 'oversized' else len(raw)))
            self.end_headers()
            if mode != 'oversized':
                self.wfile.write(raw)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    base = f'http://127.0.0.1:{server.server_port}/v1'
    total = 0
    try:
        with tempfile.TemporaryDirectory(prefix='mcodex-models-') as temp:
            root = Path(temp)
            store, workspace, home = [root / part for part in ['store', 'workspace', 'home']]
            for path in [store, workspace, home]:
                path.mkdir()
            (store / 'config.toml').write_text('model = "selected-custom"\nmodels = ["another-custom"]\nmodel_reasoning_effort = "medium"\n')
            app = helpers.AppServer(binary, store, workspace, home, base, 8)
            try:
                app.rpc('initialize')
                catalog = app.rpc('model/list')
                require(catalog['catalog']['available'] is False and not requests, 'signed-out discovery made a provider request')
                require(catalog['defaultModel'] == 'selected-custom', 'configured selection changed')
                app.rpc('account/login/start', {'type': 'apiKey', 'apiKey': 'sk-local-fixture-one'})
                app.pump_until(lambda: any(n.get('method') == 'account/updated' for n in app.notifications), 'account connection event')
                catalog = app.rpc('model/list')
                require(catalog['catalog']['source'] == 'provider' and catalog['catalog']['available'], 'live catalog did not load')
                model = next(m for m in catalog['data'] if m['id'] == 'first-account-model')
                require(model['supportedReasoningEfforts'] == [] and model['responsesSupport'] == 'unknown', 'basic models metadata invented capabilities')
                count = len(requests)
                app.rpc('model/list')
                require(len(requests) == count, 'fresh catalog cache missed')
                print('PASS signed-out fallback, live discovery and same-account cache', flush=True)

                for mode, error in [('http-error', 'HTTP 403'), ('invalid', 'invalid model catalog'), ('oversized', 'too large')]:
                    state['mode'] = mode
                    catalog = app.rpc('model/list', {'refresh': True})
                    require(catalog['catalog']['stale'] and error in catalog['catalog']['error'], 'failed refresh lost cached models or error')
                    require('sk-local-fixture' not in json.dumps(catalog), 'provider body or credential leaked')
                state['mode'] = 'api'
                app.rpc('model/list', {'refresh': True})
                print('PASS failed, malformed and oversized refreshes retain clearly stale catalog', flush=True)

                app.notifications.clear()
                app.rpc('account/login/start', {'type': 'apiKey', 'apiKey': 'sk-local-fixture-two'})
                app.pump_until(lambda: any(n.get('method') == 'account/updated' for n in app.notifications), 'second account event')
                catalog = app.rpc('model/list')
                ids = [m['id'] for m in catalog['data']]
                require('second-account-model' in ids and 'first-account-model' not in ids, 'another account reused the previous catalog')
                def send(method, params=None):
                    app.next_id += 1
                    request_id = app.next_id
                    app.process.stdin.write(json.dumps({'id': request_id, 'method': method, 'params': params or {}}) + '\n')
                    app.process.stdin.flush()
                    app.rpc_count += 1
                    return request_id
                state['mode'] = 'slow'
                loading = send('model/list', {'refresh': True})
                require(slow_opened.wait(3), 'model refresh did not reach the slow provider')
                queued = send('model/list', {'refresh': True})
                logout = send('account/logout')
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline and not json.loads((store / 'auth.json').read_text()).get('signed_out'):
                    time.sleep(0.01)
                require(json.loads((store / 'auth.json').read_text()).get('signed_out'), 'logout did not invalidate credentials before model refresh finished')
                before_release = len(requests)
                slow_release.set()
                app.pump_until(lambda: loading in app.replies and queued in app.replies and logout in app.replies, 'catalog/logout race')
                require('account changed' in app.replies.pop(loading).get('error', {}).get('message', '').lower(), 'late model refresh was published after logout')
                queued_result = app.replies.pop(queued)
                require('error' not in queued_result and not queued_result['result']['catalog']['available'], 'queued refresh did not reload the signed-out account')
                require(len(requests) == before_release, 'queued refresh sent old credentials after logout')
                require('error' not in app.replies.pop(logout), 'logout did not finish after catalog cancellation')
                count = len(requests)
                catalog = app.rpc('model/list', {'refresh': True})
                require(not catalog['catalog']['available'] and len(requests) == count, 'logout retained provider models or contacted provider')
                require(all(m['source'] == 'configured' for m in catalog['data']), 'logout did not remove account-specific catalog')
                print('PASS credential replacement and concurrent logout isolate cached catalog', flush=True)
                total += app.rpc_count
            finally:
                app.close()

            payload = base64.urlsafe_b64encode(json.dumps({'exp': 4102444800}).encode()).decode().rstrip('=')
            (store / 'auth.json').write_text(json.dumps({'tokens': {'access_token': 'fixture.' + payload + '.signature', 'refresh_token': 'unused-local-refresh', 'account_id': 'local-account'}}))
            os.chmod(store / 'auth.json', 0o600)
            state['mode'] = 'codex'
            app = helpers.AppServer(binary, store, workspace, home, base, 8)
            try:
                app.rpc('initialize')
                catalog = app.rpc('model/list')
                model = next(m for m in catalog['data'] if m['id'] == 'reported-model')
                require([v['reasoningEffort'] for v in model['supportedReasoningEfforts']] == ['low', 'high', 'ultra'], 'Codex reported effort metadata changed')
                require(model['defaultReasoningEffort'] == 'high' and model['contextWindow'] == 100000 and model['supportedServiceTiers'][0]['id'] == 'priority', 'Codex model metadata missing')
                require(parse_qs(urlparse(requests[-1]['path']).query).get('client_version') == ['0.163.0'], 'Codex discovery did not send client version')
                require(requests[-1]['account'] == 'local-account', 'Codex discovery did not scope account')
                require(model['serviceTiers'] == model['supportedServiceTiers'], 'Codex serviceTiers contract is missing')
                app.rpc('settings/write', {'model':'reported-model','reasoningEffort':'ultra','serviceTier':'fast'})
                configured=app.rpc('model/list')
                require(configured['defaultReasoningEffort']=='ultra' and configured['serviceTier']=='priority','Ultra or Fast did not persist as Codex values')
                before=(store/'config.toml').read_bytes()
                try:
                    app.rpc('settings/write', {'model':'reported-model','reasoningEffort':'max'})
                    raise AssertionError('unreported model effort was accepted')
                except Exception as error:
                    require('not supported' in str(error),'wrong validation failure')
                require((store/'config.toml').read_bytes()==before,'invalid effort changed saved settings')
                print('PASS Codex catalog preserves provider reasoning, context and service-tier metadata', flush=True)
                total += app.rpc_count
            finally:
                app.close()
    finally:
        server.shutdown()
        server.server_close()
        worker.join(2)
    print(f'PASS model catalog smoke: 4 groups, {total} RPCs, {len(requests)} loopback requests; no real provider; temporary state removed', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path(__file__).resolve().parents[1] / 'target/debug/mcodex')
    args = parser.parse_args()
    try:
        run(args.binary.resolve())
    except Exception as error:
        print(f'FAIL model catalog smoke: {error}', file=sys.stderr)
        raise SystemExit(1)
