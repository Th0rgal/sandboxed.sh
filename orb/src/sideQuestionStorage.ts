import {readSideThread,saveSideThread} from "./composerDrafts";
import { getApiUrl, getJwt } from './api';
import type { SideAttachment, SideExchange } from './sideQuestionClient';

export type SavedSideQuestion = {
  history: SideExchange[];
  draft: string;
  model: string;
  open: boolean;
  docked: boolean;
  pending?: { question: string; answer: string; error?: string; attachments?:SideAttachment[] };
};
export function sideQuestionKey(mission: string): string {
  let account = 'local';
  try {
    const token = getJwt();
    if (token) {
      const payload = JSON.parse(atob(token.split('.')[1].replace(/-/g, '+').replace(/_/g, '/')));
      account = String(payload.sub ?? payload.user_id ?? 'authenticated');
    }
  } catch { account = 'authenticated'; }
  return `orb.btw:v1:${JSON.stringify([getApiUrl().replace(/\/+$/, ''), account, mission])}`;
}
export async function readSideQuestion(key: string): Promise<SavedSideQuestion | undefined> {
  try {
    const value = await readSideThread<SavedSideQuestion>(key).catch(()=>undefined) ?? JSON.parse(localStorage.getItem(key) ?? 'null');
    if (!value || !Array.isArray(value.history)) return;
    return {
      history: value.history.filter((v: SideExchange) => typeof v?.question === 'string' && typeof v?.answer === 'string').slice(-20),
      draft: typeof value.draft === 'string' ? value.draft.slice(0, 2000) : '',
      model: typeof value.model === 'string' ? value.model : '',
      open: value.open === true,
      docked: value.docked === true,
      pending: typeof value.pending?.question === 'string' && typeof value.pending?.answer === 'string' ? value.pending : undefined,
    };
  } catch { return; }
}
export async function writeSideQuestion(key: string, value: SavedSideQuestion): Promise<boolean> {
  try { await saveSideThread(key,value); localStorage.removeItem(key); return true; }
  catch { return false; }
}
