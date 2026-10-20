#!/usr/bin/env python3
"""Read-only model/settings comparison with the installed Codex App backend.

No thread or turn is created. No inference request is sent. Mini runs with an
isolated state directory and reads only the normal Codex import sources.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time


class Client:
    def __init__(self, binary, env=None):
        self.process = subprocess.Popen([str(binary), 'app-server'], env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.lines = queue.Queue()
        self.sequence = 0
        def read():
            for line in self.process.stdout:
                self.lines.put(line)
            self.lines.put(None)
        threading.Thread(target=read, daemon=True).start()

    def request(self, method, params=None):
        self.sequence += 1
        self.process.stdin.write(json.dumps({'id': self.sequence, 'method': method, 'params': params or {}}) + '\n')
        self.process.stdin.flush()
        deadline = time.monotonic() + 35
        while time.monotonic() < deadline:
            try:
                line = self.lines.get(timeout=min(1, max(.01, deadline-time.monotonic())))
            except queue.Empty:
                continue
            if line is None:
                raise RuntimeError('App server closed during ' + method)
            message = json.loads(line)
            if message.get('id') == self.sequence:
                if 'error' in message:
                    raise RuntimeError('App server rejected ' + method)
                return message['result']
        raise TimeoutError(method)

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=8)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait(timeout=5)


def metadata(model):
    keys = ['model', 'displayName', 'hidden', 'defaultReasoningEffort', 'serviceTiers', 'defaultServiceTier']
    return {key: model.get(key) for key in keys} | {'supportedReasoningEfforts': [
        {key: item.get(key) for key in ['reasoningEffort', 'description']}
        for item in model['supportedReasoningEfforts']]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--codex-app-cli', type=Path, default=Path('/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex'))
    parser.add_argument('--mini-binary', type=Path, default=Path(__file__).resolve().parents[1]/'target/debug/mcodex')
    args = parser.parse_args()
    for binary in [args.codex_app_cli, args.mini_binary]:
        if not binary.is_file():
            parser.error('Binary not found: ' + str(binary))
    clients = []
    with tempfile.TemporaryDirectory(prefix='mcodex-model-parity-check-') as temporary:
        try:
            codex = Client(args.codex_app_cli)
            clients.append(codex)
            codex.request('initialize', {'clientInfo': {'name': 'mcodex_catalog_check', 'version': '1'}, 'capabilities': {'experimentalApi': True}})
            expected = codex.request('model/list', {'includeHidden': True, 'limit': 100})
            expected_config = codex.request('config/read', {'includeLayers': False})['config']
            env = {'PATH': os.environ.get('PATH','/usr/bin:/bin'), 'HOME': str(Path.home()),
                'CODEX_HOME': os.environ.get('CODEX_HOME', str(Path.home()/'.codex')),
                'MCODEX_HOME': temporary, 'MCODEX_NO_CODEX_IMPORT': '0', 'TOKIO_WORKER_THREADS': '2'}
            mini = Client(args.mini_binary.resolve(), env)
            clients.append(mini)
            mini.request('initialize')
            actual = mini.request('model/list', {'includeHidden': True})
            if [metadata(item) for item in actual['data']] != [metadata(item) for item in expected['data']]:
                raise AssertionError('Model order, visibility, reasoning or speed metadata differs from the installed Codex App')
            for actual_key, config_key in [('defaultModel','model'), ('defaultReasoningEffort','model_reasoning_effort'), ('serviceTier','service_tier')]:
                wanted = expected_config.get(config_key)
                if wanted is not None and actual.get(actual_key) != wanted:
                    raise AssertionError('Configured default differs: ' + config_key)
            print('PASS installed Codex App parity:', len(actual['data']), 'models,',
                sum(not item['hidden'] for item in actual['data']), 'visible;',
                actual['defaultModel'], actual['defaultReasoningEffort'], actual['serviceTier'])
        finally:
            for client in clients:
                client.close()


if __name__ == '__main__':
    main()
