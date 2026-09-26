// A bounded, connection-scoped bridge from upload to the canonical message.
// Reopening a conversation still reads the durable file, never this cache alone.
const previews = new Map<string, string>();
let size = 0;
const LIMIT = 48 * 1024 * 1024;
export function rememberImagePreview(scope: string, path: string, dataUrl: string) {
  const key = JSON.stringify([scope, path]);
  size -= previews.get(key)?.length ?? 0;
  previews.delete(key);
  if (dataUrl.length <= LIMIT) { previews.set(key, dataUrl); size += dataUrl.length; }
  while (size > LIMIT) {
    const oldest = previews.keys().next().value!;
    size -= previews.get(oldest)!.length;
    previews.delete(oldest);
  }
}
export function imagePreview(scope: string, path: string) {
  return previews.get(JSON.stringify([scope, path]));
}
export const imagePreviewScope = (endpoint: string, connection: number, destination: string) => JSON.stringify([endpoint, connection, destination]);
