'use strict';

const vscode = acquireVsCodeApi();
const tree = document.getElementById('repositories');
const messages = document.getElementById('messages');
const saved = vscode.getState() || {};
const expanded = new Set(Array.isArray(saved.expanded) ? saved.expanded : []);
const collapsedGroups = new Set(Array.isArray(saved.collapsedGroups) ? saved.collapsedGroups : []);
let selected = typeof saved.selected === 'string' ? saved.selected : undefined;
let snapshot = { repositories: [], errors: [], loading: true };
let visible = [];

function save() {
  vscode.setState({ expanded: [...expanded], collapsedGroups: [...collapsedGroups], selected });
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

function fileCount(repo) {
  const paths = new Set();
  for (const group of repo.groups || []) {
    for (const file of group.files || []) paths.add(file.path);
  }
  return paths.size;
}

function labelForRepo(repo, count) {
  const details = [repo.name, repo.worktree ? 'worktree' : '', repo.branch || 'unknown branch',
    repo.statusUnavailable ? 'status unavailable' : repo.dirty ? 'has changes' : 'clean',
    `${count} ${count === 1 ? 'file' : 'files'}`];
  if (repo.ahead > 0) details.push(`${repo.ahead} ahead`);
  if (repo.behind > 0) details.push(`${repo.behind} behind`);
  return details.filter(Boolean).join(', ');
}

function repoRow(repo, count, isExpanded) {
  const row = element('div', 'row repo-row');
  add(row, 'dirty', repo.dirty ? '*' : '\u00a0');
  const name = add(row, 'name', '', repo.name);
  if (repo.worktree) add(name, 'worktree', '⎇ ');
  name.appendChild(document.createTextNode(repo.name));
  const branch = repo.branch || '—';
  add(row, `branch${branch === 'main' || branch === 'master' ? ' default' : ''}`, branch, branch);
  const tail = element('span', 'tail');
  if (repo.ahead > 0) add(tail, 'ahead', `↑${repo.ahead}`);
  if (repo.behind > 0) add(tail, 'behind', `↓${repo.behind}`);
  if (repo.statusUnavailable) add(tail, 'behind', '!', 'Status unavailable');
  else if (count) add(tail, 'count', `${isExpanded ? '▾' : '▸'}[${count}]`);
  row.appendChild(tail);
  return row;
}

function statusClass(status) {
  return ({ M: 'modified', A: 'added', D: 'deleted', R: 'renamed', '?': 'untracked', C: 'conflicted' })[status] || 'muted';
}

function fileRow(file) {
  const row = element('div', 'row file-row');
  add(row, statusClass(file.status), file.status || ' ');
  add(row, 'file-path', file.path || file.name, file.path || file.name);
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
  visible = [];
  tree.replaceChildren();
  renderMessages();
  for (const repo of snapshot.repositories) {
    const key = `repo:${repo.id}`;
    const count = fileCount(repo);
    const groups = Array.isArray(repo.groups) ? repo.groups : [];
    const isExpanded = expanded.has(key);
    const item = createItem(key, labelForRepo(repo, count), repoRow(repo, count, isExpanded), groups.length > 0, isExpanded, undefined);
    tree.appendChild(item.item);
    if (!isExpanded || !groups.length) continue;
    const children = element('div', 'children');
    children.setAttribute('role', 'group');
    item.item.appendChild(children);
    for (const group of groups) {
      const groupKey = `group:${JSON.stringify([repo.id, group.id])}`;
      const files = Array.isArray(group.files) ? group.files : [];
      const groupExpanded = !collapsedGroups.has(groupKey);
      const row = element('div', 'row group-row');
      add(row, 'subtree', groupExpanded ? '▾' : '▸');
      add(row, '', `${group.label} [${files.length}]`);
      const groupItem = createItem(groupKey, `${group.label}, ${files.length} ${files.length === 1 ? 'file' : 'files'}`,
        row, files.length > 0, groupExpanded, key);
      children.appendChild(groupItem.item);
      if (!groupExpanded || !files.length) continue;
      const filesGroup = element('div', 'children');
      filesGroup.setAttribute('role', 'group');
      groupItem.item.appendChild(filesGroup);
      for (const file of files) {
        const fileKey = `file:${file.id}`;
        const fileItem = createItem(fileKey, `${file.status || 'changed'} ${file.path || file.name}`,
          fileRow(file), false, false, groupKey);
        fileItem.fileId = file.id;
        filesGroup.appendChild(fileItem.item);
      }
    }
  }
  if (!visible.some(item => item.key === selected)) selected = visible[0]?.key;
  for (const item of visible) {
    const active = item.key === selected;
    item.item.setAttribute('aria-selected', active ? 'true' : 'false');
    item.item.setAttribute('tabindex', active ? '0' : '-1');
  }
  if (hadFocus) visible.find(item => item.key === selected)?.item.focus();
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

function toggle(item) {
  if (!item.expandable) return;
  if (item.key.startsWith('repo:')) {
    if (item.isExpanded) expanded.delete(item.key);
    else expanded.add(item.key);
  } else if (item.isExpanded) collapsedGroups.add(item.key);
  else collapsedGroups.delete(item.key);
  render();
}

function activate(item) {
  if (!item) return;
  if (item.expandable) toggle(item);
  else if (item.fileId !== undefined) vscode.postMessage({ type: 'openChange', id: item.fileId });
}

tree.addEventListener('click', event => {
  const row = event.target.closest('.row');
  const key = row?.parentElement?.dataset.key;
  const item = visible.find(value => value.key === key);
  if (!item) return;
  select(item);
  activate(item);
});

tree.addEventListener('keydown', event => {
  const current = visible.find(item => item.key === selected);
  if (!current) return;
  const index = visible.indexOf(current);
  let handled = true;
  switch (event.key) {
    case 'ArrowDown': select(visible[Math.min(index + 1, visible.length - 1)]); break;
    case 'ArrowUp': select(visible[Math.max(index - 1, 0)]); break;
    case 'Home': select(visible[0]); break;
    case 'End': select(visible[visible.length - 1]); break;
    case 'ArrowRight':
      if (current.expandable && !current.isExpanded) toggle(current);
      else if (visible[index + 1]?.parent === current.key) select(visible[index + 1]);
      break;
    case 'ArrowLeft':
      if (current.expandable && current.isExpanded) toggle(current);
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
  snapshot = { ...event.data, repositories: Array.isArray(event.data.repositories) ? event.data.repositories : [] };
  render();
});

render();
vscode.postMessage({ type: 'ready' });
