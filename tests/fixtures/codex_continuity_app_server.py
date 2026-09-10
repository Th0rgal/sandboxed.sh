#!/usr/bin/env python3
"""Synthetic JSON-RPC process: no network, credentials, model or real commands."""
import json
import os
from pathlib import Path
import sys

root = Path(os.environ['CONTINUITY_FIXTURE_DIR'])
mode = os.environ.get('CONTINUITY_FIXTURE_MODE', 'normal')
state_path = root / 'native.json'
state = json.loads(state_path.read_text()) if state_path.exists() else {'starts': 0, 'goal': None}


def save():
    state_path.write_text(json.dumps(state))


def emit(value):
    print(json.dumps(value), flush=True)


def notify(method, params):
    emit({'method': method, 'params': params})


def started():
    notify('turn/started', {'threadId': state['id'], 'turn': {'id': 'active-turn', 'status': 'inProgress'}})
    (root / 'started').touch()


def finish():
    if state['goal']:
        state['goal']['status'] = 'blocked'
        state['goal']['tokensUsed'] += 7
        state['goal']['timeUsedSeconds'] += 2
        save()
        notify('thread/goal/updated', {'threadId': state['id'], 'turnId': 'active-turn', 'goal': state['goal']})
    notify('item/completed', {'threadId': state['id'], 'turnId': 'active-turn',
                              'item': {'id': 'answer', 'type': 'agentMessage', 'text': 'Synthetic checkpoint'}})
    notify('turn/completed', {'threadId': state['id'], 'turn': {'id': 'active-turn', 'status': 'completed'}})


with (root / 'pids').open('a') as out:
    out.write(str(os.getpid()) + '\n')

for line in sys.stdin:
    req = json.loads(line)
    if 'id' not in req:
        continue
    method, params = req['method'], req.get('params', {})
    with (root / 'requests.jsonl').open('a') as out:
        out.write(json.dumps({'method': method, 'params': params}) + '\n')
    if ((mode == 'init-error' and method == 'initialize')
            or (mode == 'resume-error' and method == 'thread/resume')
            or (mode == 'goal-error' and method == 'thread/goal/get')
            or (mode == 'pause-error' and method == 'thread/goal/set' and params.get('status') == 'paused')):
        emit({'id': req['id'], 'error': {'code': -32000, 'message': 'synthetic unavailable'}})
        continue
    result = {}
    if method == 'initialize':
        result = {'userAgent': 'fixture', 'codexHome': str(root / '.codex'), 'platformFamily': 'unix', 'platformOs': 'linux'}
    elif method in ('thread/start', 'thread/resume'):
        if method == 'thread/start':
            state['starts'] += 1
            state.update(id='native-thread-' + str(state['starts']), cwd=params['cwd'], goal=None)
            save()
        else:
            assert params['threadId'] == state['id']
            if mode in ('restored-goal-hint', 'restored-goal-stopped'):
                # Real CLI 0.153.0 publishes the restored goal at resume,
                # before goal/get and the subsequent explicit activation.
                notify('thread/goal/updated', {'threadId': state['id'], 'goal': state['goal']})
        active = mode in ('active', 'active-no-id')
        turns = [{'id': 'active-turn', 'status': 'inProgress'}] if mode == 'active' else []
        if method == 'thread/resume' and mode == 'goal-snapshot':
            turns = [{'id': 'active-turn', 'status': 'completed', 'items': [{'id': 'snapshot-answer', 'type': 'agentMessage', 'text': 'Retained checkpoint receipt'}]}]
        result = {'thread': {'id': state['id'], 'cwd': '/wrong' if mode == 'cwd-mismatch' else state['cwd'],
                             'status': {'type': 'active' if active else 'idle'},
                             'turns': turns}}
    elif method == 'thread/goal/get':
        result = {'goal': state['goal']}
    elif method == 'thread/goal/set':
        if 'objective' in params:
            state['goal'] = {'threadId': state['id'], 'objective': params['objective'],
                             'status': params['status'], 'tokenBudget': 100, 'tokensUsed': 0,
                             'timeUsedSeconds': 0, 'createdAt': 1, 'updatedAt': 1}
        else:
            assert 'tokenBudget' not in params
            state['goal']['status'] = params['status']
        save()
        result = {'goal': state['goal']}
    elif method == 'thread/goal/clear':
        raise AssertionError('continuity must not clear native goals')
    elif method == 'turn/start':
        state.setdefault('inputs', []).append(params['input'][0]['text'])
        save()
    elif method == 'turn/steer':
        assert params['expectedTurnId'] == 'active-turn'
        state.setdefault('hints', []).append(params['input'][0]['text'])
        save()
    emit({'id': req['id'], 'result': result})
    if method in ('turn/start', 'thread/goal/set') and (method == 'turn/start' or params['status'] == 'active'):
        if mode == 'restored-goal-stopped':
            # A real stop after activation, before a turn starts, must survive.
            state['goal']['status'] = 'blocked'
            save()
            notify('thread/goal/updated', {'threadId': state['id'], 'goal': state['goal']})
            continue
        started()
        if mode in ('goal-snapshot', 'goal-snapshot-no-turn'):
            state['goal']['status'] = 'blocked'
            state['goal']['tokensUsed'] = 7
            save()
            os._exit(0)
        if mode == 'crash-tool':
            notify('item/started', {'threadId': state['id'], 'turnId': 'active-turn',
                                    'item': {'id': 'unknown-tool', 'type': 'commandExecution', 'command': 'synthetic-do-once'}})
            os._exit(0)
        if mode == 'crash-once':
            os._exit(0)
        if mode not in ('hold', 'hint', 'pause-error', 'restored-goal-hint'):
            finish()
    elif method == 'turn/steer':
        finish()
    elif method == 'thread/resume' and mode == 'crash-once':
        started()
        finish()
    elif method == 'thread/resume' and mode == 'crash-tool':
        os._exit(0)
