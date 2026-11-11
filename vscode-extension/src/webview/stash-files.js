'use strict';

// Stash file children for the Explorer webview. Loaded before view.js; exposed
// as a global so the main tree keeps the node building and state ownership.
(function () {
  function row(element, add, statusClass, file) {
    const item = element('div', `row file-row stash-file-row${file.gitlink ? ' submodule-file-row' : ''}`);
    add(item, statusClass(file.status), file.status || ' ');
    if (file.gitlink) add(item, 'submodule-tag', '[sub: pointer]', 'Saved submodule pointer');
    const text = file.oldPath ? `${file.oldPath} → ${file.path}` : file.path;
    add(item, 'file-path', text, text);
    if (file.binary) add(item, 'muted stash-binary', 'binary', 'Saved binary file');
    return item;
  }

  function request(post, pending, stash) {
    if (!stash || pending.has(stash.id)) return;
    // Automatic loads happen once per stash. Errors stay visible until the user
    // collapses and expands the stash again, so a failing backend cannot loop.
    if (stash.fileState !== undefined) return;
    pending.set(stash.id, { state: stash.fileState, acknowledged: false });
    post({ type: 'loadStashFiles', id: stash.id });
  }

  function retry(post, pending, stash) {
    if (!stash) return;
    pending.set(stash.id, { state: stash.fileState, acknowledged: false });
    post({ type: 'loadStashFiles', id: stash.id });
  }

  function reconcile(pending, stash) {
    const state = pending.get(stash.id);
    if (!state) return;
    if (stash.fileState === 'loading') state.acknowledged = true;
    else if (stash.fileState !== undefined && stash.fileState !== state.state) pending.delete(stash.id);
  }

  function status(container, element, text, error) {
    const item = element('div', `page-status${error ? ' error' : ''}`, text);
    item.setAttribute('role', error ? 'alert' : 'status');
    container.appendChild(item);
  }

  function render(options) {
    const { element, add, createItem, statusClass, container, stash, open, pending, post } = options;
    const parent = options.parentKey;
    request(post, pending, stash);
    if (stash.fileState === 'error') {
      status(container, element, stash.fileError || 'Could not load saved files.', true);
      return;
    }
    if (stash.fileState === 'empty') {
      status(container, element, 'No saved files.');
      return;
    }
    if (stash.fileState !== 'ready') {
      status(container, element, 'Loading saved files…');
      return;
    }
    for (const file of Array.isArray(stash.files) ? stash.files : []) {
      const item = createItem(`stash-file:${file.id}`, file.label || file.path, row(element, add, statusClass, file),
        false, open, parent);
      item.stashId = stash.id;
      item.stashFileId = file.id;
      container.appendChild(item.item);
    }
  }

  window.GitpaneStashFiles = { render, retry };
})();
