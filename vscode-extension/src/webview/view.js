'use strict';

const vscode = acquireVsCodeApi();
const tree = document.getElementById('repositories');
const messages = document.getElementById('messages');
const startSearchButton = document.getElementById('start-search');
const searchBox = document.getElementById('search-box');
const searchInput = document.getElementById('file-search');
const searchStatus = document.getElementById('search-status');
const searchPrev = document.getElementById('search-prev');
const searchNext = document.getElementById('search-next');
const saved = vscode.getState() || {};
const expanded = new Set(Array.isArray(saved.expanded) ? saved.expanded : []);
const collapsedGroups = new Set(Array.isArray(saved.collapsedGroups) ? saved.collapsedGroups : []);
const expandedStashes = new Set(Array.isArray(saved.expandedStashes) ? saved.expandedStashes : []);
const worktreeVisibility = new Map((Array.isArray(saved.worktreeVisibility) ? saved.worktreeVisibility : [])
  .filter(item => Array.isArray(item) && typeof item[0] === 'string' && /^[a-f0-9]{64}$/.test(item[0]) && typeof item[1] === 'boolean').slice(-1024));
const transientWorktreeVisibility = new Map();
const appliedWorktreeReveals = new Set((Array.isArray(saved.appliedWorktreeReveals) ? saved.appliedWorktreeReveals : [])
  .filter(id => typeof id === 'string' && id.length > 0 && id.length <= 128).slice(-128));
let selected = typeof saved.selected === 'string' ? saved.selected : undefined;
let snapshot = { repositories: [], errors: [], loading: true };
let visible = [];
const pendingLoads = new Map();
const pendingPages = new Map();
let searchOwner;
let searchTimer;

function save() {
  vscode.setState({ expanded: [...expanded], collapsedGroups: [...collapsedGroups], expandedStashes: [...expandedStashes],
    worktreeVisibility: [...worktreeVisibility].slice(-1024), appliedWorktreeReveals: [...appliedWorktreeReveals].slice(-128), selected });
}

function worktreesVisible(repo) {
  const key = repo.worktreeStateKey;
  if (key && transientWorktreeVisibility.has(repo.id)) {
    setWorktreesVisible(repo, transientWorktreeVisibility.get(repo.id));
    transientWorktreeVisibility.delete(repo.id);
  }
  return (key && worktreeVisibility.has(key) ? worktreeVisibility.get(key) : transientWorktreeVisibility.get(repo.id))
    ?? snapshot.expandWorktrees === true;
}

function setWorktreesVisible(repo, show) {
  if (repo.worktreeStateKey) {
    worktreeVisibility.delete(repo.worktreeStateKey);
    worktreeVisibility.set(repo.worktreeStateKey, show);
    if (worktreeVisibility.size > 1024) worktreeVisibility.delete(worktreeVisibility.keys().next().value);
  } else transientWorktreeVisibility.set(repo.id, show);
}

function toggleWorktrees(repo) {
  if (!repo?.worktrees?.length) return;
  const show = !worktreesVisible(repo);
  const children = visible.filter(item => item.parent === `repo:${repo.id}` && item.worktreeId && item.isExpanded);
  setWorktreesVisible(repo, show);
  if (!show) {
    for (const item of children) releaseChanges(item);
  }
  render();
}

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = String(text);
  return node;
}

function add(parent, className, text, title) {
  const node = element('span', className, text);
  if (title) node.title = title;
  parent.appendChild(node);
  return node;
}

function menuButton(label, kind, id) {
  const button = element('button', 'menu-action', '⋯');
  button.type = 'button';
  button.title = label;
  button.setAttribute('aria-label', label);
  button.addEventListener('click', event => {
    event.stopPropagation();
    vscode.postMessage({ type: 'menu', kind, id });
  });
  return button;
}

function fileCount(repo) {
  const paths = new Set();
  for (const group of repo.groups || []) {
    if (group.id === 'attention') continue;
    for (const file of group.files || []) paths.add(file.path);
  }
  return paths.size;
}

function countInfo(owner) {
  const page = owner.changesPage;
  const loaded = fileCount(owner);
  const pending = !!page && page.state !== 'ready';
  const lower = Number.isSafeInteger(owner.changeCountLowerBound) && owner.changeCountLowerBound >= 0
    ? owner.changeCountLowerBound : 0;
  const paginated = !!page && (page.previous || page.next || page.page > 0);
  const count = pending || paginated ? Math.max(loaded, lower) : loaded;
  return { count, unknown: !!page && page.state !== 'ready' && !count && !!owner.dirty,
    partial: !!page && ((pending && count > 0) || paginated),
    hasDirectory: (owner.groups || []).some(group => (group.files || []).some(file => file.directory)) };
}

function countText(info) {
  if (info.unknown) return 'changes available';
  const changes = info.partial || info.hasDirectory;
  const noun = changes ? (info.count === 1 ? 'change' : 'changes') : (info.count === 1 ? 'file' : 'files');
  return `${info.partial ? 'at least ' : ''}${info.count} ${noun}`;
}

function requestChanges(owner, force = false) {
  if (!owner?.changesPage || owner.changesPage.state === 'loading' || pendingLoads.has(owner.id)) return;
  if (!force && owner.changesPage.state !== 'idle') return;
  pendingLoads.set(owner.id, { state: owner.changesPage.state, acknowledged: false });
  vscode.postMessage({ type: 'loadChanges', id: owner.id });
}

function requestPage(ownerId, direction, page) {
  if (pendingPages.has(ownerId)) return;
  pendingPages.set(ownerId, { page, acknowledged: false });
  vscode.postMessage({ type: 'changesPage', id: ownerId, direction });
}

function reconcilePending(owner) {
  const page = owner.changesPage;
  if (!page) return;
  const load = pendingLoads.get(owner.id);
  if (load) {
    if (page.state === 'loading') load.acknowledged = true;
    else if ((page.state === 'ready' || page.state === 'error') &&
      (load.acknowledged || page.state !== load.state)) pendingLoads.delete(owner.id);
  }
  const pending = pendingPages.get(owner.id);
  if (!pending) return;
  if (page.state === 'loading') pending.acknowledged = true;
  else if (page.state === 'error' || (page.state === 'ready' &&
    (pending.acknowledged || page.page !== pending.page))) pendingPages.delete(owner.id);
}

function labelForRepo(repo, info) {
  const details = [repo.name, repo.worktree ? 'worktree' : '', repo.branch || 'unknown branch',
    repo.statusUnavailable ? 'status unavailable' : repo.dirty ? 'has changes' : 'clean',
    repo.statusUnavailable ? '' : countText(info)];
  if (!repo.statusUnavailable) {
    if (repo.stashCount > 0) details.push(`${repo.stashCount} ${repo.stashCount === 1 ? 'stash' : 'stashes'}`);
    if (repo.worktreeCount > 0) details.push(`${repo.worktreeCount} ${repo.worktreeCount === 1 ? 'worktree' : 'worktrees'}`);
    if (repo.ahead > 0) details.push(`${repo.ahead} ahead`);
    if (repo.behind > 0) details.push(`${repo.behind} behind`);
    if (repo.hasDirtySubmodules) details.push('dirty submodules');
    if (repo.hasUnpushedSubmodules) details.push('submodules need attention');
  }
  if (repo.live) details.push('live session');
  if (repo.pinned) details.push('pinned submodule');
  return details.filter(Boolean).join(', ');
}

function repoRow(repo, info, isExpanded, hasWorktrees, hasStashes, showWorktrees) {
  const row = element('div', 'row repo-row');
  add(row, 'dirty', repo.dirty ? '*' : '\u00a0');
  const name = add(row, 'name', '', repo.name);
  if (repo.pinned) add(name, 'pin', '⌖ ', 'Pinned submodule');
  if (repo.worktree) add(name, 'worktree', '⎇ ');
  name.appendChild(document.createTextNode(repo.name));
  const branch = repo.branch || '—';
  add(row, `branch${branch === 'main' || branch === 'master' ? ' default' : ''}`, branch, branch);
  const tail = element('span', 'tail');
  if (repo.live) add(tail, 'live', '◉', 'Live session');
  if (repo.statusUnavailable) add(tail, 'behind', '!', 'Status unavailable');
  else {
    if (repo.stashCount > 0) {
      if (hasStashes) {
        const key = `stash-group:${repo.id}`;
        const open = expandedStashes.has(key);
        const button = element('button', 'stash stash-action', `${open ? '▾' : '▸'}$${repo.stashCount}`);
        button.type = 'button';
        button.setAttribute('aria-label', `${open ? 'Collapse' : 'Expand'} stashes for ${repo.name}`);
        button.setAttribute('aria-expanded', String(open));
        button.addEventListener('click', event => {
          event.stopPropagation();
          if (open) expandedStashes.delete(key);
          else { expanded.add(`repo:${repo.id}`); expandedStashes.add(key); requestChanges(repo); }
          render();
        });
        tail.appendChild(button);
      } else add(tail, 'stash', `$${repo.stashCount}`, `${repo.stashCount} stashes`);
    }
    if (repo.worktreeCount > 0) {
      if (hasWorktrees) {
        const button = element('button', 'worktree worktree-action', `${showWorktrees ? '▾' : '▸'}⎇${repo.worktreeCount}`);
        button.type = 'button';
        button.setAttribute('aria-label', `${showWorktrees ? 'Collapse' : 'Expand'} worktrees for ${repo.name}`);
        button.setAttribute('aria-expanded', String(showWorktrees));
        button.addEventListener('click', event => { event.stopPropagation(); toggleWorktrees(repo); });
        tail.appendChild(button);
      } else add(tail, 'worktree', `⎇${repo.worktreeCount}`, `${repo.worktreeCount} worktrees`);
    }
    if (repo.ahead > 0) add(tail, 'ahead', `↑${repo.ahead}`);
    if (repo.behind > 0) add(tail, 'behind', `↓${repo.behind}`);
    if (repo.hasDirtySubmodules) add(tail, 'dirty-submodule', '◈', 'Dirty submodules');
    if (repo.hasUnpushedSubmodules) add(tail, 'unpushed-submodule', '⇡', 'Submodules need attention');
    if (info.count || (info.unknown && repo.dirty)) {
      const value = info.unknown ? '?' : `${info.count}${info.partial ? '+' : ''}`;
      add(tail, 'count', `${isExpanded ? '▾' : '▸'}[${value}]`, countText(info));
    }
  }
  const graphButton = element('button', 'graph-action', '●');
  graphButton.type = 'button';
  graphButton.title = `Open commit graph for ${repo.name}`;
  graphButton.setAttribute('aria-label', graphButton.title);
  graphButton.addEventListener('click', event => {
    event.stopPropagation();
    vscode.postMessage({ type: 'openGraph', id: repo.id });
  });
  tail.appendChild(graphButton);
  tail.appendChild(menuButton(`Actions for ${repo.name}`, 'repo', repo.id));
  row.appendChild(tail);
  return row;
}

function statusClass(status) {
  return ({ M: 'modified', A: 'added', D: 'deleted', R: 'renamed', '?': 'untracked', C: 'conflicted', '!': 'behind' })[status] || 'muted';
}

function fileRow(file) {
  const row = element('div', `row file-row${file.directory ? ' directory-row' : ''}`);
  add(row, statusClass(file.status), file.status || ' ');
  add(row, `file-path${file.submodule ? ' submodule-path' : ''}`, file.path || file.name, file.path || file.name);
  if (file.directory) add(row, 'muted', 'directory', 'Untracked directory');
  else row.appendChild(menuButton(`Actions for ${file.path || file.name}`, 'file', file.id));
  return row;
}

function worktreeRow(worktree) {
  const row = element('div', 'row worktree-row');
  add(row, 'subtree', '⎇');
  add(row, 'dirty', worktree.dirty ? '*' : '\u00a0');
  add(row, 'worktree branch', worktree.branch || 'unknown branch', worktree.branch);
  add(row, 'name', worktree.name || '', worktree.name);
  const tail = element('span', 'tail');
  if (worktree.live) add(tail, 'live', '◉', 'Live session');
  if (worktree.ahead > 0) add(tail, 'ahead', `↑${worktree.ahead}`);
  if (worktree.behind > 0) add(tail, 'behind', `↓${worktree.behind}`);
  if (worktree.hasUnpushedSubmodules) add(tail, 'unpushed-submodule', '⇡', 'Submodules need attention');
  if (worktree.changesPage) {
    const info = countInfo(worktree);
    if (info.count || (info.unknown && worktree.dirty)) {
      add(tail, 'count', `[${info.unknown ? '?' : `${info.count}${info.partial ? '+' : ''}`}]`, countText(info));
    }
  } else if (worktree.fileCount > 0) add(tail, 'count', `[${worktree.fileCount}]`);
  if (worktree.available) {
    const graph = element('button', 'graph-action', '●');
    graph.type = 'button';
    graph.title = `Open commit graph for ${worktree.name || worktree.branch}`;
    graph.setAttribute('aria-label', graph.title);
    graph.addEventListener('click', event => {
      event.stopPropagation();
      vscode.postMessage({ type: 'openWorktreeGraph', id: worktree.id });
    });
    tail.appendChild(graph);
  } else add(tail, 'behind', '!', 'Worktree repository unavailable');
  tail.appendChild(menuButton(`Actions for worktree ${worktree.name || worktree.branch}`, 'worktree', worktree.id));
  row.appendChild(tail);
  return row;
}

function createItem(key, label, row, expandable, isExpanded, parent) {
  const item = element('div', 'treeitem');
  item.setAttribute('role', 'treeitem');
  item.setAttribute('aria-label', label);
  item.setAttribute('aria-selected', key === selected ? 'true' : 'false');
  item.setAttribute('tabindex', key === selected ? '0' : '-1');
  if (expandable) item.setAttribute('aria-expanded', isExpanded ? 'true' : 'false');
  item.dataset.key = key;
  item.appendChild(row);
  const data = { key, item, expandable, isExpanded, parent };
  visible.push(data);
  return data;
}

function renderGroups(parentElement, groups, parentKey, ownerId, partial = false) {
  for (const group of groups) {
    const groupKey = `group:${JSON.stringify([ownerId, group.id])}`;
    const files = Array.isArray(group.files) ? group.files : [];
    const owner = visible.find(item => item.owner?.id === ownerId)?.owner;
    const match = owner?.changesPage?.search;
    if (searchOwner === ownerId && match?.phase === 'ready' && match.query === searchInput.value && match.group === group.id) {
      collapsedGroups.delete(groupKey);
    }
    const groupExpanded = !collapsedGroups.has(groupKey);
    const row = element('div', 'row group-row');
    add(row, 'subtree', groupExpanded ? '▾' : '▸');
    add(row, '', `${group.label} [${files.length}]`);
    const groupItem = createItem(groupKey, `${group.label}, ${files.length} ${files.length === 1 ? 'file' : 'files'}${partial ? ' on this page' : ''}`,
      row, files.length > 0, groupExpanded, parentKey);
    parentElement.appendChild(groupItem.item);
    if (!groupExpanded || !files.length) continue;
    const filesGroup = element('div', 'children');
    filesGroup.setAttribute('role', 'group');
    groupItem.item.appendChild(filesGroup);
    for (const file of files) {
      const fileKey = `file:${file.id}`;
      const row = fileRow(file);
      if (searchOwner === ownerId && searchInput.value && (file.path || '').toLowerCase().includes(searchInput.value.toLowerCase())) {
        row.classList.add('search-match');
      }
      const fileItem = createItem(fileKey, `${group.id === 'attention' ? 'Submodule needs attention:' : file.status || 'changed'} ${file.path || file.name}${file.directory ? ', untracked directory' : ''}`,
        row, false, false, groupKey);
      fileItem.searchOwner = ownerId;
      fileItem.searchPath = file.directory ? file.path.slice(0, -1) : file.path;
      fileItem.searchGroup = group.id;
      if (!file.directory) fileItem.fileId = file.id;
      filesGroup.appendChild(fileItem.item);
    }
  }
}

function renderPage(parentElement, owner, parentKey) {
  const page = owner.changesPage;
  if (!page) return;
  if (page.state === 'loading') {
    const status = element('div', 'page-status', 'Loading changes…');
    status.setAttribute('role', 'status');
    parentElement.appendChild(status);
  } else if (page.state === 'error') {
    const error = element('div', 'page-status error', page.error || 'Could not load changes.');
    error.setAttribute('role', 'alert');
    parentElement.appendChild(error);
  } else if (page.state === 'ready' && (page.previous || page.next)) {
    const count = (owner.groups || []).reduce((total, group) => total + (group.files?.length || 0), 0);
    const status = element('div', 'page-status', `Page ${page.page + 1} · ${count} ${count === 1 ? 'entry' : 'entries'} shown`);
    status.setAttribute('role', 'status');
    parentElement.appendChild(status);
  } else if (page.state === 'ready' && !(owner.groups || []).some(group => group.files?.length)) {
    const status = element('div', 'page-status', 'No changed files.');
    status.setAttribute('role', 'status');
    parentElement.appendChild(status);
  }
  const directions = page.state === 'error' ? ['retry'] : [
    ...(page.previous ? ['previous'] : []), ...(page.next ? ['next'] : [])
  ];
  for (const direction of directions) {
    const label = direction === 'retry' ? 'Retry loading changes' :
      direction === 'previous' ? 'Previous changes' : 'Next changes';
    const row = element('div', 'row page-row');
    add(row, '', label);
    const item = createItem(`page:${direction}:${owner.id}`, `${label} for ${owner.name || owner.branch}`,
      row, false, false, parentKey);
    item.pageOwner = owner.id;
    item.pageNumber = page.page;
    item.pageDirection = direction;
    if (page.state === 'loading' || pendingPages.has(owner.id) || pendingLoads.has(owner.id)) {
      item.item.setAttribute('aria-disabled', 'true');
      item.pageDisabled = true;
    }
    parentElement.appendChild(item.item);
  }
}

function renderMessages() {
  messages.replaceChildren();
  const errors = Array.isArray(snapshot.errors) ? snapshot.errors : [];
  for (const error of errors) {
    const message = element('div', 'message error', error);
    message.setAttribute('role', 'alert');
    messages.appendChild(message);
  }
  if (errors.length) {
    const button = element('button', 'retry', 'Retry');
    button.type = 'button';
    button.addEventListener('click', () => vscode.postMessage({ type: 'refresh' }));
    messages.appendChild(button);
  }
  if (!snapshot.repositories.length && !errors.length) {
    messages.appendChild(element('div', 'message', snapshot.loading ? 'Loading repositories…' :
      snapshot.emptyMessage || 'No repositories in this workspace'));
  }
}

function render() {
  const hadFocus = tree.contains(document.activeElement);
  const previousSelection = visible.find(item => item.key === selected);
  const focusedAction = document.activeElement.classList?.contains('worktree-action') ? 'worktree-action' :
    document.activeElement.classList?.contains('stash-action') ? 'stash-action' :
    document.activeElement.classList?.contains('graph-action') ? 'graph-action' :
    document.activeElement.classList?.contains('menu-action') ? 'menu-action' : undefined;
  const focusedActionKey = focusedAction ? document.activeElement.closest('.treeitem')?.dataset.key : undefined;
  visible = [];
  const owners = new Set();
  tree.replaceChildren();
  renderMessages();
  for (const repo of snapshot.repositories) {
    owners.add(repo.id);
    reconcilePending(repo);
    const key = `repo:${repo.id}`;
    const info = countInfo(repo);
    const groups = Array.isArray(repo.groups) ? repo.groups : [];
    const worktrees = !repo.statusUnavailable && Array.isArray(repo.worktrees) ? repo.worktrees : [];
    const stashes = !repo.statusUnavailable && Array.isArray(repo.stashes) ? repo.stashes : [];
    const filesExpanded = expanded.has(key);
    const showWorktrees = worktrees.length > 0 && worktreesVisible(repo);
    const stashOpen = stashes.length > 0 && expandedStashes.has(`stash-group:${repo.id}`);
    const hasPage = !!repo.changesPage && (repo.dirty || repo.hasUnpushedSubmodules || repo.changesPage.state === 'loading' ||
      repo.changesPage.state === 'error' || repo.changesPage.previous || repo.changesPage.next);
    const fileExpandable = groups.length > 0 || hasPage;
    const hasChildren = (filesExpanded && (fileExpandable || stashes.length > 0)) || showWorktrees || stashOpen;
    const item = createItem(key, labelForRepo(repo, info), repoRow(repo, info, filesExpanded, worktrees.length > 0, stashes.length > 0, showWorktrees),
      fileExpandable || worktrees.length > 0 || stashes.length > 0, hasChildren, undefined);
    item.owner = repo;
    item.filesExpanded = filesExpanded;
    item.fileExpandable = fileExpandable;
    item.showWorktrees = showWorktrees;
    item.stashOpen = stashOpen;
    tree.appendChild(item.item);
    if (!hasChildren) continue;
    const children = element('div', 'children');
    children.setAttribute('role', 'group');
    item.item.appendChild(children);
    if (filesExpanded && fileExpandable) {
      requestChanges(repo);
      renderGroups(children, groups, key, repo.id, info.partial);
      renderPage(children, repo, key);
    }
    for (const worktree of showWorktrees ? worktrees : []) {
      owners.add(worktree.id);
      reconcilePending(worktree);
      const worktreeKey = `worktree:${worktree.id}`;
      const worktreeDetails = [worktree.name, 'worktree', worktree.branch, worktree.dirty ? 'has changes' : 'clean'];
      const worktreeInfo = countInfo(worktree);
      if (worktree.changesPage) worktreeDetails.push(countText(worktreeInfo));
      else if (worktree.fileCount > 0) worktreeDetails.push(`${worktree.fileCount} ${worktree.fileCount === 1 ? 'file' : 'files'}`);
      if (worktree.ahead > 0) worktreeDetails.push(`${worktree.ahead} ahead`);
      if (worktree.behind > 0) worktreeDetails.push(`${worktree.behind} behind`);
      if (worktree.hasUnpushedSubmodules) worktreeDetails.push('submodules need attention');
      if (worktree.live) worktreeDetails.push('live session');
      if (!worktree.available) worktreeDetails.push('repository unavailable');
      const worktreeLabel = worktreeDetails.filter(Boolean).join(', ');
      const worktreeGroups = worktree.available && Array.isArray(worktree.groups) ? worktree.groups : [];
      const worktreeExpanded = expanded.has(worktreeKey);
      const worktreeHasPage = worktree.available && !!worktree.changesPage &&
        (worktree.dirty || worktree.hasUnpushedSubmodules || worktree.changesPage.state === 'loading' || worktree.changesPage.state === 'error' ||
          worktree.changesPage.previous || worktree.changesPage.next);
      const worktreeItem = createItem(worktreeKey, worktreeLabel, worktreeRow(worktree),
        worktreeGroups.length > 0 || worktreeHasPage, worktreeExpanded, key);
      worktreeItem.worktreeId = worktree.id;
      worktreeItem.owner = worktree;
      children.appendChild(worktreeItem.item);
      if (worktreeExpanded && (worktreeGroups.length || worktreeHasPage)) {
        requestChanges(worktree);
        const files = element('div', 'children worktree-files');
        files.setAttribute('role', 'group');
        worktreeItem.item.appendChild(files);
        renderGroups(files, worktreeGroups, worktreeKey, worktree.id, worktreeInfo.partial);
        renderPage(files, worktree, worktreeKey);
      }
    }
    if (stashes.length && (filesExpanded || stashOpen)) {
      const stashKey = `stash-group:${repo.id}`;
      const stashExpanded = expandedStashes.has(stashKey);
      const row = element('div', 'row group-row');
      add(row, 'stash', stashExpanded ? '▾' : '▸');
      add(row, 'stash', `Stashes [${stashes.length}]`);
      const stashGroup = createItem(stashKey, `Stashes, ${stashes.length} ${stashes.length === 1 ? 'stash' : 'stashes'}`,
        row, true, stashExpanded, key);
      children.appendChild(stashGroup.item);
      if (stashExpanded) {
        const stashChildren = element('div', 'children');
        stashChildren.setAttribute('role', 'group');
        stashGroup.item.appendChild(stashChildren);
        for (const stash of stashes) {
          const label = `stash@{${stash.index}}`;
          const stashRow = element('div', 'row stash-row');
          add(stashRow, 'stash', `$ ${label}`);
          add(stashRow, 'stash-message', stash.message, stash.message);
          const stashItem = createItem(`stash:${stash.id}`, `${label}, ${stash.message}`, stashRow, false, false, stashKey);
          stashChildren.appendChild(stashItem.item);
        }
      }
    }
  }
  for (const id of pendingLoads.keys()) if (!owners.has(id)) pendingLoads.delete(id);
  for (const id of pendingPages.keys()) if (!owners.has(id)) pendingPages.delete(id);
  for (const id of transientWorktreeVisibility.keys()) if (!owners.has(id)) transientWorktreeVisibility.delete(id);
  syncSearch();
  if (!visible.some(item => item.key === selected)) {
    const alternate = previousSelection?.pageOwner && visible.find(item =>
      item.pageOwner === previousSelection.pageOwner && !item.pageDisabled);
    selected = alternate?.key || visible.find(item => item.key === previousSelection?.parent)?.key || visible[0]?.key;
  }
  for (const item of visible) {
    const active = item.key === selected;
    item.item.setAttribute('aria-selected', active ? 'true' : 'false');
    item.item.setAttribute('tabindex', active ? '0' : '-1');
  }
  if (hadFocus) {
    const action = focusedAction && visible.find(item => item.key === focusedActionKey)?.item.querySelector(`.${focusedAction}`);
    if (action) action.focus();
    else visible.find(item => item.key === selected)?.item.focus();
  }
  save();
}

function select(item, focus = true) {
  if (!item) return;
  const previous = visible.find(value => value.key === selected);
  if (previous) {
    previous.item.setAttribute('aria-selected', 'false');
    previous.item.setAttribute('tabindex', '-1');
  }
  selected = item.key;
  item.item.setAttribute('aria-selected', 'true');
  item.item.setAttribute('tabindex', '0');
  if (focus) item.item.focus();
  save();
}

function searchOwnerItem(item) {
  while (item && !item.owner) item = visible.find(candidate => candidate.key === item.parent);
  return item;
}

function parentRepository(item) {
  while (item && !item.key.startsWith('repo:')) item = visible.find(candidate => candidate.key === item.parent);
  return item;
}

function sendSearch(direction = 'first') {
  if (!searchOwner) return;
  if (new TextEncoder().encode(searchInput.value).length > 512) {
    searchStatus.textContent = 'Search is too long.';
    searchPrev.disabled = searchNext.disabled = true;
    return;
  }
  vscode.postMessage({ type: 'searchChanges', id: searchOwner, query: searchInput.value, direction });
}

function closeSearch() {
  if (searchTimer) clearTimeout(searchTimer);
  searchInput.value = '';
  sendSearch();
  searchOwner = undefined;
  searchBox.hidden = true;
  searchStatus.textContent = '';
  render();
}

function beginSearch() {
  const item = searchOwnerItem(visible.find(value => value.key === selected));
  if (!item?.owner?.changesPage || !item.expandable) return;
  if (item.key.startsWith('repo:') && !item.fileExpandable) return;
  if (searchOwner && searchOwner !== item.owner.id) closeSearch();
  searchOwner = item.owner.id;
  if (item.key.startsWith('repo:') ? !item.filesExpanded : !item.isExpanded) toggle(item);
  searchBox.hidden = false;
  searchInput.placeholder = `Search ${item.owner.name || item.owner.branch}`;
  searchInput.focus();
}

function syncSearch() {
  if (!searchOwner) return;
  const item = visible.find(value => value.owner?.id === searchOwner);
  if (!item) {
    searchOwner = undefined;
    searchBox.hidden = true;
    searchInput.value = '';
    searchStatus.textContent = '';
    return;
  }
  const result = item.owner.changesPage?.search;
  if (new TextEncoder().encode(searchInput.value).length > 512) {
    searchStatus.textContent = 'Search is too long.';
    searchPrev.disabled = searchNext.disabled = true;
    return;
  }
  const current = result?.query === searchInput.value;
  const ready = current && result.phase === 'ready';
  searchPrev.disabled = searchNext.disabled = !ready || !result.count;
  searchStatus.textContent = !searchInput.value ? '' : !current || result.phase === 'loading' ? 'Searching…' :
    result.phase === 'error' ? result.error : result.count ? `${result.position + 1} of ${result.count}` : 'No matches';
  if (!ready || !result.count) return;
  const match = visible.find(value => value.searchOwner === searchOwner && value.searchPath === result.path && value.searchGroup === result.group);
  if (match) selected = match.key;
}

startSearchButton.addEventListener('click', beginSearch);
searchInput.addEventListener('input', () => {
  if (searchTimer) clearTimeout(searchTimer);
  searchStatus.textContent = searchInput.value ? 'Searching…' : '';
  searchPrev.disabled = searchNext.disabled = true;
  if (!searchInput.value) sendSearch();
  else searchTimer = setTimeout(() => { searchTimer = undefined; sendSearch(); }, 100);
  render();
});
searchInput.addEventListener('keydown', event => {
  if (event.key === 'Escape') { closeSearch(); event.preventDefault(); }
  else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    if (!searchNext.disabled) sendSearch(event.key === 'ArrowDown' ? 'next' : 'previous');
    event.preventDefault();
  } else if (event.key === 'Enter') {
    const result = visible.find(item => item.owner?.id === searchOwner)?.owner.changesPage?.search;
    if (!searchNext.disabled && result?.query === searchInput.value) {
      activate(visible.find(item => item.key === selected && item.searchOwner === searchOwner &&
        item.searchPath === result.path && item.searchGroup === result.group));
    }
    event.preventDefault();
  }
});
searchPrev.addEventListener('click', () => sendSearch('previous'));
searchNext.addEventListener('click', () => sendSearch('next'));

function releaseChanges(item) {
  if (!item.owner?.changesPage) return;
  if (searchOwner === item.owner.id) closeSearch();
  pendingLoads.delete(item.owner.id);
  pendingPages.delete(item.owner.id);
  vscode.postMessage({ type: 'releaseChanges', id: item.owner.id });
}

function releaseDescendants(item) {
  const byKey = new Map(visible.map(value => [value.key, value]));
  for (const candidate of visible) {
    if (candidate.key === item.key) {
      releaseChanges(candidate);
      continue;
    }
    let parent = candidate.parent;
    while (parent && parent !== item.key) parent = byKey.get(parent)?.parent;
    if (parent === item.key && candidate.isExpanded) releaseChanges(candidate);
  }
}

function toggle(item) {
  if (!item.expandable) return;
  if (item.key.startsWith('repo:')) {
    if (item.filesExpanded) {
      expanded.delete(item.key);
      releaseChanges(item);
      render();
      return;
    }
    if (!item.fileExpandable && item.owner.worktrees?.length) { toggleWorktrees(item.owner); return; }
    if (!item.fileExpandable && item.owner.stashes?.length) {
      const stashKey = `stash-group:${item.owner.id}`;
      if (expandedStashes.has(stashKey)) expandedStashes.delete(stashKey);
      else expandedStashes.add(stashKey);
      render();
      return;
    }
    expanded.add(item.key);
    requestChanges(item.owner, true);
  } else if (item.key.startsWith('worktree:')) {
    if (item.isExpanded) {
      expanded.delete(item.key);
      releaseDescendants(item);
    }
    else {
      expanded.add(item.key);
      requestChanges(item.owner, true);
      for (const worktree of item.owner?.worktrees || []) {
        if (worktree.available && expanded.has(`worktree:${worktree.id}`)) requestChanges(worktree, true);
      }
    }
  } else if (item.key.startsWith('stash-group:')) {
    if (item.isExpanded) expandedStashes.delete(item.key);
    else expandedStashes.add(item.key);
  } else if (item.isExpanded) collapsedGroups.add(item.key);
  else collapsedGroups.delete(item.key);
  render();
}

function activate(item) {
  if (!item) return;
  if (item.expandable) toggle(item);
  else if (item.pageOwner && !item.pageDisabled) {
    if (item.pageDirection === 'retry') {
      const owner = visible.find(value => value.key === item.parent)?.owner;
      requestChanges(owner, true);
    } else requestPage(item.pageOwner, item.pageDirection, item.pageNumber);
    render();
  }
  else if (item.fileId !== undefined) vscode.postMessage({ type: 'openChange', id: item.fileId });
  else if (item.worktreeId !== undefined) vscode.postMessage({ type: 'menu', kind: 'worktree', id: item.worktreeId });
}

tree.addEventListener('focusin', event => {
  const key = event.target.closest('.treeitem')?.dataset.key;
  const item = visible.find(value => value.key === key);
  if (item && selected !== key) select(item, false);
});

tree.addEventListener('click', event => {
  const row = event.target.closest('.row');
  const key = row?.parentElement?.dataset.key;
  const item = visible.find(value => value.key === key);
  if (!item) return;
  select(item);
  activate(item);
});

tree.addEventListener('keydown', event => {
  if ((event.target.classList?.contains('graph-action') || event.target.classList?.contains('menu-action') ||
      event.target.classList?.contains('stash-action') || event.target.classList?.contains('worktree-action')) &&
      (event.key === 'Enter' || event.key === ' ')) return;
  const current = visible.find(item => item.key === selected);
  if (!current) return;
  const index = visible.indexOf(current);
  let handled = true;
  switch (event.key) {
    case '/': beginSearch(); break;
    case 'w': {
      const repo = parentRepository(current);
      if (!repo?.owner?.worktrees?.length) { handled = false; break; }
      let child = current;
      while (child && child.parent !== repo.key) child = visible.find(item => item.key === child.parent);
      if (repo.showWorktrees && child?.worktreeId) select(repo);
      toggleWorktrees(repo.owner);
      break;
    }
    case 'ArrowDown': select(visible[Math.min(index + 1, visible.length - 1)]); break;
    case 'ArrowUp': select(visible[Math.max(index - 1, 0)]); break;
    case 'Home': select(visible[0]); break;
    case 'End': select(visible[visible.length - 1]); break;
    case 'ArrowRight':
      if (current.key.startsWith('repo:') && current.fileExpandable && !current.filesExpanded) toggle(current);
      else if (current.expandable && !current.isExpanded) toggle(current);
      else if (visible[index + 1]?.parent === current.key) select(visible[index + 1]);
      break;
    case 'ArrowLeft':
      if (current.key.startsWith('repo:') && current.filesExpanded) toggle(current);
      else if (current.key.startsWith('repo:') && current.showWorktrees) toggleWorktrees(current.owner);
      else if (current.key.startsWith('repo:') && current.stashOpen) {
        expandedStashes.delete(`stash-group:${current.owner.id}`);
        render();
      }
      else if (current.expandable && current.isExpanded) toggle(current);
      else select(visible.find(item => item.key === current.parent));
      break;
    case 'Enter':
    case ' ': activate(current); break;
    default: handled = false;
  }
  if (handled) event.preventDefault();
});

window.addEventListener('message', event => {
  if (event.data?.type !== 'snapshot') return;
  if (event.data.themeClear) window.GitpaneTheme?.clear();
  if (event.data.theme && !window.GitpaneTheme?.set(event.data.theme)) {
    event.data.errors = [...(event.data.errors || []), 'Gitpane returned an invalid theme.'];
  }
  snapshot = { ...event.data, repositories: Array.isArray(event.data.repositories) ? event.data.repositories : [] };
  for (const reveal of Array.isArray(snapshot.revealWorktrees) ? snapshot.revealWorktrees.slice(0, 128) : []) {
    if (!reveal || typeof reveal.id !== 'string' || reveal.id.length > 128 ||
        typeof reveal.identity !== 'string' || !/^[a-f0-9]{64}$/.test(reveal.identity)) continue;
    const repo = snapshot.repositories.find(item => item.worktreeStateKey === reveal.identity);
    if (!repo) continue;
    if (!appliedWorktreeReveals.has(reveal.id)) {
      setWorktreesVisible(repo, true);
      appliedWorktreeReveals.add(reveal.id);
      if (appliedWorktreeReveals.size > 128) appliedWorktreeReveals.delete(appliedWorktreeReveals.values().next().value);
    }
    save();
    vscode.postMessage({ type: 'ackWorktreeReveal', id: reveal.id });
  }
  render();
});

render();
vscode.postMessage({ type: 'ready' });
