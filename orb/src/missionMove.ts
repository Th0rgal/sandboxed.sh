import { api, getApiUrl, getMission, connectionVersion } from './api';
import { copyText } from './clipboard';
const prefix = 'orb:cut-mission:';
export async function cutMission(id: string): Promise<string> {
  const text = prefix + JSON.stringify({ id, backend: getApiUrl(), nonce: crypto.randomUUID() });
  await copyText(text);
  return text;
}
export function readCutMission(text: string, backend: string): string | null {
  if (!text.startsWith(prefix)) return null;
  try {
    const value = JSON.parse(text.slice(prefix.length));
    return value.backend === backend && typeof value.id === 'string' && /^[\da-f]{8}-(?:[\da-f]{4}-){3}[\da-f]{12}$/i.test(value.id) ? value.id : null;
  } catch { return null; }
}
export async function moveMission(id: string, project: string, folder: string): Promise<void> {
  const version = connectionVersion();
  const mission = await getMission(id);
  if (version !== connectionVersion()) throw new Error('Connection changed. Cut the conversation again.');
  const tags = (mission.tags ?? []).filter(tag => !tag.startsWith('orb-folder:'));
  if (folder) tags.push(`orb-folder:${folder}`);
  await api(`/api/control/missions/${encodeURIComponent(id)}/project`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ project, tags }),
  });
}
