import { describe, it, expect } from 'vitest';
import {
  isFinishedStatus,
  needsAttentionStatus,
  categorizeMission,
  categorizeMissions,
  finishedTone,
  getMissionDotColor,
  getMissionTextColor,
  FINISHED_STATUSES,
  NEEDS_ATTENTION_STATUSES,
} from './mission-status';
import type { MissionStatus } from './api/missions';

describe('mission-status', () => {
  describe('isFinishedStatus', () => {
    it('returns true for green finished statuses', () => {
      expect(isFinishedStatus('completed')).toBe(true);
      expect(isFinishedStatus('acknowledged')).toBe(true);
    });

    it('returns true for red (failure) statuses in Finished', () => {
      expect(isFinishedStatus('failed')).toBe(true);
      expect(isFinishedStatus('interrupted')).toBe(true);
      expect(isFinishedStatus('blocked')).toBe(true);
      expect(isFinishedStatus('not_feasible')).toBe(true);
    });

    it('returns false for non-finished statuses', () => {
      expect(isFinishedStatus('active')).toBe(false);
      expect(isFinishedStatus('awaiting_user')).toBe(false);
    });
  });

  describe('needsAttentionStatus', () => {
    it('returns true only for awaiting_user', () => {
      expect(needsAttentionStatus('awaiting_user')).toBe(true);
    });

    it('returns false for failure statuses (now in Finished/red)', () => {
      expect(needsAttentionStatus('interrupted')).toBe(false);
      expect(needsAttentionStatus('blocked')).toBe(false);
      expect(needsAttentionStatus('failed')).toBe(false);
      expect(needsAttentionStatus('not_feasible')).toBe(false);
    });

    it('returns false for other statuses', () => {
      expect(needsAttentionStatus('active')).toBe(false);
      expect(needsAttentionStatus('completed')).toBe(false);
      expect(needsAttentionStatus('acknowledged')).toBe(false);
    });
  });

  describe('finishedTone', () => {
    it('returns green for completed/acknowledged', () => {
      expect(finishedTone('completed')).toBe('green');
      expect(finishedTone('acknowledged')).toBe('green');
    });

    it('returns red for failure statuses', () => {
      expect(finishedTone('failed')).toBe('red');
      expect(finishedTone('interrupted')).toBe('red');
      expect(finishedTone('blocked')).toBe('red');
      expect(finishedTone('not_feasible')).toBe('red');
    });
  });

  describe('categorizeMission', () => {
    describe('running takes priority', () => {
      it('categorizes as running when actually running, regardless of stored status', () => {
        // Key scenario: resumed mission still has stale status but is actually running
        expect(categorizeMission('awaiting_user', true)).toBe('running');
        expect(categorizeMission('active', true)).toBe('running');
        expect(categorizeMission('interrupted', true)).toBe('running');
        // Edge cases
        expect(categorizeMission('completed', true)).toBe('running');
        expect(categorizeMission('failed', true)).toBe('running');
      });
    });

    describe('needs-you when not running', () => {
      it('categorizes awaiting_user missions as needs-you when not running', () => {
        expect(categorizeMission('awaiting_user', false)).toBe('needs-you');
      });

      it('failure statuses no longer land in needs-you (they go to finished/red)', () => {
        expect(categorizeMission('interrupted', false)).toBe('finished');
        expect(categorizeMission('blocked', false)).toBe('finished');
      });
    });

    describe('finished when not running', () => {
      it('green-tone statuses', () => {
        expect(categorizeMission('completed', false)).toBe('finished');
        expect(categorizeMission('acknowledged', false)).toBe('finished');
      });

      it('red-tone statuses', () => {
        expect(categorizeMission('failed', false)).toBe('finished');
        expect(categorizeMission('interrupted', false)).toBe('finished');
        expect(categorizeMission('blocked', false)).toBe('finished');
        expect(categorizeMission('not_feasible', false)).toBe('finished');
      });
    });

    describe('other category', () => {
      it('categorizes active but not-running missions as other', () => {
        // Edge case: stored as active but runtime says not running.
        // Can happen briefly during state transitions or right after a
        // resume before the runner-tracker catches up.
        expect(categorizeMission('active', false)).toBe('other');
      });
    });

    describe('waiting for tool takes priority over running', () => {
      it('categorizes a mission parked on a frontend tool as needs-you', () => {
        // Backend still reports the mission as running (run state
        // waiting_for_tool), but it is blocked on the user.
        expect(categorizeMission('active', true, true)).toBe('needs-you');
        expect(categorizeMission('active', false, true)).toBe('needs-you');
      });
    });
  });

  describe('categorizeMissions', () => {
    type TestMission = { id: string; status: MissionStatus };

    it('groups missions into correct categories', () => {
      const missions: TestMission[] = [
        { id: '1', status: 'active' },
        { id: '2', status: 'awaiting_user' },
        { id: '3', status: 'completed' },
        { id: '4', status: 'failed' },
        { id: '5', status: 'acknowledged' },
        { id: '6', status: 'interrupted' },
      ];
      const runningIds = new Set(['1']);

      const result = categorizeMissions(missions, runningIds);

      expect(result.running.map(m => m.id)).toEqual(['1']);
      expect(result['needs-you'].map(m => m.id)).toEqual(['2']);
      expect(result.finished.map(m => m.id)).toEqual(['3', '4', '5', '6']);
      expect(result.other).toEqual([]);
    });

    it('puts missions waiting on a frontend tool in needs-you, not running', () => {
      const missions: TestMission[] = [
        { id: 'asking', status: 'active' }, // parked on AskUserQuestion
        { id: 'executing', status: 'active' }, // genuinely running a turn
      ];
      // Backend reports both in the running list, but `asking` is waiting_for_tool.
      const runningIds = new Set(['executing']);
      const waitingForToolIds = new Set(['asking']);

      const result = categorizeMissions(missions, runningIds, waitingForToolIds);

      expect(result.running.map(m => m.id)).toEqual(['executing']);
      expect(result['needs-you'].map(m => m.id)).toEqual(['asking']);
    });

    it('handles resumed mission correctly (awaiting_user but running)', () => {
      const missions: TestMission[] = [
        { id: 'resumed', status: 'awaiting_user' }, // DB still says awaiting_user
        { id: 'new', status: 'active' },
        { id: 'waiting', status: 'awaiting_user' },
      ];
      const runningIds = new Set(['resumed', 'new']);

      const result = categorizeMissions(missions, runningIds);

      expect(result.running.map(m => m.id)).toEqual(['resumed', 'new']);
      expect(result['needs-you'].map(m => m.id)).toEqual(['waiting']);
      expect(result.finished).toEqual([]);
    });

    it('handles empty missions array', () => {
      const result = categorizeMissions([], new Set());

      expect(result.running).toEqual([]);
      expect(result['needs-you']).toEqual([]);
      expect(result.finished).toEqual([]);
      expect(result.other).toEqual([]);
    });

    it('handles empty running set', () => {
      const missions: TestMission[] = [
        { id: '1', status: 'active' },
        { id: '2', status: 'completed' },
      ];

      const result = categorizeMissions(missions, new Set());

      expect(result.running).toEqual([]);
      expect(result.other.map(m => m.id)).toEqual(['1']); // active but not running
      expect(result.finished.map(m => m.id)).toEqual(['2']);
    });

    it('puts each mission in exactly one category', () => {
      const missions: TestMission[] = [
        { id: '1', status: 'active' },
        { id: '2', status: 'awaiting_user' },
        { id: '3', status: 'completed' },
        { id: '4', status: 'acknowledged' },
        { id: '5', status: 'failed' },
        { id: '6', status: 'not_feasible' },
        { id: '7', status: 'interrupted' },
        { id: '8', status: 'blocked' },
      ];
      const runningIds = new Set(['1', '2']);

      const result = categorizeMissions(missions, runningIds);

      const allCategorized = [
        ...result.running,
        ...result['needs-you'],
        ...result.finished,
        ...result.other,
      ];

      expect(allCategorized.length).toBe(missions.length);
      const ids = allCategorized.map(m => m.id);
      expect(new Set(ids).size).toBe(ids.length);
    });
  });

  describe('getMissionDotColor', () => {
    it('returns indigo for running missions regardless of status', () => {
      expect(getMissionDotColor('active', true)).toBe('bg-indigo-400');
      expect(getMissionDotColor('awaiting_user', true)).toBe('bg-indigo-400');
      expect(getMissionDotColor('completed', true)).toBe('bg-indigo-400');
    });

    it('returns status-specific color when not running', () => {
      expect(getMissionDotColor('completed', false)).toBe('bg-emerald-400');
      expect(getMissionDotColor('acknowledged', false)).toBe('bg-emerald-400');
      expect(getMissionDotColor('awaiting_user', false)).toBe('bg-amber-400');
      // Failure statuses share a red tone in the Finished column.
      expect(getMissionDotColor('failed', false)).toBe('bg-red-400');
      expect(getMissionDotColor('interrupted', false)).toBe('bg-red-400');
      expect(getMissionDotColor('blocked', false)).toBe('bg-red-400');
      expect(getMissionDotColor('not_feasible', false)).toBe('bg-red-400');
      expect(getMissionDotColor('active', false)).toBe('bg-indigo-400');
    });
  });

  describe('getMissionTextColor', () => {
    it('returns indigo for running missions regardless of status', () => {
      expect(getMissionTextColor('active', true)).toBe('text-indigo-400');
      expect(getMissionTextColor('awaiting_user', true)).toBe('text-indigo-400');
    });

    it('returns status-specific color when not running', () => {
      expect(getMissionTextColor('completed', false)).toBe('text-emerald-400');
      expect(getMissionTextColor('awaiting_user', false)).toBe('text-amber-400');
      expect(getMissionTextColor('failed', false)).toBe('text-red-400');
      expect(getMissionTextColor('interrupted', false)).toBe('text-red-400');
    });
  });

  describe('status constants', () => {
    it('FINISHED_STATUSES contains all bucket-Finished statuses', () => {
      expect(FINISHED_STATUSES).toContain('completed');
      expect(FINISHED_STATUSES).toContain('acknowledged');
      expect(FINISHED_STATUSES).toContain('failed');
      expect(FINISHED_STATUSES).toContain('interrupted');
      expect(FINISHED_STATUSES).toContain('blocked');
      expect(FINISHED_STATUSES).toContain('not_feasible');
      expect(FINISHED_STATUSES).toHaveLength(6);
    });

    it('NEEDS_ATTENTION_STATUSES contains only awaiting_user', () => {
      expect(NEEDS_ATTENTION_STATUSES).toEqual(['awaiting_user']);
    });
  });
});
