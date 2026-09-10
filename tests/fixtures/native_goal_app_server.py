#!/usr/bin/env python3
"""Deterministic Codex 0.153.0 wire fixture; no model, credentials or live DB."""
import json
import os
from pathlib import Path
import sys
import time

root = Path(os.environ['GOAL_FIXTURE_DIR'])
order = os.environ['GOAL_FIXTURE_ORDER']

def emit(value):
    print(json.dumps(value), flush=True)

def notify(method, params):
    emit({'method': method, 'params': params})

for line in sys.stdin:
    req = json.loads(line)
    method = req.get('method')
    if 'id' not in req:
        continue
    result = {}
    if method == 'initialize':
        result = {'userAgent': 'fixture', 'codexHome': str(root), 'platformFamily': 'unix', 'platformOs': 'linux'}
    elif method == 'thread/start':
        result = {'thread': {'id': 'fixture-thread'}}
    elif method in ('thread/goal/set', 'turn/start'):
        params = req['params']
        is_goal = method == 'thread/goal/set'
        content = params['objective'] if is_goal else params['input'][0]['text']
        with (root / 'requests.jsonl').open('a') as log:
            log.write(json.dumps(req) + '\n')
        recovered = (root / 'external-fixed').exists()
        if is_goal:
            assert params['status'] == 'active'
        turn = {'id': 'turn-1', 'status': 'completed'}
        goal = {'threadId': 'fixture-thread', 'turnId': 'turn-1', 'goal': {
            'threadId': 'fixture-thread', 'objective': content, 'status': 'complete' if recovered else 'blocked',
            'tokenBudget': None, 'tokensUsed': 100, 'timeUsedSeconds': 3, 'createdAt': 1, 'updatedAt': 2}}
        emit({'id': req['id'], 'result': {'goal': goal['goal']} if is_goal else {'turn': turn}})
        if order == 'continuity_required':
            notify('item/agentMessage/delta', {'threadId': 'fixture-thread', 'delta': 'Synthetic prior output'})
            notify('error', {'threadId': 'fixture-thread', 'error': {'message': 'codex_continuity_missing: verified native binding unavailable'}, 'willRetry': False})
            continue
        if is_goal and order == 'before_started':
            notify('thread/goal/updated', goal)
        notify('turn/started', {'threadId': 'fixture-thread', 'turn': {'id': 'turn-1'}})
        if is_goal and not recovered:
            (root / 'started').touch()
            while not (root / 'release').exists():
                time.sleep(0.01)
        if is_goal and order == 'before':
            notify('thread/goal/updated', goal)
        text = ('Recovered same objective' if recovered else 'Blocked: external node unavailable; evidence retained') if is_goal else 'ACK: ' + content
        if order != 'before_started':
            notify('item/agentMessage/delta', {'threadId': 'fixture-thread', 'turnId': 'turn-1', 'itemId': 'final', 'delta': text})
        notify('item/completed', {'threadId': 'fixture-thread', 'turnId': 'turn-1', 'item': {'id': 'final', 'type': 'agentMessage', 'text': text}})
        notify('turn/completed', {'threadId': 'fixture-thread', 'turn': turn})
        if is_goal and order == 'after':
            notify('thread/goal/updated', goal)
        continue
    elif method == 'thread/goal/clear':
        (root / 'unexpected-clear').touch()
    emit({'id': req['id'], 'result': result})
