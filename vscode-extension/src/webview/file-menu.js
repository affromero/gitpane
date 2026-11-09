'use strict';

window.GitpaneFileMenu = function ({ document, window, postMessage, selection }) {
  let requestId = 0;
  let pending;
  let menu;
  let returnFocus;

  function close() {
    const restoreFocus = menu?.contains(document.activeElement);
    requestId++;
    pending = undefined;
    menu?.remove();
    menu = undefined;
    if (restoreFocus && returnFocus?.isConnected) returnFocus.focus();
    returnFocus = undefined;
  }

  function open(id, x, y) {
    close();
    returnFocus = document.activeElement;
    pending = { id, x, y, requestId, selection: selection() };
    postMessage({ type: 'fileMenuRequest', id, requestId });
  }

  window.addEventListener('message', event => {
    const value = event.data;
    if (value?.type !== 'fileMenu' || !pending || value.id !== pending.id ||
        value.requestId !== pending.requestId || selection() !== pending.selection ||
        typeof value.token !== 'string' || !Array.isArray(value.choices)) return;
    const current = pending;
    menu?.remove();
    menu = document.createElement('div');
    menu.className = 'file-context-menu';
    menu.setAttribute('role', 'menu');
    menu.setAttribute('aria-label', 'File actions');
    for (const choice of value.choices) {
      if (typeof choice?.label !== 'string' || typeof choice.action !== 'string') continue;
      const button = document.createElement('button');
      button.type = 'button';
      button.setAttribute('role', 'menuitem');
      button.textContent = choice.label;
      button.addEventListener('click', () => {
        if (selection() === current.selection) postMessage({ type: 'fileMenuAction', id: current.id, token: value.token, action: choice.action });
        close();
      });
      menu.appendChild(button);
    }
    if (!menu.children.length) return close();
    document.body.appendChild(menu);
    const bounds = menu.getBoundingClientRect();
    menu.style.left = `${Math.max(0, Math.min(current.x, window.innerWidth - bounds.width))}px`;
    menu.style.top = `${Math.max(0, Math.min(current.y, window.innerHeight - bounds.height))}px`;
    menu.querySelector('button')?.focus();
  });

  document.addEventListener('pointerdown', event => { if (pending && (!menu || !menu.contains(event.target))) close(); });
  document.addEventListener('keydown', event => {
    if (!menu) return;
    if (event.key === 'Escape') { event.preventDefault(); close(); return; }
    if (!['ArrowDown', 'ArrowUp'].includes(event.key)) return;
    event.preventDefault();
    const buttons = [...menu.querySelectorAll('button')];
    const index = buttons.indexOf(document.activeElement);
    buttons[(index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length]?.focus();
  });
  return { open, close, closeIfChanged: () => { if (pending && selection() !== pending.selection) close(); } };
};
