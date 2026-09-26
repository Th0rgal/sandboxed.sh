// Keep startup failures visible even when the application's module graph fails.
void import('./index').catch(error => {
  console.error('Orb failed to start', error);
  const root = document.getElementById('root');
  if (!root) return;
  const notice = document.createElement('div');
  notice.style.cssText = 'margin:20vh auto;padding:24px;max-width:360px;font:14px/1.6 system-ui;color:#ccc';
  const title = document.createElement('h2');
  title.textContent = 'Orb couldn’t load';
  const message = document.createElement('p');
  message.textContent = 'The interface failed to load. Reload to try again.';
  const retry = document.createElement('button');
  retry.textContent = 'Reload Orb';
  retry.style.cssText = 'padding:8px 14px;border:1px solid #555;border-radius:8px;background:#282828;color:#eee;cursor:pointer';
  retry.onclick = () => location.reload();
  notice.append(title, message, retry);
  root.replaceChildren(notice);
});
