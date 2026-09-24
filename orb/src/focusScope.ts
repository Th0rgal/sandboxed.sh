/** Nested dialogs own keyboard focus in stack order, including portalled popovers. */
type Scope = { root: HTMLElement; previous: HTMLElement | null; parent?: () => HTMLElement | undefined };
const scopes: Scope[] = [];
export const hasFocusScope = () => scopes.length > 0;

function focusable(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(
    'button, a[href], input, select, textarea, summary, [tabindex]',
  )).filter((el) => {
    if (el.tabIndex < 0 || el.matches(':disabled') || el.closest('[hidden], [inert]')) return false;
    for (let node: HTMLElement | null = el; node; node = node.parentElement) {
      if (node instanceof HTMLDetailsElement && !node.open && !node.querySelector(":scope > summary")?.contains(el)) return false;
      const style = getComputedStyle(node);
      if (style.display === 'none' || style.visibility === 'hidden') return false;
      if (node === root) break;
    }
    return true;
  });
}

export function trapFocus(root: HTMLElement, onEscape: () => void, options: { parent?: () => HTMLElement | undefined; initialFocus?: () => HTMLElement | undefined; returnFocus?: HTMLElement | null } = {}): () => void {
  const previous = options.returnFocus !== undefined ? options.returnFocus : document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const scope: Scope = { root, previous, parent: options.parent };
  const childIndex = scopes.findIndex((child) => root.contains(child.root) || child.parent?.() === root);
  if (childIndex < 0) scopes.push(scope);
  else {
    scope.previous = scopes[childIndex].previous;
    scopes.splice(childIndex, 0, scope);
  }
  const top = () => scopes.at(-1) === scope;
  const first = () => focusable(root)[0] ?? root;
  const key = (event: KeyboardEvent) => {
    if (!top() || event.defaultPrevented) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      onEscape();
    } else if (event.key === 'Tab') {
      const items = focusable(root);
      const index = items.indexOf(document.activeElement as HTMLElement);
      if (!items.length || index < 0 || (event.shiftKey ? index === 0 : index === items.length - 1)) {
        event.preventDefault();
        event.stopPropagation();
        (event.shiftKey ? items.at(-1) ?? root : items[0] ?? root).focus();
      }
    }
  };
  const keepFocus = (event: FocusEvent) => {
    if (top() && !root.contains(event.target as Node)) first().focus();
  };
  root.addEventListener('keydown', key);
  document.addEventListener('focusin', keepFocus);
  if (top()) {
    const items = focusable(root);
    const requested = options.initialFocus?.();
    (requested && (requested === root || items.includes(requested))
      ? requested : items.find(el => el.hasAttribute('autofocus')) ?? first()).focus();
  }
  return () => {
    const wasTop = top();
    scopes.splice(scopes.indexOf(scope), 1);
    // If an outer dialog unmounts with a child still open, inherit its return target.
    for (const child of scopes) {
      if (child.previous && root.contains(child.previous)) child.previous = scope.previous;
    }
    root.removeEventListener('keydown', key);
    document.removeEventListener('focusin', keepFocus);
    if (wasTop) {
      const parentScope = scopes.at(-1);
      const restore = () => {
        // A new modal may have opened before a deferred restore runs.
        if (scopes.at(-1) !== parentScope) return;
        const parent = parentScope?.root;
        const target = scope.previous;
        if (target?.isConnected && (!parent || parent.contains(target))) target.focus();
        else if (parent) (focusable(parent)[0] ?? parent).focus();
      };
      // Solid flushes the parent's inert binding after the child's cleanup.
      if (parentScope?.root.inert) queueMicrotask(restore);
      else restore();
    }
  };
}
