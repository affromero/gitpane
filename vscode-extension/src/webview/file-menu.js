'use strict';

window.GitpaneMenuButton = function (document, label, activate) {
  const button = document.createElement('button');
  button.className = 'menu-action';
  button.textContent = '⋯';
  button.type = 'button';
  button.title = label;
  button.setAttribute('aria-label', label);
  button.addEventListener('click', event => { event.stopPropagation(); activate(button); });
  return button;
};

window.GitpaneFileMenu = function ({ document, window, postMessage, selection }) {
  let requestId = 0;
  let pending;
  let menu;
  let returnFocus;
  let returnKey;
  let returnAction;

  function close() {
    const restoreFocus = menu?.contains(document.activeElement);
    requestId++;
    pending = undefined;
    menu?.remove();
    menu = undefined;
    if (restoreFocus) {
      const row = [...document.querySelectorAll('[data-key]')].find(item => item.dataset.key === returnKey);
      const target = returnFocus?.isConnected ? returnFocus : (returnAction && row?.querySelector('.menu-action')) || row;
      target?.focus();
    }
    returnFocus = undefined;
    returnKey = undefined;
    returnAction = false;
  }

  function open(id, x, y, kind = 'file', focus = document.activeElement) {
    close();
    returnFocus = focus;
    returnKey = returnFocus?.closest?.('[data-key]')?.dataset.key;
    returnAction = returnFocus?.classList?.contains('menu-action');
    pending = { id, kind, x, y, requestId, selection: selection() };
    postMessage(kind === 'file' ? { type: 'fileMenuRequest', id, requestId } : kind === 'commit' ?
      { type: 'commitMenuRequest', id, requestId } : { type: 'rowMenuRequest', kind, id, requestId });
  }

  function openItem(item) {
    const row = item.item.querySelector('.row');
    const kind = row?.classList.contains('repo-row') ? 'repo' : row?.classList.contains('worktree-row') ? 'worktree' : undefined;
    const id = kind === 'repo' ? item.owner?.id : kind === 'worktree' ? item.worktreeId : item.fileId;
    if (!id) return false;
    if (kind) postMessage({ type: 'selectOwner', id });
    const rect = row.getBoundingClientRect();
    open(id, rect.left, rect.bottom, kind || 'file');
    return true;
  }

  window.addEventListener('message', event => {
    const value = event.data;
    if (!pending || value?.type !== (pending.kind === 'file' ? 'fileMenu' : pending.kind === 'commit' ? 'commitMenu' : 'rowMenu') ||
        (pending.kind !== 'file' && pending.kind !== 'commit' && value.kind !== pending.kind) || value.id !== pending.id ||
        value.requestId !== pending.requestId || selection() !== pending.selection ||
        typeof value.token !== 'string' || !Array.isArray(value.choices)) return;
    const current = pending;
    menu?.remove();
    menu = document.createElement('div');
    menu.className = 'file-context-menu';
    menu.setAttribute('role', 'menu');
    menu.setAttribute('aria-label', current.kind === 'file' ? 'File actions' : current.kind === 'commit' ? 'Commit actions' : 'Repository actions');
    for (const choice of value.choices) {
      if (typeof choice?.label !== 'string' || typeof choice.action !== 'string') continue;
      const button = document.createElement('button');
      button.type = 'button';
      button.setAttribute('role', 'menuitem');
      button.textContent = choice.label;
      button.addEventListener('click', () => {
        if (selection() === current.selection) postMessage(current.kind === 'file' ?
          { type: 'fileMenuAction', id: current.id, token: value.token, action: choice.action } :
          current.kind === 'commit' ? { type: 'commitMenuAction', id: current.id, token: value.token, action: choice.action } :
            { type: 'rowMenuAction', kind: current.kind, id: current.id, token: value.token, action: choice.action });
        close();
      });
      menu.appendChild(button);
    }
    if (!menu.children.length) return close();
    document.body.appendChild(menu);
    reposition();
    menu.querySelector('button')?.focus();
  });

  function reposition() {
    if (!menu || !pending) return;
    const bounds = menu.getBoundingClientRect();
    menu.style.left = `${Math.max(0, Math.min(pending.x, window.innerWidth - bounds.width))}px`;
    menu.style.top = `${Math.max(0, Math.min(pending.y, window.innerHeight - bounds.height))}px`;
  }
  window.addEventListener('resize', reposition);
  window.addEventListener('scroll', reposition, true);

  document.addEventListener('pointerdown', event => { if (pending && (!menu || !menu.contains(event.target))) close(); });
  document.addEventListener('keydown', event => {
    if (event.key === 'Escape' && pending) { event.preventDefault(); close(); return; }
    if (!menu) return;
    if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const buttons = [...menu.querySelectorAll('button')];
    const index = buttons.indexOf(document.activeElement);
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 :
      (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length;
    buttons[next]?.focus();
  });
  return { open, openItem, close, closeIfChanged: () => { if (pending && selection() !== pending.selection) close(); } };
};
